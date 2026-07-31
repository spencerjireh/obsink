use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::{
    api_client::{ApiClient, ApiError},
    crypto::{decrypt, derive_keys, encrypt, CryptoError, CryptoKeys, KeyBytes},
    hasher::{build_manifest_from_dir, hash_file, HasherError},
    manifest::{diff_manifests, ManifestDiff},
    progress::{ProgressEvent, ProgressSink, SyncPhase},
    types::{
        Conflict, ConflictResolution, ConflictResolutionChoice, FileEntry, Manifest, SyncAction,
        SyncActionKind, SyncFailure, SyncPlan, SyncResult, VaultConfig,
    },
};

const MANIFEST_FILE: &str = ".obsink/manifest.json";

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
    fs::write(path, bytes)?;
    Ok(())
}

pub fn sync_manifest_path(local_root: &Path) -> PathBuf {
    local_root.join(MANIFEST_FILE)
}

pub fn diff_local_and_remote(local: &Manifest, remote: &Manifest) -> ManifestDiff {
    diff_manifests(local, remote)
}

pub fn build_working_manifest_for_path(
    local_root: &Path,
    keys: &CryptoKeys,
) -> Result<Manifest, SyncEngineError> {
    let previous_manifest = load_manifest_from_disk(&sync_manifest_path(local_root))?;
    build_working_manifest(local_root, &previous_manifest, keys)
}

pub async fn prepare_sync(
    config: &VaultConfig,
    key: &KeyBytes,
    progress: &dyn ProgressSink,
) -> Result<SyncPlan, SyncEngineError> {
    let keys = derive_keys(key);
    let client = ApiClient::new(config.clone());
    let local_root = Path::new(&config.local_path);
    let working_manifest = build_working_manifest_for_path(local_root, &keys)?;
    let remote_manifest = client.get_manifest(&keys).await?;
    let diff = diff_manifests(&working_manifest, &remote_manifest);

    progress.report(ProgressEvent::Phase(SyncPhase::Downloading));
    let download_failures = apply_downloads(local_root, &keys, &client, &diff.download, progress).await;

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

    let resolution_map = resolutions
        .iter()
        .map(|resolution| (resolution.path.clone(), resolution.choice.clone()))
        .collect::<BTreeMap<_, _>>();

    let mut pending_uploads = plan.upload.clone();

    if !plan.conflicts.is_empty() {
        progress.report(ProgressEvent::Phase(SyncPhase::ResolvingConflicts));
    }

    for conflict in &plan.conflicts {
        let choice = resolution_map
            .get(&conflict.path)
            .ok_or_else(|| SyncEngineError::MissingResolution(conflict.path.clone()))?;

        match choice {
            ConflictResolutionChoice::KeepLocal => {
                pending_uploads.push(conflict_to_upload(conflict));
            }
            ConflictResolutionChoice::KeepRemote => {
                progress.report(ProgressEvent::FileStarted {
                    path: conflict.path.clone(),
                    kind: SyncActionKind::Download,
                    index: 0,
                    total: 1,
                });
                let bytes = apply_keep_remote(local_root, &keys, &client, conflict).await?;
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
                    write_conflict_copy(local_root, &keys, &client, conflict).await?;
                progress.report(ProgressEvent::FileCompleted {
                    path: conflict.path.clone(),
                    bytes,
                });
                pending_uploads.push(conflict_to_upload(conflict));
                pending_uploads.push(build_upload_action_for_path(
                    local_root,
                    &duplicate_path,
                    &keys,
                )?);
            }
        }
    }

    progress.report(ProgressEvent::Phase(SyncPhase::Uploading));
    let total = pending_uploads.len();
    let mut late_conflicts = Vec::new();
    let mut upload_failures = Vec::new();
    let mut successful_uploads = 0usize;
    for (index, action) in pending_uploads.iter().enumerate() {
        progress.report(ProgressEvent::FileStarted {
            path: action.path.clone(),
            kind: action.kind.clone(),
            index,
            total,
        });
        match apply_upload(local_root, &keys, &client, action).await {
            Ok(()) => {
                successful_uploads += 1;
                let bytes = action.local.as_ref().map(|entry| entry.size).unwrap_or(0);
                progress.report(ProgressEvent::FileCompleted {
                    path: action.path.clone(),
                    bytes,
                });
            }
            Err(SyncEngineError::Api(ApiError::Conflict { path, conflict })) => {
                if let Some(remote) = conflict.current {
                    let local = action.local.clone().unwrap_or_else(|| FileEntry {
                        hash: String::new(),
                        modified: 0,
                        size: 0,
                        deleted: false,
                        enc_path: String::new(),
                    });
                    late_conflicts.push(Conflict { path, local, remote });
                }
            }
            Err(error) => {
                let fatal = is_fatal_sync_error(&error);
                let message = error.to_string();
                progress.report(ProgressEvent::FileFailed {
                    path: action.path.clone(),
                    error: message.clone(),
                });
                upload_failures.push(SyncFailure {
                    path: action.path.clone(),
                    kind: action.kind.clone(),
                    error: message,
                    fatal,
                });
                if fatal {
                    break;
                }
            }
        }
    }

    // Carry download-side failures from prepare into the final result.
    let mut failures = plan.failures.clone();
    failures.extend(upload_failures);

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
        uploaded: successful_uploads,
        downloaded,
        failed: failures.len(),
    });

    if !late_conflicts.is_empty() {
        // Conflicts need the user's input before the manifest can advance.
        return Ok(SyncResult {
            upload: pending_uploads,
            download: plan.download.clone(),
            conflicts: late_conflicts,
            failures,
        });
    }

    // No late conflicts: checkpoint the server manifest so local state advances
    // past every file that did transfer (the resume point). Skip the re-fetch
    // when a fatal error already proved the network is gone.
    if !failures.iter().any(|failure| failure.fatal) {
        match client.get_manifest(&keys).await {
            Ok(remote_manifest) => {
                save_manifest_to_disk(&sync_manifest_path(local_root), &remote_manifest)?;
            }
            Err(error) => {
                failures.push(SyncFailure {
                    path: String::new(),
                    kind: SyncActionKind::Upload,
                    error: error.to_string(),
                    fatal: true,
                });
            }
        }
    }

    Ok(SyncResult {
        upload: pending_uploads,
        download: plan.download.clone(),
        conflicts: Vec::new(),
        failures,
    })
}

