//! UniFFI bindings for ObSink on iOS (and other Swift/Kotlin hosts).
//!
//! This crate is a thin, FFI-friendly facade over `obsink-core`. The core API is
//! async; here we expose a small *synchronous* surface that blocks on an internal
//! Tokio runtime, which is far simpler to consume from Swift than async FFI. A
//! `VaultClient` object holds the derived key and the pending sync plan between
//! the `prepare` and `complete` phases, mirroring the desktop flow.

use std::{fs, path::Path, sync::Mutex};

use obsink_core::{
    complete_sync, decrypt, derive_key, derive_keys, prepare_sync, ApiClient, ConflictResolution,
    ConflictResolutionChoice, CreateVaultRequest, KeyBytes, SyncPlan, VaultConfig, VaultSummary,
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

/// Result of a sync phase. `completed` is true once changes have been pushed and
/// the local manifest saved (i.e. there were no conflicts to resolve).
#[derive(Debug, Clone, uniffi::Record)]
pub struct SyncOutcome {
    pub uploaded: u32,
    pub downloaded: u32,
    pub conflicts: Vec<MobileConflict>,
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
    /// conflicts. Stores the plan so `complete` can finish the cycle.
    pub fn prepare(&self) -> Result<SyncOutcome, MobileError> {
        let plan = block_on(prepare_sync(&self.config, &self.key)).map_err(sync_err)?;
        let outcome = SyncOutcome {
            uploaded: plan.upload.len() as u32,
            downloaded: plan.download.len() as u32,
            conflicts: plan.conflicts.iter().map(to_mobile_conflict).collect(),
            completed: false,
        };
        *self.pending.lock().expect("pending lock") = Some(plan);
        Ok(outcome)
    }

    /// Finish the sync from the stored plan, applying the host's conflict
    /// resolutions and pushing local changes.
    pub fn complete(&self, resolutions: Vec<MobileResolution>) -> Result<SyncOutcome, MobileError> {
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
        let result = block_on(complete_sync(&self.config, &self.key, &plan, &resolutions))
            .map_err(sync_err)?;
        Ok(SyncOutcome {
            uploaded: result.upload.len() as u32,
            downloaded: result.download.len() as u32,
            conflicts: result.conflicts.iter().map(to_mobile_conflict).collect(),
            completed: result.conflicts.is_empty(),
        })
    }

    /// Convenience: prepare and, if there are no conflicts, complete in one call.
    /// If conflicts exist, returns them (completed = false) for the host to
    /// resolve and then call `complete`.
    pub fn sync(&self) -> Result<SyncOutcome, MobileError> {
        let outcome = self.prepare()?;
        if outcome.conflicts.is_empty() {
            return self.complete(Vec::new());
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
