//! UniFFI bindings for ObSink on iOS (and other Swift/Kotlin hosts).
//!
//! This crate is a thin, FFI-friendly facade over `obsink-core`. The core API is
//! async; here we expose a small *synchronous* surface that blocks on an internal
//! Tokio runtime, which is far simpler to consume from Swift than async FFI. A
//! `VaultClient` object holds the derived key and the pending sync plan between
//! the `prepare` and `complete` phases, mirroring the desktop flow.

use std::{fs, path::Path, sync::{Arc, Mutex}};

use obsink_core::{
    complete_sync, decrypt, derive_key, derive_keys, prepare_sync, ApiClient, ConflictResolution,
    ConflictResolutionChoice, CreateVaultRequest, KeyBytes, ProgressEvent, ProgressSink,
    SyncActionKind, SyncFailure, SyncPlan, SyncPhase, VaultConfig, VaultSummary,
};

uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MobileError {
    #[error("{message}")]
    Sync { message: String },
    #[error("invalid key length: expected 32 bytes, got {length}")]
    InvalidKey { length: u64 },
    #[error("no pending sync; call prepare() first")]
    NoPendingSync,
}

fn sync_err(error: impl std::fmt::Display) -> MobileError {
    MobileError::Sync {
        message: error.to_string(),
    }
}

/// Connection details for one vault, supplied by the host app.
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileVaultConfig {
    pub worker_url: String,
    pub api_key: String,
    pub vault_id: String,
    pub local_path: String,
}

impl From<MobileVaultConfig> for VaultConfig {
    fn from(value: MobileVaultConfig) -> Self {
        VaultConfig {
            worker_url: value.worker_url,
            api_key: value.api_key,
            vault_id: value.vault_id,
            local_path: value.local_path,
        }
    }
}

/// How the host wants a single conflict resolved.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum MobileChoice {
    KeepLocal,
    KeepRemote,
    KeepBoth,
}

impl From<MobileChoice> for ConflictResolutionChoice {
    fn from(value: MobileChoice) -> Self {
        match value {
            MobileChoice::KeepLocal => ConflictResolutionChoice::KeepLocal,
            MobileChoice::KeepRemote => ConflictResolutionChoice::KeepRemote,
            MobileChoice::KeepBoth => ConflictResolutionChoice::KeepBoth,
        }
    }
}

/// Mirror of core `SyncActionKind` for progress/failure events.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum MobileActionKind {
    Upload,
    Download,
    DeleteLocal,
    DeleteRemote,
}

/// Mirror of core `SyncPhase`.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum MobileSyncPhase {
    Downloading,
    ResolvingConflicts,
    Uploading,
}

/// Mirror of core `ProgressEvent`, surfaced to Swift via the `ProgressListener`
/// callback during a sync.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum MobileProgressEvent {
    Phase { phase: MobileSyncPhase },
    FileStarted { path: String, kind: MobileActionKind, index: u32, total: u32 },
    FileCompleted { path: String, bytes: u64 },
    FileFailed { path: String, error: String },
    Done { uploaded: u32, downloaded: u32, failed: u32 },
}

/// A conflict the host must resolve before the sync can complete.
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileConflict {
    pub path: String,
    pub local_modified: u64,
    pub remote_modified: u64,
    pub local_size: u64,
    pub remote_size: u64,
    pub local_deleted: bool,
    pub remote_deleted: bool,
}

/// The host's resolution choice for one conflicted path.
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileResolution {
    pub path: String,
    pub choice: MobileChoice,
}

/// A transfer that failed during a sync. Mirrors core's `SyncFailure`.
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileSyncFailure {
    pub path: String,
    pub kind: MobileActionKind,
    pub error: String,
    pub fatal: bool,
}