fn build_working_manifest(
    local_root: &Path,
    previous_manifest: &Manifest,
    keys: &CryptoKeys,
) -> Result<Manifest, SyncEngineError> {
    let mut current = build_manifest_from_dir(local_root, keys)?;
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
    for (index, action) in downloads.iter().enumerate() {
        match action.kind {
            SyncActionKind::Download => {
                progress.report(ProgressEvent::FileStarted {
                    path: action.path.clone(),
                    kind: SyncActionKind::Download,
                    index,
                    total,
                });
                let outcome = async {
                    let blob = client.get_file(&action.path, keys).await?;
                    let plaintext = decrypt(&keys.content_enc, &blob)?;
                    write_local_file(local_root, &action.path, &plaintext)?;
                    Ok::<usize, SyncEngineError>(plaintext.len())
                }
                .await;
                match outcome {
                    Ok(len) => progress.report(ProgressEvent::FileCompleted {
                        path: action.path.clone(),
                        bytes: len as u64,
                    }),
                    Err(error) => {
                        let fatal = is_fatal_sync_error(&error);
                        let message = error.to_string();
                        progress.report(ProgressEvent::FileFailed {
                            path: action.path.clone(),
                            error: message.clone(),
                        });
                        failures.push(SyncFailure {
                            path: action.path.clone(),
                            kind: SyncActionKind::Download,
                            error: message,
                            fatal,
                        });
                        if fatal {
                            break;
                        }
                    }
                }
            }
            SyncActionKind::DeleteLocal => {
                if let Err(error) = delete_local_file(local_root, &action.path) {
                    failures.push(SyncFailure {
                        path: action.path.clone(),
                        kind: SyncActionKind::DeleteLocal,
                        error: error.to_string(),
                        fatal: false,
                    });
                }
            }
            _ => {}
        }
    }

    failures
}

/// A fatal error is systemic (network down, auth failure, server error): it
/// will likely strike every remaining file too, so the batch stops. Per-file
/// errors (a too-large 413, a missing 404, a local crypto/path failure) leave
/// the rest of the batch viable, so the sync continues.
fn is_fatal_sync_error(error: &SyncEngineError) -> bool {
    match error {
        SyncEngineError::Api(api_error) => is_fatal_api_error(api_error),
        _ => false,
    }
}

fn is_fatal_api_error(error: &ApiError) -> bool {
    match error {
        ApiError::Http(_) => true,
        ApiError::UnexpectedStatus { status, .. } => matches!(status.as_u16(), 401 | 403 | 500..=599),
        ApiError::Crypto(_) | ApiError::Conflict { .. } => false,
    }
}

