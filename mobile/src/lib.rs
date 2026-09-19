//! UniFFI bindings for ObSink on iOS (and other Swift/Kotlin hosts).
//!
//! This crate is a thin, FFI-friendly facade over `obsink-core`. The core API is
//! async; here we expose a small *synchronous* surface that blocks on an internal
//! Tokio runtime, which is far simpler to consume from Swift than async FFI. A
//! `VaultClient` object holds the derived key and the pending sync plan between
//! the `prepare` and `complete` phases, mirroring the desktop flow.

use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

use obsink_core::{
    build_working_manifest_for_path, complete_sync, decrypt, derive_key, derive_keys,
    diff_local_and_remote, fetch_remote_manifest, normalize_server_url, prepare_sync, ApiClient,
    AuthClient, ConflictResolution, ConflictResolutionChoice, CreateVaultRequest, KeyBytes,
    ProgressEvent, ProgressSink, SyncActionKind, SyncFailure, SyncPhase, SyncPlan, VaultConfig,
    VaultSummary,
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
    pub server_url: String,
    pub api_key: String,
    pub vault_id: String,
    pub local_path: String,
}

impl From<MobileVaultConfig> for VaultConfig {
    fn from(value: MobileVaultConfig) -> Self {
        VaultConfig {
            server_url: value.server_url,
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
    Phase {
        phase: MobileSyncPhase,
    },
    FileStarted {
        path: String,
        kind: MobileActionKind,
        index: u32,
        total: u32,
    },
    FileCompleted {
        path: String,
        bytes: u64,
    },
    FileFailed {
        path: String,
        error: String,
    },
    Done {
        uploaded: u32,
        downloaded: u32,
        failed: u32,
    },
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

/// Local-vs-remote diff counts without transferring anything — the data source
/// for the stale-vault warning (spec §3.4, OBS-33). Mirrors desktop `get_status`.
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileVaultStatus {
    pub pending_uploads: u32,
    pub pending_downloads: u32,
    pub pending_conflicts: u32,
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

/// A server-only config (no vault id / local path) for list/create calls.
fn server_only(server_url: String, api_key: String) -> VaultConfig {
    VaultConfig {
        server_url,
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

/// List vaults reachable at a server (OBS-28).
#[uniffi::export]
pub fn list_vaults(
    server_url: String,
    api_key: String,
) -> Result<Vec<MobileVaultSummary>, MobileError> {
    let vaults = block_on(ApiClient::new(server_only(server_url, api_key)).list_vaults())
        .map_err(sync_err)?;
    Ok(vaults.into_iter().map(MobileVaultSummary::from).collect())
}

/// Create a new vault at a server; returns its id + metadata (OBS-28).
#[uniffi::export]
pub fn create_vault(
    server_url: String,
    api_key: String,
    name: String,
) -> Result<MobileVaultSummary, MobileError> {
    let request = CreateVaultRequest {
        name,
        max_file_size: 50 * 1024 * 1024,
    };
    let response =
        block_on(ApiClient::new(server_only(server_url, api_key)).create_vault(&request))
            .map_err(sync_err)?;
    Ok(response.vault.into())
}

// --- Accounts ---------------------------------------------------------------

/// Canonical form of a server URL (the keychain account for its bearer).
#[uniffi::export]
pub fn canonical_server_url(url: String) -> String {
    normalize_server_url(&url)
}

/// Which sign-in methods a server offers (`GET /`).
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileCapabilities {
    pub email: bool,
    pub apple: bool,
    /// New accounts need an invite code once the server has any user.
    pub invite_required: bool,
}

/// A signed-in session: `token` is the bearer to store in the Keychain.
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileSession {
    pub token: String,
    pub session_id: String,
    pub user_id: String,
    pub email: Option<String>,
}

/// `GET /auth/me` for the signed-in account.
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileAccount {
    pub user_id: String,
    pub email: Option<String>,
    pub devices: Vec<MobileDevice>,
    pub usage: Option<MobileUsage>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileUsage {
    pub total_bytes: u64,
    pub max_vault_bytes: Option<u64>,
    pub max_vaults: Option<u32>,
    pub vaults: Vec<MobileVaultUsage>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileVaultUsage {
    pub id: String,
    pub bytes: u64,
}

/// An invite code for someone else to create an account.
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileInvite {
    pub code: String,
    pub expires: u64,
    pub status: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileDevice {
    pub session_id: String,
    pub device_name: String,
    pub created: u64,
    pub current: bool,
}

#[uniffi::export]
pub fn auth_capabilities(server_url: String) -> Result<MobileCapabilities, MobileError> {
    let caps = block_on(AuthClient::new(&server_url).capabilities()).map_err(sync_err)?;
    Ok(MobileCapabilities {
        email: caps.auth.email,
        apple: caps.auth.apple,
        invite_required: caps.invite_required,
    })
}

/// Send a one-time code to `email`. Returns the code only against a dev
/// server (`AUTH_DEV_RETURN_CODE=1`), otherwise `None`.
#[uniffi::export]
pub fn auth_email_start(server_url: String, email: String) -> Result<Option<String>, MobileError> {
    let result = block_on(AuthClient::new(&server_url).email_start(&email)).map_err(sync_err)?;
    Ok(result.code)
}

#[uniffi::export]
pub fn auth_email_verify(
    server_url: String,
    email: String,
    code: String,
    device_name: String,
    invite_code: Option<String>,
) -> Result<MobileSession, MobileError> {
    let session = block_on(AuthClient::new(&server_url).email_verify(
        &email,
        &code,
        &device_name,
        clean_invite(invite_code.as_deref()),
    ))
    .map_err(sync_err)?;
    Ok(to_mobile_session(session))
}

fn clean_invite(code: Option<&str>) -> Option<&str> {
    code.map(str::trim).filter(|code| !code.is_empty())
}

/// Exchange an Apple identity token (from `ASAuthorizationAppleIDCredential`)
/// for a session. Pass the credential's email when Apple supplies it (first
/// authorization only).
#[uniffi::export]
pub fn auth_apple(
    server_url: String,
    identity_token: String,
    device_name: String,
    email: Option<String>,
    invite_code: Option<String>,
) -> Result<MobileSession, MobileError> {
    let session = block_on(AuthClient::new(&server_url).apple_sign_in(
        &identity_token,
        &device_name,
        email.as_deref(),
        clean_invite(invite_code.as_deref()),
    ))
    .map_err(sync_err)?;
    Ok(to_mobile_session(session))
}

#[uniffi::export]
pub fn auth_me(server_url: String, token: String) -> Result<MobileAccount, MobileError> {
    let me = block_on(AuthClient::new(&server_url).me(&token)).map_err(sync_err)?;
    let user = me
        .user
        .ok_or_else(|| sync_err("this credential is the operator API key, not an account"))?;
    Ok(MobileAccount {
        user_id: user.id,
        email: user.email,
        devices: me
            .sessions
            .into_iter()
            .map(|session| MobileDevice {
                session_id: session.id,
                device_name: session.device_name,
                created: session.created,
                current: session.current,
            })
            .collect(),
        usage: me.usage.map(|usage| MobileUsage {
            total_bytes: usage.total_bytes,
            max_vault_bytes: usage.max_vault_bytes,
            max_vaults: usage.max_vaults,
            vaults: usage
                .vaults
                .into_iter()
                .map(|vault| MobileVaultUsage {
                    id: vault.id,
                    bytes: vault.bytes,
                })
                .collect(),
        }),
    })
}

/// Mint an invite code so someone else can create an account.
#[uniffi::export]
pub fn auth_create_invite(server_url: String, token: String) -> Result<MobileInvite, MobileError> {
    let invite = block_on(AuthClient::new(&server_url).create_invite(&token)).map_err(sync_err)?;
    Ok(MobileInvite {
        code: invite.code,
        expires: invite.expires,
        status: invite.status,
    })
}

/// Invites this account has minted, newest first.
#[uniffi::export]
pub fn auth_list_invites(
    server_url: String,
    token: String,
) -> Result<Vec<MobileInvite>, MobileError> {
    let invites = block_on(AuthClient::new(&server_url).list_invites(&token)).map_err(sync_err)?;
    Ok(invites
        .into_iter()
        .map(|invite| MobileInvite {
            code: invite.code,
            expires: invite.expires,
            status: invite.status,
        })
        .collect())
}

/// Revoke the current session (sign out this device).
#[uniffi::export]
pub fn auth_logout(server_url: String, token: String) -> Result<(), MobileError> {
    block_on(AuthClient::new(&server_url).logout(&token)).map_err(sync_err)
}

/// Delete the account and every vault it owns. Irreversible.
#[uniffi::export]
pub fn auth_delete_account(server_url: String, token: String) -> Result<(), MobileError> {
    block_on(AuthClient::new(&server_url).delete_account(&token)).map_err(sync_err)
}

/// Delete a vault (and its server-side blobs) the bearer owns.
#[uniffi::export]
pub fn delete_vault(
    server_url: String,
    api_key: String,
    vault_id: String,
) -> Result<(), MobileError> {
    let config = VaultConfig {
        server_url,
        api_key,
        vault_id,
        local_path: String::new(),
    };
    block_on(ApiClient::new(config).delete_vault()).map_err(sync_err)
}

fn to_mobile_session(session: obsink_core::Session) -> MobileSession {
    MobileSession {
        token: session.token,
        session_id: session.session.id,
        user_id: session.user.id,
        email: session.user.email,
    }
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
        ProgressEvent::FileStarted {
            path,
            kind,
            index,
            total,
        } => MobileProgressEvent::FileStarted {
            path,
            kind: to_mobile_kind(kind),
            index: index as u32,
            total: total as u32,
        },
        ProgressEvent::FileCompleted { path, bytes } => {
            MobileProgressEvent::FileCompleted { path, bytes }
        }
        ProgressEvent::FileFailed { path, error } => {
            MobileProgressEvent::FileFailed { path, error }
        }
        ProgressEvent::Done {
            uploaded,
            downloaded,
            failed,
        } => MobileProgressEvent::Done {
            uploaded: uploaded as u32,
            downloaded: downloaded as u32,
            failed: failed as u32,
        },
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
            let blob = block_on(ApiClient::new(self.config.clone()).get_file(&path, &keys))
                .map_err(sync_err)?;
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

    /// Diff the local working manifest against the remote one without touching
    /// any files. Powers the stale-vault warning on open (spec §3.4, OBS-33).
    pub fn vault_status(&self) -> Result<MobileVaultStatus, MobileError> {
        let keys = derive_keys(&self.key);
        let local = build_working_manifest_for_path(Path::new(&self.config.local_path), &keys)
            .map_err(sync_err)?;
        let remote = block_on(fetch_remote_manifest(
            &ApiClient::new(self.config.clone()),
            Path::new(&self.config.local_path),
            &keys,
        ))
        .map_err(sync_err)?;
        let diff = diff_local_and_remote(&local, &remote);
        Ok(MobileVaultStatus {
            pending_uploads: diff.upload.len() as u32,
            pending_downloads: diff.download.len() as u32,
            pending_conflicts: diff.conflicts.len() as u32,
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