/// Result of a sync phase. `completed` is true once changes have been pushed and
/// the local manifest saved (i.e. there were no conflicts to resolve).
#[derive(Debug, Clone, uniffi::Record)]
pub struct SyncOutcome {
    pub uploaded: u32,
    pub downloaded: u32,
    pub conflicts: Vec<MobileConflict>,
    pub failures: Vec<MobileSyncFailure>,
    pub completed: bool,
}

/// A vault the host can list/create/connect to (OBS-28).
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileVaultSummary {
    pub id: String,
    pub name: String,
    pub created: u64,
    pub max_file_size: u64,
}

impl From<VaultSummary> for MobileVaultSummary {
    fn from(v: VaultSummary) -> Self {
        Self {
            id: v.id,
            name: v.name,
            created: v.created,
            max_file_size: v.max_file_size,
        }
    }
}

/// Read-only content preview of both sides of a conflict (OBS-25).
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileConflictPreview {
    pub path: String,
    pub local_text: String,
    pub remote_text: String,
    pub local_deleted: bool,
    pub remote_deleted: bool,
}

/// A Worker-only config (no vault id / local path) for list/create calls.
fn worker_only(worker_url: String, api_key: String) -> VaultConfig {
    VaultConfig {
        worker_url,
        api_key,
        vault_id: String::new(),
        local_path: String::new(),
    }
}

/// Derive the 32-byte master key from a passphrase and vault ID (the salt).
#[uniffi::export]
pub fn derive_master_key(passphrase: String, vault_id: String) -> Result<Vec<u8>, MobileError> {
    derive_key(&passphrase, vault_id.as_bytes())
        .map(|key| key.to_vec())
        .map_err(sync_err)
}

/// List vaults reachable at a Worker (OBS-28).
#[uniffi::export]
pub fn list_vaults(worker_url: String, api_key: String) -> Result<Vec<MobileVaultSummary>, MobileError> {
    let vaults =
        block_on(ApiClient::new(worker_only(worker_url, api_key)).list_vaults()).map_err(sync_err)?;
    Ok(vaults.into_iter().map(MobileVaultSummary::from).collect())
}

/// Create a new vault at a Worker; returns its id + metadata (OBS-28).
#[uniffi::export]
pub fn create_vault(
    worker_url: String,
    api_key: String,
    name: String,
) -> Result<MobileVaultSummary, MobileError> {
    let request = CreateVaultRequest {
        name,
        max_file_size: 50 * 1024 * 1024,
    };
    let response =
        block_on(ApiClient::new(worker_only(worker_url, api_key)).create_vault(&request))
            .map_err(sync_err)?;
    Ok(response.vault.into())
}

/// Foreign-implemented receiver of sync progress events. Swift passes an
/// object implementing this trait to `VaultClient::sync`/`prepare`/`complete`;
/// Rust calls `on_progress` from inside the (blocking) sync call.
#[uniffi::export(callback_interface)]
pub trait ProgressListener: Send + Sync {
    fn on_progress(&self, event: MobileProgressEvent);
}

/// Adapts a Swift-supplied `ProgressListener` to the core `ProgressSink` trait
/// so the sync engine can emit events without knowing about UniFFI.
struct ListenerSink(Arc<dyn ProgressListener>);

impl ProgressSink for ListenerSink {
    fn report(&self, event: ProgressEvent) {
        self.0.on_progress(to_mobile_event(event));
    }
}

fn to_mobile_kind(kind: SyncActionKind) -> MobileActionKind {
    match kind {
        SyncActionKind::Upload => MobileActionKind::Upload,
        SyncActionKind::Download => MobileActionKind::Download,
        SyncActionKind::DeleteLocal => MobileActionKind::DeleteLocal,
        SyncActionKind::DeleteRemote => MobileActionKind::DeleteRemote,
    }
}

fn to_mobile_phase(phase: SyncPhase) -> MobileSyncPhase {
    match phase {
        SyncPhase::Downloading => MobileSyncPhase::Downloading,
        SyncPhase::ResolvingConflicts => MobileSyncPhase::ResolvingConflicts,
        SyncPhase::Uploading => MobileSyncPhase::Uploading,
    }
}

