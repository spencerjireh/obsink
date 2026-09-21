use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use futures_util::{stream, StreamExt};
use thiserror::Error;

use serde::{Deserialize, Serialize};

use crate::{
    api_client::{ApiClient, ApiError, ManifestFetch},
    crypto::{decrypt, derive_keys, encrypt, CryptoError, CryptoKeys, KeyBytes},
    fs_util::write_atomic,
    hash_cache::HashCache,
    hasher::{build_manifest_with_cache, hash_file, HasherError},
    ignore::IgnoreRules,
    manifest::{checkpoint_manifest, diff_manifests, ManifestDiff},
    progress::{ProgressEvent, ProgressSink, SyncPhase},
    types::{
        BatchOp, Conflict, ConflictResolution, ConflictResolutionChoice, FileEntry, Manifest,
        SyncAction, SyncActionKind, SyncFailure, SyncPlan, SyncResult, VaultConfig,
    },
};

/// The checkpoint of the last completed sync: the server manifest as re-fetched
/// after that sync, minus held-back paths. It is the *base* of the three-way
/// diff and the source of local-deletion detection.
const MANIFEST_FILE: &str = ".obsink/manifest.json";
/// Last server manifest seen plus its ETag, so an unchanged manifest costs a
/// 304 instead of a full download. Distinct from `MANIFEST_FILE`, which is the
/// checkpoint of the last *completed* sync.
const REMOTE_MANIFEST_CACHE_FILE: &str = ".obsink/remote-manifest.json";
/// Ciphertext per batch upload. Well under the server's 128 MiB body cap and
/// the memory a phone can spare for one request.
const BATCH_BYTE_BUDGET: u64 = 32 * 1024 * 1024;
/// Operations per batch upload.
const BATCH_MAX_OPS: usize = 64;
/// Downloads in flight at once.
const DOWNLOAD_CONCURRENCY: usize = 8;