async fn apply_keep_remote(
    local_root: &Path,
    keys: &CryptoKeys,
    client: &ApiClient,
    conflict: &Conflict,
) -> Result<u64, SyncEngineError> {
    if conflict.remote.deleted {
        delete_local_file(local_root, &conflict.path)?;
        Ok(0)
    } else {
        let blob = client.get_file(&conflict.path, keys).await?;
        let plaintext = decrypt(&keys.content_enc, &blob)?;
        write_local_file(local_root, &conflict.path, &plaintext)?;
        Ok(plaintext.len() as u64)
    }
}

async fn write_conflict_copy(
    local_root: &Path,
    keys: &CryptoKeys,
    client: &ApiClient,
    conflict: &Conflict,
) -> Result<(String, u64), SyncEngineError> {
    if conflict.remote.deleted {
        return Ok((conflict_copy_path(&conflict.path), 0));
    }

    let duplicate_path = conflict_copy_path(&conflict.path);
    let blob = client.get_file(&conflict.path, keys).await?;
    let plaintext = decrypt(&keys.content_enc, &blob)?;
    write_local_file(local_root, &duplicate_path, &plaintext)?;
    Ok((duplicate_path, plaintext.len() as u64))
}

async fn apply_upload(
    local_root: &Path,
    keys: &CryptoKeys,
    client: &ApiClient,
    action: &SyncAction,
) -> Result<(), SyncEngineError> {
    match action.kind {
        SyncActionKind::Upload => {
            let path = local_root.join(&action.path);
            let plaintext = fs::read(path)?;
            let ciphertext = encrypt(&keys.content_enc, &plaintext)?;
            client
                .put_file(
                    &action.path,
                    action.remote.as_ref().map(|entry| entry.hash.as_str()),
                    action
                        .local
                        .as_ref()
                        .map(|entry| entry.hash.as_str())
                        .unwrap_or_default(),
                    ciphertext,
                    keys,
                )
                .await
                .map_err(SyncEngineError::Api)
        }
        SyncActionKind::DeleteRemote => client
            .delete_file(
                &action.path,
                action.remote.as_ref().map(|entry| entry.hash.as_str()),
                keys,
            )
            .await
            .map_err(SyncEngineError::Api),
        _ => Ok(()),
    }
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

fn write_local_file(
    local_root: &Path,
    relative_path: &str,
    contents: &[u8],
) -> Result<(), SyncEngineError> {
    let path = local_root.join(relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(())
}

fn delete_local_file(local_root: &Path, relative_path: &str) -> Result<(), SyncEngineError> {
    let path = local_root.join(relative_path);
    if !path.exists() {
        return Ok(());
    }

    fs::remove_file(&path)?;
    cleanup_empty_dirs(local_root, path.parent());
    Ok(())
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

    use filetime::{set_file_mtime, FileTime};
    use httpmock::{Method::GET, Method::PUT, MockServer};
    use tempfile::tempdir;

    use super::{
        complete_sync, conflict_copy_path, load_manifest_from_disk, prepare_sync,
        save_manifest_to_disk, sync_manifest_path,
    };
    use crate::{
        crypto::{content_hmac, derive_keys, encrypt, encrypt_path, path_token, CryptoKeys},
        progress::NoProgress,
        types::{ConflictResolution, ConflictResolutionChoice, FileEntry, Manifest, VaultConfig},
    };

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
        let enc_path = encrypt_path(&keys.path_enc, path).unwrap();
        serde_json::json!({
            token: {
                "hash": content_hmac(&keys.content_mac, content),
                "modified": modified,
                "size": content.len(),
                "deleted": deleted,
                "encPath": enc_path,
            }
        })
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
            worker_url: base_url,
            api_key: "token".to_string(),
            vault_id: "vault_123".to_string(),
            local_path,
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
        let put_mock = server
            .mock_async(move |when, then| {
                when.method(PUT)
                    .path(format!("/vaults/vault_123/files/{token}"))
                    .header_exists("x-content-hash")
                    .header_exists("x-enc-path");
                then.status(200);
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
        put_mock.assert_async().await;
        assert!(result.conflicts.is_empty());
    }

    #[tokio::test]
    async fn keep_remote_resolves_conflict_and_updates_local_file() {
        let dir = tempdir().unwrap();
        let note_path = dir.path().join("note.md");
        fs::write(&note_path, "local version").unwrap();
        set_file_mtime(&note_path, FileTime::from_unix_time(1, 0)).unwrap();
        let server = MockServer::start_async().await;
        let key = [9_u8; 32];
        let keys = derive_keys(&key);
        let encrypted = encrypt(&keys.content_enc, b"remote version").unwrap();
        let token = path_token(&keys.path_token, "note.md");
        let remote_manifest = server_manifest(&keys, "note.md", b"remote version", 1, false);

        save_manifest_to_disk(
            &sync_manifest_path(dir.path()),
            &Manifest::from([(
                "note.md".to_string(),
                FileEntry {
                    hash: content_hmac(&keys.content_mac, b"base"),
                    modified: 1,
                    size: 4,
                    deleted: false,
                    enc_path: String::new(),
                },
            )]),
        )
        .unwrap();

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
    }

    #[tokio::test]
    async fn upload_loop_continues_past_per_file_failure() {
        // a.md, b.md, c.md upload in BTreeSet (alphabetical) order. b returns
        // 413 (per-file, non-fatal): the loop records the failure and continues
        // to c. The manifest still checkpoints because no fatal occurred.
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

        server
            .mock_async(|when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&serde_json::json!({}));
            })
            .await;
        let put_a = server
            .mock_async(move |when, then| {
                when.method(PUT).path(format!("/vaults/vault_123/files/{token_a}"));
                then.status(200);
            })
            .await;
        let _put_b = server
            .mock_async(move |when, then| {
                when.method(PUT).path(format!("/vaults/vault_123/files/{token_b}"));
                then.status(413).body("file too large");
            })
            .await;
        let put_c = server
            .mock_async(move |when, then| {
                when.method(PUT).path(format!("/vaults/vault_123/files/{token_c}"));
                then.status(200);
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        assert_eq!(plan.upload.len(), 3);

        let result = complete_sync(&cfg, &key, &plan, &[], &NoProgress)
            .await
            .unwrap();

        // a and c were both attempted (loop did not stop at b).
        put_a.assert_hits_async(1).await;
        put_c.assert_hits_async(1).await;
        // Exactly one non-fatal failure, for b.md.
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].path, "b.md");
        assert!(!result.failures[0].fatal);
        assert!(result.conflicts.is_empty());
    }

    #[tokio::test]
    async fn upload_loop_stops_on_fatal_server_error() {
        // a (200), b (500 fatal) breaks the loop; c is never attempted, and the
        // manifest checkpoint is skipped because a fatal error occurred.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "aaa").unwrap();
        fs::write(dir.path().join("b.md"), "bbb").unwrap();
        fs::write(dir.path().join("c.md"), "ccc").unwrap();
        let server = MockServer::start_async().await;
        let key = [12_u8; 32];
        let keys = derive_keys(&key);
        let token_a = path_token(&keys.path_token, "a.md");
        let token_b = path_token(&keys.path_token, "b.md");
        let token_c = path_token(&keys.path_token, "c.md");

        server
            .mock_async(|when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200).json_body_obj(&serde_json::json!({}));
            })
            .await;
        let put_a = server
            .mock_async(move |when, then| {
                when.method(PUT).path(format!("/vaults/vault_123/files/{token_a}"));
                then.status(200);
            })
            .await;
        let _put_b = server
            .mock_async(move |when, then| {
                when.method(PUT).path(format!("/vaults/vault_123/files/{token_b}"));
                then.status(500).body("server error");
            })
            .await;
        let put_c = server
            .mock_async(move |when, then| {
                when.method(PUT).path(format!("/vaults/vault_123/files/{token_c}"));
                then.status(200);
            })
            .await;

        let cfg = config(server.base_url(), dir.path().display().to_string());
        let plan = prepare_sync(&cfg, &key, &NoProgress).await.unwrap();
        let result = complete_sync(&cfg, &key, &plan, &[], &NoProgress)
            .await
            .unwrap();

        put_a.assert_hits_async(1).await;
        put_c.assert_hits_async(0).await; // loop stopped at b
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].path, "b.md");
        assert!(result.failures[0].fatal);
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

        // Build a two-entry manifest by merging per-file server_manifest objects.
        let mut manifest = server_manifest(&keys, "a.md", b"file a", 1, false);
        let manifest_b = server_manifest(&keys, "b.md", b"file b", 1, false);
        if let (Some(a), Some(b)) = (manifest.as_object_mut(), manifest_b.as_object()) {
            for (k, v) in b {
                a.insert(k.clone(), v.clone());
            }
        }

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
}