fn to_mobile_event(event: ProgressEvent) -> MobileProgressEvent {
    match event {
        ProgressEvent::Phase(phase) => MobileProgressEvent::Phase {
            phase: to_mobile_phase(phase),
        },
        ProgressEvent::FileStarted { path, kind, index, total } => MobileProgressEvent::FileStarted {
            path,
            kind: to_mobile_kind(kind),
            index: index as u32,
            total: total as u32,
        },
        ProgressEvent::FileCompleted { path, bytes } => MobileProgressEvent::FileCompleted { path, bytes },
        ProgressEvent::FileFailed { path, error } => MobileProgressEvent::FileFailed { path, error },
        ProgressEvent::Done { uploaded, downloaded, failed } => {
            MobileProgressEvent::Done {
                uploaded: uploaded as u32,
                downloaded: downloaded as u32,
                failed: failed as u32,
            }
        }
    }
}

fn to_mobile_failures(failures: &[SyncFailure]) -> Vec<MobileSyncFailure> {
    failures
        .iter()
        .map(|failure| MobileSyncFailure {
            path: failure.path.clone(),
            kind: to_mobile_kind(failure.kind.clone()),
            error: failure.error.clone(),
            fatal: failure.fatal,
        })
        .collect()
}

/// Stateful sync client for one vault. Holds the derived key and the pending
/// plan between `prepare` and `complete`.
#[derive(uniffi::Object)]
pub struct VaultClient {
    config: VaultConfig,
    key: KeyBytes,
    pending: Mutex<Option<SyncPlan>>,
}

#[uniffi::export]
impl VaultClient {
    /// Build a client from config and a 32-byte master key (see `derive_master_key`).
    #[uniffi::constructor]
    pub fn new(
        config: MobileVaultConfig,
        key: Vec<u8>,
    ) -> Result<std::sync::Arc<Self>, MobileError> {
        let key: KeyBytes = key
            .as_slice()
            .try_into()
            .map_err(|_| MobileError::InvalidKey {
                length: key.len() as u64,
            })?;
        Ok(std::sync::Arc::new(Self {
            config: config.into(),
            key,
            pending: Mutex::new(None),
        }))
    }

    /// Pull the remote manifest, apply downloads, and report pending uploads and
    /// conflicts. Stores the plan so `complete` can finish the cycle. Progress
    /// events flow to `listener` during the call.
    pub fn prepare(&self, listener: Box<dyn ProgressListener>) -> Result<SyncOutcome, MobileError> {
        self.prepare_with(Arc::from(listener))
    }

    /// Finish the sync from the stored plan, applying the host's conflict
    /// resolutions and pushing local changes. Progress events flow to
    /// `listener` during the call.
    pub fn complete(
        &self,
        resolutions: Vec<MobileResolution>,
        listener: Box<dyn ProgressListener>,
    ) -> Result<SyncOutcome, MobileError> {
        self.complete_with(resolutions, Arc::from(listener))
    }

    /// Convenience: prepare and, if there are no conflicts, complete in one call.
    /// If conflicts exist, returns them (completed = false) for the host to
    /// resolve and then call `complete`. Progress events flow to `listener`.
    pub fn sync(&self, listener: Box<dyn ProgressListener>) -> Result<SyncOutcome, MobileError> {
        let listener: Arc<dyn ProgressListener> = Arc::from(listener);
        let outcome = self.prepare_with(listener.clone())?;
        if outcome.conflicts.is_empty() {
            return self.complete_with(Vec::new(), listener);
        }
        Ok(outcome)
    }