#[derive(Debug, Error)]
pub enum SyncEngineError {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("api error: {0}")]
    Api(#[from] ApiError),
    #[error("hashing error: {0}")]
    Hasher(#[from] HasherError),
    #[error("crypto error: {0}")]
    Crypto(#[from] CryptoError),
    #[error("missing resolution for conflict at {0}")]
    MissingResolution(String),
    #[error("blocking task failed: {0}")]
    Blocking(#[from] tokio::task::JoinError),
}

/// Run filesystem or CPU-bound work on tokio's blocking pool. Every
/// `std::fs` touch in this module goes through here so an `fsync` or a full
/// vault walk never parks a reactor thread that in-flight HTTP futures need
/// (the timeouts in `api_client` would otherwise measure scheduler latency).
/// The closure owns its inputs; `std::fs` stays inside it rather than
/// switching to `tokio::fs`, which is a `spawn_blocking` per call.
async fn blocking<T, F>(f: F) -> Result<T, SyncEngineError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, SyncEngineError> + Send + 'static,
{
    tokio::task::spawn_blocking(f).await?
}

pub fn load_manifest_from_disk(path: &Path) -> Result<Manifest, SyncEngineError> {
    if !path.exists() {
        return Ok(Manifest::new());
    }

    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn save_manifest_to_disk(path: &Path, manifest: &Manifest) -> Result<(), SyncEngineError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let bytes = serde_json::to_vec_pretty(manifest)?;
    write_atomic(path, &bytes)?;
    Ok(())
}

pub fn sync_manifest_path(local_root: &Path) -> PathBuf {
    local_root.join(MANIFEST_FILE)
}

pub fn remote_manifest_cache_path(local_root: &Path) -> PathBuf {
    local_root.join(REMOTE_MANIFEST_CACHE_FILE)
}

#[derive(Debug, Serialize, Deserialize)]
struct RemoteManifestCache {
    etag: Option<String>,
    manifest: Manifest,
}

fn load_remote_cache(local_root: &Path) -> Option<RemoteManifestCache> {
    let bytes = fs::read(remote_manifest_cache_path(local_root)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn save_remote_cache(
    local_root: &Path,
    cache: &RemoteManifestCache,
) -> Result<(), SyncEngineError> {
    let path = remote_manifest_cache_path(local_root);
    write_atomic(&path, &serde_json::to_vec(cache)?)?;
    Ok(())
}

/// Fetch the server manifest through the on-disk ETag cache: send the cached
/// ETag, reuse the cached manifest on 304, and refresh the cache on 200. A
/// missing or corrupt cache falls back to an unconditional fetch.
pub async fn fetch_remote_manifest(
    client: &ApiClient,
    local_root: &Path,
    keys: &CryptoKeys,
) -> Result<Manifest, SyncEngineError> {
    let cached = {
        let root = local_root.to_path_buf();
        blocking(move || Ok(load_remote_cache(&root))).await?
    }
    .filter(|cache| cache.etag.is_some());
    let etag = cached.as_ref().and_then(|cache| cache.etag.as_deref());
    match client.get_manifest_if_changed(keys, etag).await? {
        ManifestFetch::NotModified => match cached {
            Some(cache) => Ok(cache.manifest),
            // Defensive: a 304 without a cache means the validator came from
            // nowhere; fetch unconditionally.
            None => match client.get_manifest_if_changed(keys, None).await? {
                ManifestFetch::Modified { manifest, .. } => Ok(manifest),
                ManifestFetch::NotModified => Ok(Manifest::new()),
            },
        },
        ManifestFetch::Modified { manifest, etag } => {
            if etag.is_some() {
                let root = local_root.to_path_buf();
                let cache = RemoteManifestCache {
                    etag,
                    manifest: manifest.clone(),
                };
                if let Err(error) = blocking(move || save_remote_cache(&root, &cache)).await {
                    tracing::warn!(%error, "could not write the remote manifest cache");
                }
            }
            Ok(manifest)
        }
    }
}

/// Three-way diff of the working manifest against the remote, using the last
/// checkpoint as the base, with ignored paths removed from all three first.
/// Filtering the base too matters: the walker skips an ignored path, so a
/// base entry for it would look like a local deletion and push a
/// `DeleteRemote`. Filtered everywhere, a path that was synced before it was
/// ignored simply becomes invisible; its server copy stays. See
/// [`diff_manifests`].
pub fn diff_local_and_remote(
    base: &Manifest,
    local: &Manifest,
    remote: &Manifest,
    ignore: &IgnoreRules,
) -> ManifestDiff {
    let keep = |manifest: &Manifest| -> Manifest {
        manifest
            .iter()
            .filter(|(path, _)| !ignore.is_ignored(path))
            .map(|(path, entry)| (path.clone(), entry.clone()))
            .collect()
    };
    diff_manifests(&keep(base), &keep(local), &keep(remote))
}

/// Whether the server manifest differs from the cached one: sends the
/// cached ETag and answers `false` on 304. A 200 refreshes the cache so the
/// `prepare_sync` that follows is served from it. No cache means "changed".
pub async fn remote_changed(
    client: &ApiClient,
    local_root: &Path,
    keys: &CryptoKeys,
) -> Result<bool, SyncEngineError> {
    let cached = {
        let root = local_root.to_path_buf();
        blocking(move || Ok(load_remote_cache(&root))).await?
    }
    .filter(|cache| cache.etag.is_some());
    let Some(cache) = cached else {
        return Ok(true);
    };
    match client
        .get_manifest_if_changed(keys, cache.etag.as_deref())
        .await?
    {
        ManifestFetch::NotModified => Ok(false),
        ManifestFetch::Modified { manifest, etag } => {
            if etag.is_some() {
                let root = local_root.to_path_buf();
                let cache = RemoteManifestCache { etag, manifest };
                if let Err(error) = blocking(move || save_remote_cache(&root, &cache)).await {
                    tracing::warn!(%error, "could not write the remote manifest cache");
                }
            }
            Ok(true)
        }
    }
}

/// The two local inputs of a sync: the last checkpoint (`base`) and the
/// current on-disk state with local deletions synthesized (`working`).
#[derive(Debug, Clone, Default)]
pub struct LocalState {
    pub base: Manifest,
    pub working: Manifest,
}

pub fn load_local_state(
    local_root: &Path,
    keys: &CryptoKeys,
    ignore: &IgnoreRules,
) -> Result<LocalState, SyncEngineError> {
    let base = load_manifest_from_disk(&sync_manifest_path(local_root))?;
    // The hash memo makes a repeat walk a stat per file; a failed save only
    // costs the next walk its speed.
    let mut cache = HashCache::load(local_root, keys);
    let working = build_working_manifest(local_root, &base, keys, &mut cache, ignore)?;
    if let Err(error) = cache.save(local_root) {
        tracing::warn!(%error, "could not write the hash cache");
    }
    Ok(LocalState { base, working })
}

pub async fn prepare_sync(
    config: &VaultConfig,
    key: &KeyBytes,
    progress: &dyn ProgressSink,
) -> Result<SyncPlan, SyncEngineError> {
    let keys = derive_keys(key);
    let client = ApiClient::new(config.clone());
    let local_root = Path::new(&config.local_path);
    let ignore = config.ignore_rules();
    let local_state = {
        let root = local_root.to_path_buf();
        let keys = keys.clone();
        let ignore = ignore.clone();
        blocking(move || load_local_state(&root, &keys, &ignore)).await?
    };
    let remote_manifest = fetch_remote_manifest(&client, local_root, &keys).await?;
    let diff = diff_local_and_remote(
        &local_state.base,
        &local_state.working,
        &remote_manifest,
        &ignore,
    );

    progress.report(ProgressEvent::Phase(SyncPhase::Downloading));
    let download_failures =
        apply_downloads(local_root, &keys, &client, &diff.download, progress).await;

    tracing::info!(
        vault = %config.vault_id,
        uploads = diff.upload.len(),
        downloads = diff.download.len(),
        conflicts = diff.conflicts.len(),
        download_failures = download_failures.len(),
        "prepared sync plan"
    );
    Ok(SyncPlan {
        upload: diff.upload,
        download: diff.download,
        conflicts: diff.conflicts,
        failures: download_failures,
    })
}

pub async fn complete_sync(
    config: &VaultConfig,
    key: &KeyBytes,
    plan: &SyncPlan,
    resolutions: &[ConflictResolution],
    progress: &dyn ProgressSink,
) -> Result<SyncResult, SyncEngineError> {
    let keys = derive_keys(key);
    let client = ApiClient::new(config.clone());
    let local_root = Path::new(&config.local_path);

    let resolved =
        apply_resolutions(local_root, &keys, &client, plan, resolutions, progress).await?;

    // Paths whose checkpoint entry must not advance: a failed transfer keeps
    // its old base so the next diff retries it, and a late or deferred
    // conflict keeps it so the next diff still sees "both changed" instead
    // of clobbering.
    let mut hold_back: BTreeSet<String> = plan
        .failures
        .iter()
        .map(|failure| failure.path.clone())
        .chain(
            resolved
                .deferred
                .iter()
                .map(|conflict| conflict.path.clone()),
        )
        .collect();

    progress.report(ProgressEvent::Phase(SyncPhase::Uploading));
    let uploads = run_uploads(
        local_root,
        &keys,
        &client,
        &resolved.uploads,
        &mut hold_back,
        progress,
    )
    .await;

    // Carry download-side failures from prepare into the final result.
    let mut failures = plan.failures.clone();
    failures.extend(uploads.failures);
    let mut conflicts = resolved.deferred;
    conflicts.extend(uploads.late_conflicts);

    let downloaded = plan
        .download
        .iter()
        .filter(|action| matches!(action.kind, SyncActionKind::Download))
        .count()
        .saturating_sub(
            failures
                .iter()
                .filter(|failure| matches!(failure.kind, SyncActionKind::Download))
                .count(),
        );
    progress.report(ProgressEvent::Done {
        uploaded: uploads.succeeded,
        downloaded,
        failed: failures.len(),
    });

    // Skip the checkpoint when a fatal error already proved the network is
    // gone; otherwise a checkpoint failure is reported on its own channel
    // rather than as a file failure.
    let checkpoint_error = if failures.iter().any(|failure| failure.fatal) {
        None
    } else {
        checkpoint(local_root, &keys, &client, &hold_back)
            .await
            .err()
            .map(|error| error.to_string())
    };

    Ok(SyncResult {
        upload: resolved.uploads,
        download: plan.download.clone(),
        conflicts,
        failures,
        checkpoint_error,
    })
}

/// What resolving a plan's conflicts leaves to upload, and which conflicts
/// the caller deferred.
struct Resolved {
    uploads: Vec<SyncAction>,
    deferred: Vec<Conflict>,
}

/// Apply the caller's choice to every conflict in the plan: `KeepLocal`
/// queues an upload, `KeepRemote` writes the server version, `KeepBoth`
/// writes a `.conflict` copy and queues both, `Defer` leaves the path
/// alone. A conflict without a choice is an error.
async fn apply_resolutions(
    local_root: &Path,
    keys: &CryptoKeys,
    client: &ApiClient,
    plan: &SyncPlan,
    resolutions: &[ConflictResolution],
    progress: &dyn ProgressSink,
) -> Result<Resolved, SyncEngineError> {
    let resolution_map = resolutions
        .iter()
        .map(|resolution| (resolution.path.clone(), resolution.choice.clone()))
        .collect::<BTreeMap<_, _>>();

    let mut uploads = plan.upload.clone();
    let mut deferred = Vec::new();

    if !plan.conflicts.is_empty() {
        progress.report(ProgressEvent::Phase(SyncPhase::ResolvingConflicts));
    }

    for conflict in &plan.conflicts {
        let choice = resolution_map
            .get(&conflict.path)
            .ok_or_else(|| SyncEngineError::MissingResolution(conflict.path.clone()))?;

        match effective_choice(choice, conflict) {
            ConflictResolutionChoice::Defer => {
                deferred.push(conflict.clone());
            }
            ConflictResolutionChoice::KeepLocal => {
                uploads.push(conflict_to_upload(conflict));
            }
            ConflictResolutionChoice::KeepRemote => {
                progress.report(ProgressEvent::FileStarted {
                    path: conflict.path.clone(),
                    kind: SyncActionKind::Download,
                    index: 0,
                    total: 1,
                });
                let bytes = apply_keep_remote(local_root, keys, client, conflict).await?;
                progress.report(ProgressEvent::FileCompleted {
                    path: conflict.path.clone(),
                    bytes,
                });
            }
            ConflictResolutionChoice::KeepBoth => {
                progress.report(ProgressEvent::FileStarted {
                    path: conflict.path.clone(),
                    kind: SyncActionKind::Download,
                    index: 0,
                    total: 1,
                });
                let (duplicate_path, bytes) =
                    write_conflict_copy(local_root, keys, client, conflict).await?;
                progress.report(ProgressEvent::FileCompleted {
                    path: conflict.path.clone(),
                    bytes,
                });
                uploads.push(conflict_to_upload(conflict));
                uploads.push({
                    let root = local_root.to_path_buf();
                    let keys = keys.clone();
                    blocking(move || build_upload_action_for_path(&root, &duplicate_path, &keys))
                        .await?
                });
            }
        }
    }

    Ok(Resolved { uploads, deferred })
}

/// What the upload phase produced. Paths that failed or hit a late 409 are
/// also added to the caller's `hold_back`.
struct UploadOutcome {
    succeeded: usize,
    failures: Vec<SyncFailure>,
    late_conflicts: Vec<Conflict>,
}

/// Upload in batches (`POST /vaults/:id/batch`): one round trip per
/// `BATCH_MAX_OPS` files or `BATCH_BYTE_BUDGET` bytes instead of one per
/// file. Each batch is read, encrypted and sent before the next is prepared,
/// so memory holds one batch of ciphertext at a time. A fatal error (whole
/// request, or a 5xx inside the answer) stops the remaining batches.
async fn run_uploads(
    local_root: &Path,
    keys: &CryptoKeys,
    client: &ApiClient,
    uploads: &[SyncAction],
    hold_back: &mut BTreeSet<String>,
    progress: &dyn ProgressSink,
) -> UploadOutcome {
    let total = uploads.len();
    let mut outcome = UploadOutcome {
        succeeded: 0,
        failures: Vec::new(),
        late_conflicts: Vec::new(),
    };
    let fail = |outcome: &mut UploadOutcome,
                hold_back: &mut BTreeSet<String>,
                action: &SyncAction,
                message: String,
                fatal: bool| {
        progress.report(ProgressEvent::FileFailed {
            path: action.path.clone(),
            error: message.clone(),
        });
        hold_back.insert(action.path.clone());
        outcome.failures.push(SyncFailure {
            path: action.path.clone(),
            kind: action.kind.clone(),
            error: message,
            fatal,
        });
    };

    let sizes: Vec<u64> = uploads
        .iter()
        .map(|action| action.local.as_ref().map(|entry| entry.size).unwrap_or(0))
        .collect();
    'batches: for range in chunk_uploads(&sizes) {
        let mut ops = Vec::with_capacity(range.len());
        let mut members = Vec::with_capacity(range.len());
        for index in range {
            let action = &uploads[index];
            progress.report(ProgressEvent::FileStarted {
                path: action.path.clone(),
                kind: action.kind.clone(),
                index,
                total,
            });
            match prepare_batch_op(local_root, keys, action).await {
                Ok(Some(op)) => {
                    ops.push(op);
                    members.push(index);
                }
                // Nothing to send for this kind; count it like the old
                // per-file loop did.
                Ok(None) => {
                    outcome.succeeded += 1;
                    progress.report(ProgressEvent::FileCompleted {
                        path: action.path.clone(),
                        bytes: 0,
                    });
                }
                Err(error) => fail(&mut outcome, hold_back, action, error.to_string(), false),
            }
        }
        if ops.is_empty() {
            continue;
        }

        let results = match client.batch(&ops, keys).await {
            Ok(results) => results,
            Err(error) => {
                // The whole request failed: every member fails alike, and a
                // systemic error stops the remaining batches as it would have
                // stopped the per-file loop.
                let fatal = is_fatal_api_error(&error);
                let message = error.to_string();
                for &index in &members {
                    fail(
                        &mut outcome,
                        hold_back,
                        &uploads[index],
                        message.clone(),
                        fatal,
                    );
                }
                if fatal {
                    break 'batches;
                }
                continue;
            }
        };

        let mut stop = false;
        for (&index, result) in members.iter().zip(results) {
            let action = &uploads[index];
            match result.status {
                200..=299 => {
                    outcome.succeeded += 1;
                    let bytes = action.local.as_ref().map(|entry| entry.size).unwrap_or(0);
                    progress.report(ProgressEvent::FileCompleted {
                        path: action.path.clone(),
                        bytes,
                    });
                }
                409 => {
                    // A 409 without a `current` entry means the server row is
                    // gone from under us; surface it as a conflict against a
                    // tombstone rather than dropping the upload on the floor.
                    let remote = result
                        .conflict
                        .and_then(|conflict| conflict.current)
                        .unwrap_or_else(|| FileEntry {
                            deleted: true,
                            ..FileEntry::default()
                        });
                    let local = action.local.clone().unwrap_or_default();
                    hold_back.insert(action.path.clone());
                    outcome.late_conflicts.push(Conflict {
                        path: action.path.clone(),
                        local,
                        remote,
                    });
                }
                status => {
                    let fatal = is_fatal_status(status);
                    fail(
                        &mut outcome,
                        hold_back,
                        action,
                        format!("api error: batch operation answered {status}"),
                        fatal,
                    );
                    stop |= fatal;
                }
            }
        }
        if stop {
            break;
        }
    }

    outcome
}

/// Checkpoint the server manifest so local state advances past every file
/// that did transfer (the resume point); held-back paths keep their old base
/// entry. Runs even with late conflicts pending, so the paths that did land
/// do not turn into false conflicts if a third device edits them next.
async fn checkpoint(
    local_root: &Path,
    keys: &CryptoKeys,
    client: &ApiClient,
    hold_back: &BTreeSet<String>,
) -> Result<(), SyncEngineError> {
    let remote_manifest = fetch_remote_manifest(client, local_root, keys).await?;
    let manifest_path = sync_manifest_path(local_root);
    let hold_back = hold_back.clone();
    blocking(move || {
        let previous_base = load_manifest_from_disk(&manifest_path)?;
        let next = checkpoint_manifest(&previous_base, &remote_manifest, &hold_back);
        save_manifest_to_disk(&manifest_path, &next)
    })
    .await
}

/// `KeepBoth` needs two live versions. When one side is a deletion there is
/// nothing to copy, so it collapses to keeping the side that still exists.
fn effective_choice(
    choice: &ConflictResolutionChoice,
    conflict: &Conflict,
) -> ConflictResolutionChoice {
    match choice {
        ConflictResolutionChoice::KeepBoth if conflict.remote.deleted => {
            ConflictResolutionChoice::KeepLocal
        }
        ConflictResolutionChoice::KeepBoth if conflict.local.deleted => {
            ConflictResolutionChoice::KeepRemote
        }
        other => other.clone(),
    }
}

/// The on-disk manifest plus a tombstone for every base entry that is no
/// longer on disk. Tombstones keep the base hash (the parent hash for the
/// remote delete); their `modified` is informational only.
fn build_working_manifest(
    local_root: &Path,
    previous_manifest: &Manifest,
    keys: &CryptoKeys,
    cache: &mut HashCache,
    ignore: &IgnoreRules,
) -> Result<Manifest, SyncEngineError> {
    let mut current = build_manifest_with_cache(local_root, keys, cache, ignore)?;
    let seen_paths = current.keys().cloned().collect::<BTreeSet<_>>();

    for (path, previous_entry) in previous_manifest {
        if seen_paths.contains(path) || previous_entry.deleted {
            continue;
        }

        current.insert(
            path.clone(),
            FileEntry {
                hash: previous_entry.hash.clone(),
                modified: now_seconds(),
                size: previous_entry.size,
                deleted: true,
                enc_path: previous_entry.enc_path.clone(),
            },
        );
    }

    Ok(current)
}

async fn apply_downloads(
    local_root: &Path,
    keys: &CryptoKeys,
    client: &ApiClient,
    downloads: &[SyncAction],
    progress: &dyn ProgressSink,
) -> Vec<SyncFailure> {
    let mut failures = Vec::new();
    let total = downloads.len();
    let (deletes, fetches): (Vec<&SyncAction>, Vec<&SyncAction>) =
        ordered_for_apply(downloads).partition(|action| action.kind == SyncActionKind::DeleteLocal);

    // Deletes first, one by one (see `ordered_for_apply`).
    for action in &deletes {
        if let Err(error) = delete_local_file(local_root, &action.path).await {
            failures.push(SyncFailure {
                path: action.path.clone(),
                kind: SyncActionKind::DeleteLocal,
                error: error.to_string(),
                fatal: false,
            });
        }
    }

    // Then the fetches, `DOWNLOAD_CONCURRENCY` at a time. A fatal error
    // raises `stop` so no further fetch starts; the ones never started stay
    // unrecorded, as the sequential loop's `break` left them. The futures are
    // built up front (not in a `map` closure) so the combined future stays
    // `Send` for callers that require it.
    let stop = AtomicBool::new(false);
    let offset = deletes.len();
    let fetch_futures: Vec<_> = fetches
        .iter()
        .enumerate()
        .map(|(position, action)| {
            fetch_one(
                local_root,
                keys,
                client,
                action,
                offset + position,
                total,
                progress,
                &stop,
            )
        })
        .collect();
    let outcomes: Vec<Option<SyncFailure>> = stream::iter(fetch_futures)
        .buffer_unordered(DOWNLOAD_CONCURRENCY)
        .collect()
        .await;
    failures.extend(outcomes.into_iter().flatten());

    failures
}

/// One download: fetch, decrypt, write. `None` on success or when `stop`
/// was already raised; the failure otherwise, raising `stop` if it is fatal.
#[allow(clippy::too_many_arguments)]
async fn fetch_one(
    local_root: &Path,
    keys: &CryptoKeys,
    client: &ApiClient,
    action: &SyncAction,
    index: usize,
    total: usize,
    progress: &dyn ProgressSink,
    stop: &AtomicBool,
) -> Option<SyncFailure> {
    if action.kind != SyncActionKind::Download || stop.load(Ordering::SeqCst) {
        return None;
    }
    progress.report(ProgressEvent::FileStarted {
        path: action.path.clone(),
        kind: SyncActionKind::Download,
        index,
        total,
    });
    let outcome = async {
        let blob = client.get_file(&action.path, keys).await?;
        decrypt_and_write(local_root, &action.path, keys, blob).await
    }
    .await;
    match outcome {
        Ok(len) => {
            progress.report(ProgressEvent::FileCompleted {
                path: action.path.clone(),
                bytes: len as u64,
            });
            None
        }
        Err(error) => {
            let fatal = is_fatal_sync_error(&error);
            if fatal {
                stop.store(true, Ordering::SeqCst);
            }
            let message = error.to_string();
            progress.report(ProgressEvent::FileFailed {
                path: action.path.clone(),
                error: message.clone(),
            });
            Some(SyncFailure {
                path: action.path.clone(),
                kind: SyncActionKind::Download,
                error: message,
                fatal,
            })
        }
    }
}

/// Local deletes run before downloads. On a case-insensitive volume (APFS
/// default) `Note.md` and `note.md` are one directory entry, so deleting the
/// tombstoned spelling after writing the new one would remove the download.
fn ordered_for_apply(downloads: &[SyncAction]) -> impl Iterator<Item = &SyncAction> {
    let deletes = downloads
        .iter()
        .filter(|action| action.kind == SyncActionKind::DeleteLocal);
    let fetches = downloads
        .iter()
        .filter(|action| action.kind != SyncActionKind::DeleteLocal);
    deletes.chain(fetches)
}

/// A fatal error is systemic (network down, auth failure, server error): it
/// will likely strike every remaining file too, so the batch stops. Per-file
/// errors (a too-large 413, a missing 404, a local crypto/path failure) leave
/// the rest of the batch viable, so the sync continues.
pub(crate) fn is_fatal_sync_error(error: &SyncEngineError) -> bool {
    match error {
        SyncEngineError::Api(api_error) => is_fatal_api_error(api_error),
        _ => false,
    }
}

fn is_fatal_api_error(error: &ApiError) -> bool {
    match error {
        // Not reaching the server is systemic. A response that did arrive
        // but could not be decoded (bad JSON, a body cut mid-stream, a
        // builder slip) belongs to that one request, so the rest of the
        // batch is still worth trying.
        ApiError::Http(error) => error.is_timeout() || error.is_connect() || error.is_request(),
        ApiError::Unauthorized => true,
        ApiError::UnexpectedStatus { status, .. } => is_fatal_status(status.as_u16()),
        ApiError::Crypto(_) | ApiError::Conflict { .. } => false,
    }
}

/// Auth failures and server errors are systemic; 4xx answers such as 413 or
/// 404 belong to one file.
fn is_fatal_status(status: u16) -> bool {
    matches!(status, 401 | 403 | 500..=599)
}

/// Split uploads into consecutive batches: a batch closes when adding the
/// next file would exceed `BATCH_BYTE_BUDGET` or `BATCH_MAX_OPS`. A file
/// larger than the budget travels alone. Sizes are the plaintext sizes from
/// the manifest (ciphertext adds a constant few bytes), so no file is read
/// before its batch is due.
fn chunk_uploads(sizes: &[u64]) -> Vec<std::ops::Range<usize>> {
    let mut batches = Vec::new();
    let mut start = 0usize;
    let mut bytes = 0u64;
    for (index, &size) in sizes.iter().enumerate() {
        let full =
            index > start && (bytes + size > BATCH_BYTE_BUDGET || index - start >= BATCH_MAX_OPS);
        if full {
            batches.push(start..index);
            start = index;
            bytes = 0;
        }
        bytes += size;
    }
    if start < sizes.len() {
        batches.push(start..sizes.len());
    }
    batches
}

async fn apply_keep_remote(
    local_root: &Path,
    keys: &CryptoKeys,
    client: &ApiClient,
    conflict: &Conflict,
) -> Result<u64, SyncEngineError> {
    if conflict.remote.deleted {
        delete_local_file(local_root, &conflict.path).await?;
        Ok(0)
    } else {
        let blob = client.get_file(&conflict.path, keys).await?;
        let len = decrypt_and_write(local_root, &conflict.path, keys, blob).await?;
        Ok(len as u64)
    }
}

async fn write_conflict_copy(
    local_root: &Path,
    keys: &CryptoKeys,
    client: &ApiClient,
    conflict: &Conflict,
) -> Result<(String, u64), SyncEngineError> {
    debug_assert!(
        !conflict.remote.deleted,
        "effective_choice collapses KeepBoth on a remote tombstone"
    );

    let duplicate_path = conflict_copy_path(&conflict.path);
    let blob = client.get_file(&conflict.path, keys).await?;
    let len = decrypt_and_write(local_root, &duplicate_path, keys, blob).await?;
    Ok((duplicate_path, len as u64))
}

/// Read and encrypt one upload into its batch operation, off the async
/// runtime. `None` for kinds the batch endpoint does not carry.
async fn prepare_batch_op(
    local_root: &Path,
    keys: &CryptoKeys,
    action: &SyncAction,
) -> Result<Option<BatchOp>, SyncEngineError> {
    match action.kind {
        SyncActionKind::Upload => {
            let content = {
                let path = local_root.join(&action.path);
                let content_enc = keys.content_enc;
                blocking(move || {
                    let plaintext = fs::read(path)?;
                    Ok(encrypt(&content_enc, &plaintext)?)
                })
                .await?
            };
            Ok(Some(BatchOp::Put {
                path: action.path.clone(),
                parent_hash: parent_hash(action).map(str::to_string),
                content_hash: action
                    .local
                    .as_ref()
                    .map(|entry| entry.hash.clone())
                    .unwrap_or_default(),
                content,
            }))
        }
        SyncActionKind::DeleteRemote => Ok(Some(BatchOp::Delete {
            path: action.path.clone(),
            parent_hash: parent_hash(action).map(str::to_string),
        })),
        _ => Ok(None),
    }
}

/// The remote hash the server checks; a synthesized tombstone has none.
fn parent_hash(action: &SyncAction) -> Option<&str> {
    action
        .remote
        .as_ref()
        .map(|entry| entry.hash.as_str())
        .filter(|hash| !hash.is_empty())
}

fn build_upload_action_for_path(
    local_root: &Path,
    path: &str,
    keys: &CryptoKeys,
) -> Result<SyncAction, SyncEngineError> {
    let absolute_path = local_root.join(path);
    let metadata = fs::metadata(&absolute_path)?;
    Ok(SyncAction {
        path: path.to_string(),
        kind: SyncActionKind::Upload,
        local: Some(FileEntry {
            hash: hash_file(&keys.content_mac, &absolute_path)?,
            modified: metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
                .as_secs(),
            size: metadata.len(),
            deleted: false,
            enc_path: String::new(),
        }),
        remote: None,
    })
}

fn conflict_to_upload(conflict: &Conflict) -> SyncAction {
    SyncAction {
        path: conflict.path.clone(),
        kind: if conflict.local.deleted {
            SyncActionKind::DeleteRemote
        } else {
            SyncActionKind::Upload
        },
        local: Some(conflict.local.clone()),
        remote: Some(conflict.remote.clone()),
    }
}

/// Decrypt a downloaded blob and write it atomically, off the async runtime.
/// Returns the plaintext length.
async fn decrypt_and_write(
    local_root: &Path,
    relative_path: &str,
    keys: &CryptoKeys,
    blob: Vec<u8>,
) -> Result<usize, SyncEngineError> {
    let path = local_root.join(relative_path);
    let content_enc = keys.content_enc;
    blocking(move || {
        let plaintext = decrypt(&content_enc, &blob)?;
        write_atomic(&path, &plaintext)?;
        Ok(plaintext.len())
    })
    .await
}

async fn delete_local_file(local_root: &Path, relative_path: &str) -> Result<(), SyncEngineError> {
    let root = local_root.to_path_buf();
    let path = root.join(relative_path);
    blocking(move || {
        if !path.exists() {
            return Ok(());
        }
        fs::remove_file(&path)?;
        cleanup_empty_dirs(&root, path.parent());
        Ok(())
    })
    .await
}

fn cleanup_empty_dirs(local_root: &Path, mut current: Option<&Path>) {
    while let Some(path) = current {
        if path == local_root {
            break;
        }

        match fs::remove_dir(path) {
            Ok(()) => current = path.parent(),
            Err(_) => break,
        }
    }
}

fn conflict_copy_path(original: &str) -> String {
    let path = Path::new(original);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(original);
    let extension = path.extension().and_then(|value| value.to_str());
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let file_name = match extension {
        Some(extension) => format!("{stem}.conflict.{extension}"),
        None => format!("{stem}.conflict"),
    };

    match parent {
        Some(parent) => format!("{}/{}", parent.to_string_lossy(), file_name),
        None => file_name,
    }
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use httpmock::{Method::DELETE, Method::GET, Method::POST, MockServer};
    use tempfile::tempdir;

    use super::{
        complete_sync, conflict_copy_path, load_manifest_from_disk, ordered_for_apply,
        prepare_sync, remote_manifest_cache_path, save_manifest_to_disk, sync_manifest_path,
    };
    use crate::{
        crypto::{content_hmac, derive_keys, encrypt, encrypt_path, path_token, CryptoKeys},
        progress::NoProgress,
        types::{
            ConflictResolution, ConflictResolutionChoice, FileEntry, Manifest, SyncAction,
            SyncActionKind, SyncPlan, VaultConfig,
        },
    };

    /// The JSON body of a batch answer: one result per operation.
    fn batch_results(results: &[(&str, u16, Option<serde_json::Value>)]) -> serde_json::Value {
        serde_json::json!({
            "results": results
                .iter()
                .map(|(path, status, conflict)| serde_json::json!({
                    "path": path,
                    "status": status,
                    "conflict": conflict,
                }))
                .collect::<Vec<_>>()
        })
    }

    /// One manifest entry as the server serialises it (also the `current`
    /// body of a 409).
    fn server_entry(
        keys: &CryptoKeys,
        path: &str,
        content: &[u8],
        modified: u64,
        deleted: bool,
    ) -> serde_json::Value {
        let enc_path = encrypt_path(&keys.path_enc, path).unwrap();
        serde_json::json!({
            "hash": content_hmac(&keys.content_mac, content),
            "modified": modified,
            "size": content.len(),
            "deleted": deleted,
            "encPath": enc_path,
        })
    }

    /// Build the JSON the server would return for a single-file manifest:
    /// keyed by the path token, with an HMAC hash and recoverable `encPath`.
    fn server_manifest(
        keys: &CryptoKeys,
        path: &str,
        content: &[u8],
        modified: u64,
        deleted: bool,
    ) -> serde_json::Value {
        let token = path_token(&keys.path_token, path);
        serde_json::json!({ token: server_entry(keys, path, content, modified, deleted) })
    }

    /// Merge several single-file manifests into one server manifest.
    fn merge_manifests(parts: Vec<serde_json::Value>) -> serde_json::Value {
        let mut merged = serde_json::Map::new();
        for part in parts {
            if let serde_json::Value::Object(map) = part {
                merged.extend(map);
            }
        }
        serde_json::Value::Object(merged)
    }

    /// The base checkpoint `.obsink/manifest.json` as an earlier sync would
    /// have left it: keyed by real path.
    fn write_base(dir: &std::path::Path, keys: &CryptoKeys, entries: &[(&str, &[u8], bool)]) {
        let manifest = entries
            .iter()
            .map(|(path, content, deleted)| {
                (
                    (*path).to_string(),
                    FileEntry {
                        hash: content_hmac(&keys.content_mac, content),
                        modified: 1,
                        size: content.len() as u64,
                        deleted: *deleted,
                        enc_path: String::new(),
                    },
                )
            })
            .collect::<Manifest>();
        save_manifest_to_disk(&sync_manifest_path(dir), &manifest).unwrap();
    }

    fn resolution(path: &str, choice: ConflictResolutionChoice) -> ConflictResolution {
        ConflictResolution {
            path: path.to_string(),
            choice,
        }
    }

    #[test]
    fn loads_missing_manifest_as_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.json");

        let manifest = load_manifest_from_disk(&path).unwrap();

        assert!(manifest.is_empty());
    }

    #[test]
    fn round_trips_manifest_on_disk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        let mut manifest = Manifest::new();
        manifest.insert(
            "note.md".to_string(),
            FileEntry {
                hash: "abc".to_string(),
                modified: 1,
                size: 5,
                deleted: false,
                enc_path: String::new(),
            },
        );

        save_manifest_to_disk(&path, &manifest).unwrap();
        let loaded = load_manifest_from_disk(&path).unwrap();

        assert_eq!(manifest, loaded);
    }

    #[test]
    fn manifest_path_lives_under_obsink_folder() {
        let dir = tempdir().unwrap();
        assert_eq!(
            sync_manifest_path(dir.path()),
            dir.path().join(".obsink/manifest.json")
        );
    }

    #[test]
    fn conflict_copy_keeps_extension() {
        assert_eq!(
            conflict_copy_path("notes/today.md"),
            "notes/today.conflict.md"
        );
        assert_eq!(conflict_copy_path("todo"), "todo.conflict");
    }

    #[test]
    fn hasher_ignores_internal_metadata() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".obsink")).unwrap();
        fs::write(dir.path().join(".obsink/manifest.json"), "{}".as_bytes()).unwrap();
        fs::write(dir.path().join("note.md"), "hello".as_bytes()).unwrap();

        let keys = derive_keys(&[3_u8; 32]);
        let manifest = crate::build_manifest_from_dir(dir.path(), &keys).unwrap();

        assert_eq!(manifest.len(), 1);
        assert!(manifest.contains_key("note.md"));
    }

    fn config(base_url: String, local_path: String) -> VaultConfig {
        VaultConfig {
            server_url: base_url,
            api_key: "token".to_string(),
            vault_id: "vault_123".to_string(),
            local_path,
            ignore: Vec::new(),
        }
    }

    #[tokio::test]
    async fn first_time_sync_downloads_remote_files() {
        let dir = tempdir().unwrap();
        let server = MockServer::start_async().await;
        let key = [7_u8; 32];
        let keys = derive_keys(&key);
        let encrypted = encrypt(&keys.content_enc, b"hello remote").unwrap();
        let token = path_token(&keys.path_token, "note.md");
        let manifest_json = server_manifest(&keys, "note.md", b"hello remote", 10, false);

        let body = manifest_json.clone();
        server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path("/vaults/vault_123/manifest")
                    .header("authorization", "Bearer token");
                then.status(200).json_body_obj(&body);
            })
            .await;
        server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path(format!("/vaults/vault_123/files/{token}"))
                    .header("authorization", "Bearer token");
                then.status(200).body(encrypted.clone());
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(plan.download.len(), 1);

        let result = complete_sync(&cfg, &key, &plan, &[], &NoProgress)
            .await
            .unwrap();
        assert!(result.conflicts.is_empty());
        assert_eq!(
            fs::read_to_string(dir.path().join("note.md")).unwrap(),
            "hello remote"
        );
        let manifest = load_manifest_from_disk(&sync_manifest_path(dir.path())).unwrap();
        assert!(manifest.contains_key("note.md"));
    }

    #[tokio::test]
    async fn sync_uploads_local_files() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("local.md"), "hello local").unwrap();
        let server = MockServer::start_async().await;
        let key = [5_u8; 32];
        let keys = derive_keys(&key);
        let token = path_token(&keys.path_token, "local.md");
        let manifest_after = server_manifest(&keys, "local.md", b"hello local", 10, false);

        server
            .mock_async(|when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&serde_json::json!({}));
            })
            .await;
        let batch_mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/vaults/vault_123/batch")
                    .header_exists("content-type")
                    .body_contains("\"action\":\"put\"")
                    .body_contains(format!("\"path\":\"{token}\""))
                    .body_contains("\"encPath\":\"")
                    .body_contains("name=\"content\"; filename=\"0\"");
                then.status(200)
                    .json_body_obj(&batch_results(&[(&token, 200, None)]));
            })
            .await;
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&manifest_after);
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(plan.upload.len(), 1);

        let result = complete_sync(&cfg, &key, &plan, &[], &NoProgress)
            .await
            .unwrap();
        batch_mock.assert_async().await;
        assert!(result.conflicts.is_empty());
        assert!(result.failures.is_empty());
    }

    #[tokio::test]
    async fn keep_remote_resolves_conflict_and_updates_local_file() {
        let dir = tempdir().unwrap();
        // base != local != remote: a real three-way conflict, whatever the mtimes.
        let note_path = dir.path().join("note.md");
        fs::write(&note_path, "local version").unwrap();
        let server = MockServer::start_async().await;
        let key = [9_u8; 32];
        let keys = derive_keys(&key);
        let encrypted = encrypt(&keys.content_enc, b"remote version").unwrap();
        let token = path_token(&keys.path_token, "note.md");
        let remote_manifest = server_manifest(&keys, "note.md", b"remote version", 1, false);
        write_base(dir.path(), &keys, &[("note.md", b"base", false)]);

        let body = remote_manifest.clone();
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&body);
            })
            .await;
        server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path(format!("/vaults/vault_123/files/{token}"));
                then.status(200).body(encrypted.clone());
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(plan.conflicts.len(), 1);

        let result = complete_sync(
            &cfg,
            &key,
            &plan,
            &[ConflictResolution {
                path: "note.md".to_string(),
                choice: ConflictResolutionChoice::KeepRemote,
            }],
            &NoProgress,
        )
        .await
        .unwrap();

        assert!(result.conflicts.is_empty());
        assert_eq!(
            fs::read_to_string(dir.path().join("note.md")).unwrap(),
            "remote version"
        );
        let checkpoint = load_manifest_from_disk(&sync_manifest_path(dir.path())).unwrap();
        assert_eq!(
            checkpoint["note.md"].hash,
            content_hmac(&keys.content_mac, b"remote version")
        );
    }

    #[tokio::test]
    async fn upload_loop_continues_past_per_file_failure() {
        // a.md, b.md, c.md travel in one batch (BTreeSet order). The server
        // answers 413 for b (per-file, non-fatal): a and c count as landed,
        // b is recorded as a failure and held back so the next sync retries
        // it. The manifest still checkpoints because nothing fatal occurred.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "aaa").unwrap();
        fs::write(dir.path().join("b.md"), "bbb").unwrap();
        fs::write(dir.path().join("c.md"), "ccc").unwrap();
        let server = MockServer::start_async().await;
        let key = [11_u8; 32];
        let keys = derive_keys(&key);
        let token_a = path_token(&keys.path_token, "a.md");
        let token_b = path_token(&keys.path_token, "b.md");
        let token_c = path_token(&keys.path_token, "c.md");

        let empty_manifest = server
            .mock_async(|when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&serde_json::json!({}));
            })
            .await;
        let batch = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/vaults/vault_123/batch")
                    .body_contains(format!("\"path\":\"{token_a}\""))
                    .body_contains(format!("\"path\":\"{token_b}\""))
                    .body_contains(format!("\"path\":\"{token_c}\""));
                then.status(200).json_body_obj(&batch_results(&[
                    (&token_a, 200, None),
                    (&token_b, 413, None),
                    (&token_c, 200, None),
                ]));
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(plan.upload.len(), 3);

        // httpmock serves the first matching mock, so swap the manifest for
        // what the server holds after a and c landed.
        empty_manifest.delete_async().await;
        let after = merge_manifests(vec![
            server_manifest(&keys, "a.md", b"aaa", 5, false),
            server_manifest(&keys, "c.md", b"ccc", 5, false),
        ]);
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&after);
            })
            .await;

        let result = complete_sync(&cfg, &key, &plan, &[], &NoProgress)
            .await
            .unwrap();

        // One round trip for all three files.
        batch.assert_hits_async(1).await;
        // Exactly one non-fatal failure, for b.md.
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].path, "b.md");
        assert!(!result.failures[0].fatal);
        assert!(result.conflicts.is_empty());

        let checkpoint = load_manifest_from_disk(&sync_manifest_path(dir.path())).unwrap();
        assert!(checkpoint.contains_key("a.md"));
        assert!(checkpoint.contains_key("c.md"));
        assert!(!checkpoint.contains_key("b.md"));

        let second = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(second.upload.len(), 1);
        assert_eq!(second.upload[0].path, "b.md");
        assert!(second.download.is_empty() && second.conflicts.is_empty());
    }

    #[tokio::test]
    async fn upload_loop_stops_on_fatal_server_error() {
        // The batch request itself fails with 500: every member is a fatal
        // failure and the manifest checkpoint is skipped.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "aaa").unwrap();
        fs::write(dir.path().join("b.md"), "bbb").unwrap();
        let server = MockServer::start_async().await;
        let key = [12_u8; 32];

        server
            .mock_async(|when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&serde_json::json!({}));
            })
            .await;
        let batch = server
            .mock_async(|when, then| {
                when.method(POST).path("/vaults/vault_123/batch");
                then.status(500).body("server error");
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        let result = complete_sync(&cfg, &key, &plan, &[], &NoProgress)
            .await
            .unwrap();

        batch.assert_hits_async(1).await;
        assert_eq!(result.failures.len(), 2);
        assert!(result.failures.iter().all(|failure| failure.fatal));
        assert_eq!(
            result
                .failures
                .iter()
                .map(|failure| failure.path.as_str())
                .collect::<Vec<_>>(),
            vec!["a.md", "b.md"]
        );
        assert!(!sync_manifest_path(dir.path()).exists());
    }

    #[tokio::test]
    async fn a_fatal_per_operation_status_stops_the_remaining_batches() {
        // Two batches (65 files); a 500 inside the first batch's results is
        // systemic, so the second batch is never sent.
        let dir = tempdir().unwrap();
        let mut paths = Vec::new();
        for i in 0..65 {
            let name = format!("f{i:03}.md");
            fs::write(dir.path().join(&name), "x").unwrap();
            paths.push(name);
        }
        let server = MockServer::start_async().await;
        let key = [13_u8; 32];
        let keys = derive_keys(&key);
        server
            .mock_async(|when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&serde_json::json!({}));
            })
            .await;
        let tokens: Vec<String> = paths
            .iter()
            .map(|path| path_token(&keys.path_token, path))
            .collect();
        let first_results: Vec<(&str, u16, Option<serde_json::Value>)> = tokens[..64]
            .iter()
            .enumerate()
            .map(|(i, token)| (token.as_str(), if i == 3 { 500 } else { 200 }, None))
            .collect();
        let batch = server
            .mock_async(|when, then| {
                when.method(POST).path("/vaults/vault_123/batch");
                then.status(200)
                    .json_body_obj(&batch_results(&first_results));
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(plan.upload.len(), 65);
        let result = complete_sync(&cfg, &key, &plan, &[], &NoProgress)
            .await
            .unwrap();

        batch.assert_hits_async(1).await;
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].path, "f003.md");
        assert!(result.failures[0].fatal);
        assert!(!sync_manifest_path(dir.path()).exists());
    }

    #[tokio::test]
    async fn batch_results_map_per_operation() {
        // 200, 409 (late conflict), 413 (per-file failure) in one answer.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "aaa").unwrap();
        fs::write(dir.path().join("b.md"), "bbb").unwrap();
        fs::write(dir.path().join("c.md"), "ccc").unwrap();
        let server = MockServer::start_async().await;
        let key = [14_u8; 32];
        let keys = derive_keys(&key);
        let token_a = path_token(&keys.path_token, "a.md");
        let token_b = path_token(&keys.path_token, "b.md");
        let token_c = path_token(&keys.path_token, "c.md");
        let other = server_entry(&keys, "b.md", b"other device", 2, false);

        let empty_manifest = server
            .mock_async(|when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&serde_json::json!({}));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(POST).path("/vaults/vault_123/batch");
                then.status(200).json_body_obj(&batch_results(&[
                    (&token_a, 200, None),
                    (
                        &token_b,
                        409,
                        Some(serde_json::json!({ "path": token_b, "current": other })),
                    ),
                    (&token_c, 413, None),
                ]));
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        empty_manifest.delete_async().await;
        let after = merge_manifests(vec![
            server_manifest(&keys, "a.md", b"aaa", 5, false),
            server_manifest(&keys, "b.md", b"other device", 2, false),
        ]);
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&after);
            })
            .await;
        let result = complete_sync(&cfg, &key, &plan, &[], &NoProgress)
            .await
            .unwrap();

        assert_eq!(result.conflicts.len(), 1);
        assert_eq!(result.conflicts[0].path, "b.md");
        assert_eq!(
            result.conflicts[0].remote.hash,
            content_hmac(&keys.content_mac, b"other device")
        );
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].path, "c.md");
        assert!(!result.failures[0].fatal);
        let checkpoint = load_manifest_from_disk(&sync_manifest_path(dir.path())).unwrap();
        assert!(checkpoint.contains_key("a.md"));
        assert!(
            !checkpoint.contains_key("b.md"),
            "conflicted path held back"
        );
        assert!(!checkpoint.contains_key("c.md"), "failed path held back");
    }

    #[tokio::test]
    async fn download_loop_continues_past_per_file_failure() {
        // Remote has a.md and b.md; b's GET returns 404 (per-file, non-fatal).
        // a downloads fine, b is recorded as a failure, sync keeps going.
        let dir = tempdir().unwrap();
        let server = MockServer::start_async().await;
        let key = [13_u8; 32];
        let keys = derive_keys(&key);
        let encrypted_a = encrypt(&keys.content_enc, b"file a").unwrap();
        let token_a = path_token(&keys.path_token, "a.md");
        let token_b = path_token(&keys.path_token, "b.md");

        let manifest = merge_manifests(vec![
            server_manifest(&keys, "a.md", b"file a", 1, false),
            server_manifest(&keys, "b.md", b"file b", 1, false),
        ]);

        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&manifest);
            })
            .await;
        server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path(format!("/vaults/vault_123/files/{token_a}"));
                then.status(200).body(encrypted_a.clone());
            })
            .await;
        server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path(format!("/vaults/vault_123/files/{token_b}"));
                then.status(404).body("not found");
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(plan.download.len(), 2);
        assert_eq!(plan.failures.len(), 1);
        assert_eq!(plan.failures[0].path, "b.md");
        assert!(!plan.failures[0].fatal);

        // a.md was written; b.md was not.
        assert_eq!(
            fs::read_to_string(dir.path().join("a.md")).unwrap(),
            "file a"
        );
        assert!(!dir.path().join("b.md").exists());
    }

    #[tokio::test]
    async fn second_sync_uses_etag_and_304() {
        let dir = tempdir().unwrap();
        let server = MockServer::start_async().await;
        let key = [7_u8; 32];
        let keys = derive_keys(&key);
        let manifest_json = server_manifest(&keys, "note.md", b"remote", 10, false);
        let encrypted = encrypt(&keys.content_enc, b"remote").unwrap();
        let token = path_token(&keys.path_token, "note.md");

        let body = manifest_json.clone();
        let fresh = server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path("/vaults/vault_123/manifest")
                    .matches(|req| {
                        !req.headers.as_ref().is_some_and(|headers| {
                            headers
                                .iter()
                                .any(|(name, _)| name.eq_ignore_ascii_case("if-none-match"))
                        })
                    });
                then.status(200)
                    .header("etag", "\"3\"")
                    .json_body_obj(&body);
            })
            .await;
        let cached = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/vaults/vault_123/manifest")
                    .header("if-none-match", "\"3\"");
                then.status(304).header("etag", "\"3\"");
            })
            .await;
        server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path(format!("/vaults/vault_123/files/{token}"));
                then.status(200).body(encrypted.clone());
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let first = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(first.download.len(), 1);
        assert!(remote_manifest_cache_path(dir.path()).exists());
        let cache: serde_json::Value =
            serde_json::from_slice(&fs::read(remote_manifest_cache_path(dir.path())).unwrap())
                .unwrap();
        assert_eq!(cache["etag"], "\"3\"");

        // Second prepare: the manifest comes from the cache via 304 and the
        // plan is identical (the downloaded file is now local, so no download).
        let second = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert!(second.download.is_empty() && second.upload.is_empty());
        fresh.assert_hits_async(1).await;
        cached.assert_hits_async(1).await;
    }

    #[tokio::test]
    async fn cache_ignored_when_corrupt() {
        let dir = tempdir().unwrap();
        let server = MockServer::start_async().await;
        let key = [7_u8; 32];
        fs::create_dir_all(dir.path().join(".obsink")).unwrap();
        fs::write(remote_manifest_cache_path(dir.path()), b"{garbage").unwrap();

        let fresh = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/vaults/vault_123/manifest")
                    .matches(|req| {
                        !req.headers.as_ref().is_some_and(|headers| {
                            headers
                                .iter()
                                .any(|(name, _)| name.eq_ignore_ascii_case("if-none-match"))
                        })
                    });
                then.status(200).json_body_obj(&serde_json::json!({}));
            })
            .await;
        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert!(plan.download.is_empty());
        fresh.assert_hits_async(1).await;
        // A server without ETags leaves the corrupt file alone (nothing to cache).
        assert_eq!(
            fs::read(remote_manifest_cache_path(dir.path())).unwrap(),
            b"{garbage"
        );
    }

    #[tokio::test]
    async fn download_failure_does_not_tombstone_next_sync() {
        // b.md fails to download (404). complete_sync must hold b back from
        // the checkpoint; the next prepare plans it as a Download again and
        // never as a remote delete.
        let dir = tempdir().unwrap();
        let server = MockServer::start_async().await;
        let key = [14_u8; 32];
        let keys = derive_keys(&key);
        let encrypted_a = encrypt(&keys.content_enc, b"file a").unwrap();
        let token_a = path_token(&keys.path_token, "a.md");
        let token_b = path_token(&keys.path_token, "b.md");
        let manifest = merge_manifests(vec![
            server_manifest(&keys, "a.md", b"file a", 1, false),
            server_manifest(&keys, "b.md", b"file b", 1, false),
        ]);

        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&manifest);
            })
            .await;
        server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path(format!("/vaults/vault_123/files/{token_a}"));
                then.status(200).body(encrypted_a.clone());
            })
            .await;
        server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path(format!("/vaults/vault_123/files/{token_b}"));
                then.status(404).body("not found");
            })
            .await;
        let token_b_delete = path_token(&keys.path_token, "b.md");
        let delete_b = server
            .mock_async(move |when, then| {
                when.method(DELETE)
                    .path(format!("/vaults/vault_123/files/{token_b_delete}"));
                then.status(200);
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        let result = complete_sync(&cfg, &key, &plan, &[], &NoProgress)
            .await
            .unwrap();
        assert_eq!(result.failures.len(), 1);

        let checkpoint = load_manifest_from_disk(&sync_manifest_path(dir.path())).unwrap();
        assert!(checkpoint.contains_key("a.md"));
        assert!(!checkpoint.contains_key("b.md"));

        let second = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert!(second.upload.is_empty(), "{:?}", second.upload);
        assert_eq!(second.download.len(), 1);
        assert_eq!(second.download[0].path, "b.md");
        assert_eq!(second.download[0].kind, SyncActionKind::Download);
        let _ = complete_sync(&cfg, &key, &second, &[], &NoProgress).await;
        delete_b.assert_hits_async(0).await;
    }

    #[tokio::test]
    async fn late_409_yields_reentrant_plan() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("note.md"), "local edit").unwrap();
        let server = MockServer::start_async().await;
        let key = [15_u8; 32];
        let keys = derive_keys(&key);
        let token = path_token(&keys.path_token, "note.md");
        write_base(dir.path(), &keys, &[("note.md", b"base", false)]);
        let base_hash = content_hmac(&keys.content_mac, b"base");
        let other_hash = content_hmac(&keys.content_mac, b"other device");

        // prepare sees the base still on the server; by the time we PUT,
        // another device has landed "other device".
        let stale = server_manifest(&keys, "note.md", b"base", 1, false);
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&stale);
            })
            .await;
        let current = server_entry(&keys, "note.md", b"other device", 2, false);
        let conflict_put = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/vaults/vault_123/batch")
                    .body_contains(format!("\"parentHash\":\"{base_hash}\""));
                then.status(200).json_body_obj(&batch_results(&[(
                    &token,
                    409,
                    Some(serde_json::json!({ "path": token, "current": current })),
                )]));
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(plan.upload.len(), 1);
        assert!(plan.conflicts.is_empty());

        let result = complete_sync(&cfg, &key, &plan, &[], &NoProgress)
            .await
            .unwrap();
        conflict_put.assert_hits_async(1).await;
        assert_eq!(result.conflicts.len(), 1);
        let late = &result.conflicts[0];
        assert_eq!(
            late.local.hash,
            content_hmac(&keys.content_mac, b"local edit")
        );
        assert_eq!(late.remote.hash, other_hash);
        // The checkpoint ran but kept the base entry for the conflicted path.
        let checkpoint = load_manifest_from_disk(&sync_manifest_path(dir.path())).unwrap();
        assert_eq!(
            checkpoint["note.md"].hash,
            content_hmac(&keys.content_mac, b"base")
        );

        let reentrant = SyncPlan::from_late_conflicts(&result).unwrap();
        assert_eq!(reentrant.conflicts.len(), 1);
        assert!(reentrant.upload.is_empty());

        conflict_put.delete_async().await;
        let winning_put = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/vaults/vault_123/batch")
                    .body_contains(format!("\"parentHash\":\"{other_hash}\""));
                then.status(200)
                    .json_body_obj(&batch_results(&[(&token, 200, None)]));
            })
            .await;
        let result = complete_sync(
            &cfg,
            &key,
            &reentrant,
            &[resolution("note.md", ConflictResolutionChoice::KeepLocal)],
            &NoProgress,
        )
        .await
        .unwrap();
        winning_put.assert_hits_async(1).await;
        assert!(result.conflicts.is_empty());
        assert!(SyncPlan::from_late_conflicts(&result).is_none());
    }

    #[tokio::test]
    async fn keep_both_on_remote_tombstone_behaves_as_keep_local() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("note.md"), "local edit").unwrap();
        let server = MockServer::start_async().await;
        let key = [16_u8; 32];
        let keys = derive_keys(&key);
        let token = path_token(&keys.path_token, "note.md");
        write_base(dir.path(), &keys, &[("note.md", b"base", false)]);
        let base_hash = content_hmac(&keys.content_mac, b"base");

        let remote = server_manifest(&keys, "note.md", b"base", 2, true);
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&remote);
            })
            .await;
        let get_file = server
            .mock_async(move |when, then| {
                when.method(GET).path_contains("/files/");
                then.status(200).body(b"unused");
            })
            .await;
        let put = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/vaults/vault_123/batch")
                    .body_contains("\"action\":\"put\"")
                    .body_contains(format!("\"parentHash\":\"{base_hash}\""));
                then.status(200)
                    .json_body_obj(&batch_results(&[(&token, 200, None)]));
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        assert!(plan.conflicts[0].remote.deleted);

        let result = complete_sync(
            &cfg,
            &key,
            &plan,
            &[resolution("note.md", ConflictResolutionChoice::KeepBoth)],
            &NoProgress,
        )
        .await
        .unwrap();

        put.assert_hits_async(1).await;
        get_file.assert_hits_async(0).await;
        assert!(result.conflicts.is_empty());
        assert!(!dir.path().join("note.conflict.md").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("note.md")).unwrap(),
            "local edit"
        );
    }

    #[tokio::test]
    async fn keep_both_on_local_tombstone_behaves_as_keep_remote() {
        let dir = tempdir().unwrap();
        let server = MockServer::start_async().await;
        let key = [17_u8; 32];
        let keys = derive_keys(&key);
        let token = path_token(&keys.path_token, "note.md");
        // base had the note; it is gone locally; the server has a newer edit.
        write_base(dir.path(), &keys, &[("note.md", b"base", false)]);
        let encrypted = encrypt(&keys.content_enc, b"remote edit").unwrap();

        let remote = server_manifest(&keys, "note.md", b"remote edit", 2, false);
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&remote);
            })
            .await;
        server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path(format!("/vaults/vault_123/files/{token}"));
                then.status(200).body(encrypted.clone());
            })
            .await;
        let delete = server
            .mock_async(move |when, then| {
                when.method(DELETE).path_contains("/files/");
                then.status(200);
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        assert!(plan.conflicts[0].local.deleted);

        let result = complete_sync(
            &cfg,
            &key,
            &plan,
            &[resolution("note.md", ConflictResolutionChoice::KeepBoth)],
            &NoProgress,
        )
        .await
        .unwrap();

        delete.assert_hits_async(0).await;
        assert!(result.conflicts.is_empty());
        assert!(!dir.path().join("note.conflict.md").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("note.md")).unwrap(),
            "remote edit"
        );
    }

    #[test]
    fn deletes_apply_before_downloads() {
        let download = SyncAction {
            path: "a.md".to_string(),
            kind: SyncActionKind::Download,
            local: None,
            remote: None,
        };
        let delete = SyncAction {
            path: "z.md".to_string(),
            kind: SyncActionKind::DeleteLocal,
            local: None,
            remote: None,
        };
        let actions = [download, delete];
        let ordered: Vec<&str> = ordered_for_apply(&actions)
            .map(|action| action.path.as_str())
            .collect();
        assert_eq!(ordered, vec!["z.md", "a.md"]);
    }

    #[tokio::test]
    async fn case_only_rename_survives_apply() {
        // Server renamed Note.md -> note.md: a tombstone for the old spelling
        // and a live entry for the new one. Deleting first, then downloading,
        // leaves note.md in place on a case-insensitive volume.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Note.md"), "old spelling").unwrap();
        let server = MockServer::start_async().await;
        let key = [18_u8; 32];
        let keys = derive_keys(&key);
        write_base(dir.path(), &keys, &[("Note.md", b"old spelling", false)]);
        let encrypted = encrypt(&keys.content_enc, b"new spelling").unwrap();
        let token_new = path_token(&keys.path_token, "note.md");

        let remote = merge_manifests(vec![
            server_manifest(&keys, "Note.md", b"old spelling", 2, true),
            server_manifest(&keys, "note.md", b"new spelling", 2, false),
        ]);
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&remote);
            })
            .await;
        server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path(format!("/vaults/vault_123/files/{token_new}"));
                then.status(200).body(encrypted.clone());
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(plan.download.len(), 2);
        assert!(plan.failures.is_empty());
        assert_eq!(
            fs::read_to_string(dir.path().join("note.md")).unwrap(),
            "new spelling"
        );
    }

    #[tokio::test]
    async fn remote_tombstone_removes_unchanged_local_and_converges() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("note.md"), "same").unwrap();
        let server = MockServer::start_async().await;
        let key = [19_u8; 32];
        let keys = derive_keys(&key);
        write_base(dir.path(), &keys, &[("note.md", b"same", false)]);

        let remote = server_manifest(&keys, "note.md", b"same", 2, true);
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&remote);
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(plan.download.len(), 1);
        assert_eq!(plan.download[0].kind, SyncActionKind::DeleteLocal);
        assert!(!dir.path().join("note.md").exists());

        let result = complete_sync(&cfg, &key, &plan, &[], &NoProgress)
            .await
            .unwrap();
        assert!(result.failures.is_empty());
        let checkpoint = load_manifest_from_disk(&sync_manifest_path(dir.path())).unwrap();
        assert!(checkpoint["note.md"].deleted);

        let second = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert!(second.upload.is_empty() && second.download.is_empty());
        assert!(second.conflicts.is_empty());
    }

    #[tokio::test]
    async fn converged_first_sync_is_noop() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("note.md"), "same").unwrap();
        let server = MockServer::start_async().await;
        let key = [20_u8; 32];
        let keys = derive_keys(&key);

        let remote = server_manifest(&keys, "note.md", b"same", 99, false);
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&remote);
            })
            .await;
        let writes = server
            .mock_async(|when, then| {
                when.method(POST).path("/vaults/vault_123/batch");
                then.status(200).json_body_obj(&batch_results(&[]));
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert!(plan.upload.is_empty() && plan.download.is_empty());
        assert!(plan.conflicts.is_empty());
        let _ = complete_sync(&cfg, &key, &plan, &[], &NoProgress)
            .await
            .unwrap();
        writes.assert_hits_async(0).await;
    }

    #[test]
    fn chunk_uploads_splits_by_bytes_and_op_count() {
        use super::{chunk_uploads, BATCH_BYTE_BUDGET, BATCH_MAX_OPS};

        assert!(chunk_uploads(&[]).is_empty());
        assert_eq!(chunk_uploads(&[1, 2, 3]), vec![0..3]);

        // Op count: 64 per batch.
        let sizes = vec![1u64; BATCH_MAX_OPS * 2 + 1];
        assert_eq!(chunk_uploads(&sizes), vec![0..64, 64..128, 128..129]);

        // Byte budget: the file that would overflow starts the next batch.
        let half = BATCH_BYTE_BUDGET / 2;
        assert_eq!(chunk_uploads(&[half, half, 1, half]), vec![0..2, 2..4]);

        // An oversize file travels alone, and does not drag its neighbours.
        let huge = BATCH_BYTE_BUDGET + 1;
        assert_eq!(chunk_uploads(&[1, huge, 1]), vec![0..1, 1..2, 2..3]);
        assert_eq!(chunk_uploads(&[huge]), vec![0..1]);

        // Deletes cost nothing.
        let sizes = vec![0u64; 10];
        assert_eq!(chunk_uploads(&sizes), vec![0..10]);
    }

    #[tokio::test]
    async fn downloads_run_concurrently() {
        // Eight files, each served after 300 ms: sequential would take 2.4 s,
        // eight-wide takes one delay.
        let dir = tempdir().unwrap();
        let server = MockServer::start_async().await;
        let key = [21_u8; 32];
        let keys = derive_keys(&key);
        let names: Vec<String> = (0..8).map(|i| format!("n{i}.md")).collect();
        let manifest = merge_manifests(
            names
                .iter()
                .map(|name| server_manifest(&keys, name, name.as_bytes(), 1, false))
                .collect(),
        );
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&manifest);
            })
            .await;
        for name in &names {
            let token = path_token(&keys.path_token, name);
            let body = encrypt(&keys.content_enc, name.as_bytes()).unwrap();
            server
                .mock_async(move |when, then| {
                    when.method(GET)
                        .path(format!("/vaults/vault_123/files/{token}"));
                    then.status(200)
                        .delay(std::time::Duration::from_millis(300))
                        .body(body.clone());
                })
                .await;
        }

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let started = std::time::Instant::now();
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        let elapsed = started.elapsed();
        assert!(plan.failures.is_empty());
        assert_eq!(plan.download.len(), 8);
        for name in &names {
            assert_eq!(fs::read_to_string(dir.path().join(name)).unwrap(), *name);
        }
        assert!(
            elapsed < std::time::Duration::from_millis(1500),
            "eight 300 ms downloads took {elapsed:?}; they ran one at a time"
        );
    }

    #[tokio::test]
    async fn a_fatal_download_stops_launching_more() {
        // Twenty files; the first GET is a 500 that arrives after a delay,
        // so at most the eight in flight are started before `stop` is set.
        let dir = tempdir().unwrap();
        let server = MockServer::start_async().await;
        let key = [22_u8; 32];
        let keys = derive_keys(&key);
        let names: Vec<String> = (0..20).map(|i| format!("n{i:02}.md")).collect();
        let manifest = merge_manifests(
            names
                .iter()
                .map(|name| server_manifest(&keys, name, name.as_bytes(), 1, false))
                .collect(),
        );
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&manifest);
            })
            .await;
        let failing_token = path_token(&keys.path_token, &names[0]);
        server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path(format!("/vaults/vault_123/files/{failing_token}"));
                then.status(500).body("server error");
            })
            .await;
        let others = server
            .mock_async(|when, then| {
                when.method(GET).path_contains("/vaults/vault_123/files/");
                then.status(200)
                    .delay(std::time::Duration::from_millis(200))
                    .body(b"not a valid blob");
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert!(plan.failures.iter().any(|failure| failure.fatal));
        // The fatal one plus whatever was already in flight; never all twenty.
        assert!(others.hits_async().await < 19, "every fetch was launched");
        assert!(plan.failures.len() < 20);
    }

    #[tokio::test]
    async fn defer_holds_the_conflict_back_and_syncs_the_rest() {
        // note.md conflicts (both sides changed since the base); other.md is a
        // plain local edit. Deferring the conflict uploads other.md, leaves
        // note.md untouched on both sides, reports it on the result and keeps
        // its base entry so the next diff still sees "both changed".
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("note.md"), "local edit").unwrap();
        fs::write(dir.path().join("other.md"), "other local").unwrap();
        let server = MockServer::start_async().await;
        let key = [23_u8; 32];
        let keys = derive_keys(&key);
        write_base(
            dir.path(),
            &keys,
            &[
                ("note.md", b"base", false),
                ("other.md", b"other base", false),
            ],
        );
        let token_note = path_token(&keys.path_token, "note.md");
        let token_other = path_token(&keys.path_token, "other.md");

        let remote = merge_manifests(vec![
            server_manifest(&keys, "note.md", b"remote edit", 2, false),
            server_manifest(&keys, "other.md", b"other base", 1, false),
        ]);
        let manifest_mock = server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&remote);
            })
            .await;
        let batch = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/vaults/vault_123/batch")
                    .body_contains(format!("\"path\":\"{token_other}\""));
                then.status(200)
                    .json_body_obj(&batch_results(&[(&token_other, 200, None)]));
            })
            .await;
        let downloads = server
            .mock_async(|when, then| {
                when.method(GET).path_contains("/files/");
                then.status(200).body(b"unused");
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.upload.len(), 1);

        manifest_mock.delete_async().await;
        let after = merge_manifests(vec![
            server_manifest(&keys, "note.md", b"remote edit", 2, false),
            server_manifest(&keys, "other.md", b"other local", 3, false),
        ]);
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&after);
            })
            .await;
        let result = complete_sync(
            &cfg,
            &key,
            &plan,
            &[resolution("note.md", ConflictResolutionChoice::Defer)],
            &NoProgress,
        )
        .await
        .unwrap();

        batch.assert_hits_async(1).await;
        downloads.assert_hits_async(0).await;
        assert_eq!(result.conflicts.len(), 1);
        assert_eq!(result.conflicts[0].path, "note.md");
        assert!(result.failures.is_empty());
        assert_eq!(
            fs::read_to_string(dir.path().join("note.md")).unwrap(),
            "local edit"
        );
        let checkpoint = load_manifest_from_disk(&sync_manifest_path(dir.path())).unwrap();
        assert_eq!(
            checkpoint["note.md"].hash,
            content_hmac(&keys.content_mac, b"base"),
            "the deferred path keeps its base entry"
        );
        assert_eq!(
            checkpoint["other.md"].hash,
            content_hmac(&keys.content_mac, b"other local")
        );
        assert_eq!(token_note.len(), 64);

        // The next prepare sees the same conflict again.
        let again = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(again.conflicts.len(), 1);
        assert!(again.upload.is_empty());
    }

    #[tokio::test]
    async fn an_ignored_path_on_the_server_is_invisible_to_the_diff() {
        // The server (and the base) hold .obsidian/workspace.json from before
        // it was ignored. Locally it exists too, with other content. Nothing
        // is uploaded, downloaded or deleted for it.
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".obsidian")).unwrap();
        fs::write(
            dir.path().join(".obsidian/workspace.json"),
            "{\"local\":true}",
        )
        .unwrap();
        fs::write(dir.path().join("note.md"), "same").unwrap();
        let server = MockServer::start_async().await;
        let key = [24_u8; 32];
        let keys = derive_keys(&key);
        write_base(
            dir.path(),
            &keys,
            &[
                ("note.md", b"same", false),
                (".obsidian/workspace.json", b"{\"base\":true}", false),
            ],
        );
        let remote = merge_manifests(vec![
            server_manifest(&keys, "note.md", b"same", 1, false),
            server_manifest(
                &keys,
                ".obsidian/workspace.json",
                b"{\"remote\":true}",
                2,
                false,
            ),
        ]);
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&remote);
            })
            .await;
        let writes = server
            .mock_async(|when, then| {
                when.method(POST).path("/vaults/vault_123/batch");
                then.status(200).json_body_obj(&batch_results(&[]));
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert!(plan.upload.is_empty(), "{:?}", plan.upload);
        assert!(plan.download.is_empty(), "{:?}", plan.download);
        assert!(plan.conflicts.is_empty());
        let result = complete_sync(&cfg, &key, &plan, &[], &NoProgress)
            .await
            .unwrap();
        writes.assert_hits_async(0).await;
        assert!(result.failures.is_empty());
        assert_eq!(
            fs::read_to_string(dir.path().join(".obsidian/workspace.json")).unwrap(),
            "{\"local\":true}"
        );
    }

    #[tokio::test]
    async fn extra_ignore_patterns_hide_a_local_folder() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("drafts")).unwrap();
        fs::write(dir.path().join("drafts/wip.md"), "wip").unwrap();
        fs::write(dir.path().join("note.md"), "note").unwrap();
        let server = MockServer::start_async().await;
        let key = [25_u8; 32];
        let keys = derive_keys(&key);
        let token = path_token(&keys.path_token, "note.md");
        server
            .mock_async(|when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&serde_json::json!({}));
            })
            .await;
        let batch = server
            .mock_async(|when, then| {
                when.method(POST).path("/vaults/vault_123/batch");
                then.status(200)
                    .json_body_obj(&batch_results(&[(&token, 200, None)]));
            })
            .await;
        let mut cfg = config(server.base_url(), dir.path().display().to_string());
        cfg.ignore = vec!["drafts/".to_string()];
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(
            plan.upload
                .iter()
                .map(|a| a.path.as_str())
                .collect::<Vec<_>>(),
            vec!["note.md"]
        );
        let _ = complete_sync(&cfg, &key, &plan, &[], &NoProgress).await;
        batch.assert_hits_async(1).await;
    }

    #[tokio::test]
    async fn a_checkpoint_failure_is_reported_on_its_own_channel() {
        // The upload lands; the manifest re-fetch afterwards fails. The
        // result carries no file failure, `checkpoint_error` is set, and the
        // base is untouched so the next diff redoes the bookkeeping.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("note.md"), "hello").unwrap();
        let server = MockServer::start_async().await;
        let key = [26_u8; 32];
        let keys = derive_keys(&key);
        let token = path_token(&keys.path_token, "note.md");

        let manifest = server
            .mock_async(|when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&serde_json::json!({}));
            })
            .await;
        let batch = server
            .mock_async(|when, then| {
                when.method(POST).path("/vaults/vault_123/batch");
                then.status(200)
                    .json_body_obj(&batch_results(&[(&token, 200, None)]));
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        manifest.delete_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(503).body("maintenance");
            })
            .await;

        let result = complete_sync(&cfg, &key, &plan, &[], &NoProgress)
            .await
            .unwrap();
        batch.assert_hits_async(1).await;
        assert!(result.failures.is_empty());
        assert_eq!(result.upload.len(), 1);
        let error = result.checkpoint_error.clone().expect("checkpoint error");
        assert!(error.contains("503"), "{error}");
        assert!(!sync_manifest_path(dir.path()).exists());
        let serialized = serde_json::to_value(&result).unwrap();
        assert!(serialized["checkpoint_error"].is_string());
    }

    #[tokio::test]
    async fn a_response_that_cannot_be_decoded_is_not_fatal() {
        // Two downloads; the first blob's GET answers with a body that is
        // not a valid ciphertext (a per-file crypto error) and the manifest
        // is fine. The second download still happens and nothing is fatal.
        // Plus the predicate itself on a decode error from reqwest.
        let dir = tempdir().unwrap();
        let server = MockServer::start_async().await;
        let key = [27_u8; 32];
        let keys = derive_keys(&key);
        let manifest = merge_manifests(vec![
            server_manifest(&keys, "a.md", b"file a", 1, false),
            server_manifest(&keys, "b.md", b"file b", 1, false),
        ]);
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&manifest);
            })
            .await;
        let token_a = path_token(&keys.path_token, "a.md");
        let token_b = path_token(&keys.path_token, "b.md");
        let good = encrypt(&keys.content_enc, b"file b").unwrap();
        server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path(format!("/vaults/vault_123/files/{token_a}"));
                then.status(200).body(b"not ciphertext");
            })
            .await;
        server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path(format!("/vaults/vault_123/files/{token_b}"));
                then.status(200).body(good.clone());
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(plan.failures.len(), 1);
        assert_eq!(plan.failures[0].path, "a.md");
        assert!(!plan.failures[0].fatal);
        assert_eq!(
            fs::read_to_string(dir.path().join("b.md")).unwrap(),
            "file b"
        );

        // A JSON body that is not JSON: reqwest's decode error, not fatal.
        let bad_json = MockServer::start_async().await;
        bad_json
            .mock_async(|when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).body("{not json");
            })
            .await;
        let error = crate::api_client::ApiClient::new(config(bad_json.base_url(), ".".into()))
            .get_manifest(&keys)
            .await
            .unwrap_err();
        assert!(matches!(&error, crate::api_client::ApiError::Http(e) if e.is_decode()));
        assert!(!super::is_fatal_api_error(&error));

        // Nobody listening: a connect error stays fatal.
        let error =
            crate::api_client::ApiClient::new(config("http://127.0.0.1:1".into(), ".".into()))
                .get_manifest(&keys)
                .await
                .unwrap_err();
        assert!(super::is_fatal_api_error(&error));
    }
}