    /// Decrypt/read both sides of a pending conflict for the UI preview (OBS-25).
    /// Requires a pending plan from `prepare`/`sync`.
    pub fn conflict_preview(&self, path: String) -> Result<MobileConflictPreview, MobileError> {
        let conflict = {
            let guard = self.pending.lock().expect("pending lock");
            let plan = guard.as_ref().ok_or(MobileError::NoPendingSync)?;
            plan.conflicts
                .iter()
                .find(|conflict| conflict.path == path)
                .cloned()
                .ok_or_else(|| MobileError::Sync {
                    message: format!("no pending conflict for {path}"),
                })?
        };
        let keys = derive_keys(&self.key);

        let local_text = if conflict.local.deleted {
            String::new()
        } else {
            fs::read_to_string(Path::new(&self.config.local_path).join(&path)).unwrap_or_default()
        };

        let (remote_text, remote_deleted) = if conflict.remote.deleted {
            (String::new(), true)
        } else {
            let blob =
                block_on(ApiClient::new(self.config.clone()).get_file(&path, &keys)).map_err(sync_err)?;
            let bytes = decrypt(&keys.content_enc, &blob).map_err(sync_err)?;
            (String::from_utf8_lossy(&bytes).into_owned(), false)
        };

        Ok(MobileConflictPreview {
            path,
            local_text,
            remote_text,
            local_deleted: conflict.local.deleted,
            remote_deleted,
         })
     }
}

/// Internal helpers (not FFI-exported) taking `Arc<dyn ProgressListener>` so
/// `sync` can share one listener across the prepare + complete phases. They live
/// outside `#[uniffi::export] impl` because UniFFI would otherwise try (and fail)
/// to give `Arc<dyn ProgressListener>` an FFI converter.
impl VaultClient {
    fn prepare_with(
        &self,
        listener: Arc<dyn ProgressListener>,
    ) -> Result<SyncOutcome, MobileError> {
        let sink = ListenerSink(listener);
        let plan = block_on(prepare_sync(&self.config, &self.key, &sink)).map_err(sync_err)?;
        let outcome = SyncOutcome {
            uploaded: plan.upload.len() as u32,
            downloaded: plan.download.len() as u32,
            conflicts: plan.conflicts.iter().map(to_mobile_conflict).collect(),
            failures: to_mobile_failures(&plan.failures),
            completed: false,
        };
        *self.pending.lock().expect("pending lock") = Some(plan);
        Ok(outcome)
    }

    fn complete_with(
        &self,
        resolutions: Vec<MobileResolution>,
        listener: Arc<dyn ProgressListener>,
    ) -> Result<SyncOutcome, MobileError> {
        let plan = self
            .pending
            .lock()
            .expect("pending lock")
            .take()
            .ok_or(MobileError::NoPendingSync)?;
        let resolutions: Vec<ConflictResolution> = resolutions
            .into_iter()
            .map(|resolution| ConflictResolution {
                path: resolution.path,
                choice: resolution.choice.into(),
            })
            .collect();
        let sink = ListenerSink(listener);
        let result = block_on(complete_sync(
            &self.config,
            &self.key,
            &plan,
            &resolutions,
            &sink,
        ))
        .map_err(sync_err)?;
        Ok(SyncOutcome {
            uploaded: result.upload.len() as u32,
            downloaded: result.download.len() as u32,
            conflicts: result.conflicts.iter().map(to_mobile_conflict).collect(),
            failures: to_mobile_failures(&result.failures),
            completed: result.conflicts.is_empty(),
        })
    }
}

fn to_mobile_conflict(conflict: &obsink_core::Conflict) -> MobileConflict {
    MobileConflict {
        path: conflict.path.clone(),
        local_modified: conflict.local.modified,
        remote_modified: conflict.remote.modified,
        local_size: conflict.local.size,
        remote_size: conflict.remote.size,
        local_deleted: conflict.local.deleted,
        remote_deleted: conflict.remote.deleted,
    }
}

/// Run a future to completion on a fresh single-threaded Tokio runtime. Mobile
/// sync is infrequent, so per-call runtime construction is acceptable and keeps
/// the FFI surface synchronous.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build Tokio runtime")
        .block_on(future)
}
