//! UniFFI bindings for ObSink on iOS (and other Swift/Kotlin hosts).
//!
//! This crate is a thin, FFI-friendly facade over `obsink-core`. The core API is
//! async; here we expose a small *synchronous* surface that blocks on an internal
//! Tokio runtime, which is far simpler to consume from Swift than async FFI. A
//! `VaultClient` object holds the vault key and the pending sync plan between
//! the `prepare` and `complete` phases, mirroring the desktop flow.
//!
//! Wire format v3 (spec §4, §6, §12): the account signs in as a device, sets
//! or enters one passphrase that unlocks the account key, and every vault key
//! is random, wrapped under that account key. The host keeps the keys in its
//! keychain and passes them in as bytes.

use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

use obsink_core::{
    complete_sync, create_account_key, decode_base64, decrypt, derive_keys, diff_local_and_remote,
    encode_base64, fetch_remote_manifest, load_local_state, new_key, new_vault_id,
    normalize_server_url, prepare_sync, rewrap_account_key, ApiClient, ApiError, AuthClient,
    AuthError, ConflictResolution, ConflictResolutionChoice, CreateVaultRequest, CryptoError,
    Device, DevicePlatform, KeyBytes, ProgressEvent, ProgressSink, SetKeysOutcome, SyncActionKind,
    SyncEngineError, SyncFailure, SyncPhase, SyncPlan, VaultConfig, VaultSummary, PROTOCOL_VERSION,
};

/// Spec §6.1: the wrapped account key is the new exposure, so the passphrase
/// has a floor (the same one the desktop and the shared UI enforce).
const MIN_PASSPHRASE_CHARS: usize = 12;

uniffi::setup_scaffolding!();

/// Errors crossing the FFI. The host decides what to show from the variant,
/// not from the text: `Unauthorized` opens sign-in, `Network` is retryable,
/// `Server` carries the status for the two 403s that need a follow-up (invite
/// code, Apple email verification).
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MobileError {
    /// Local failure (crypto, io, json) or a message with no better home.
    #[error("{message}")]
    Sync { message: String },
    #[error("invalid key length: expected 32 bytes, got {length}")]
    InvalidKey { length: u64 },
    #[error("no pending sync; call prepare() first")]
    NoPendingSync,
    /// The bearer was rejected (revoked, expired, account deleted).
    #[error("{message}")]
    Unauthorized { message: String },
    /// No response: DNS, refused, timeout, TLS.
    #[error("{message}")]
    Network { message: String },
    /// Any other non-2xx, with the server's `error` text.
    #[error("server returned {status}: {message}")]
    Server { status: u16, message: String },
    /// The server speaks another wire format (spec §15.5): the host shows
    /// `Update ObSink` and nothing else runs.
    #[error("this ObSink is too old for the server (protocol {server}, client {client})")]
    ProtocolMismatch { server: u32, client: u32 },
}

impl MobileError {
    fn sync(error: impl std::fmt::Display) -> Self {
        MobileError::Sync {
            message: error.to_string(),
        }
    }
}

/// What a call carries, which decides what a 401 means: on a `Bearer`
/// call the session is gone; on a `SignIn` call the code was wrong, which
/// is an ordinary server answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallKind {
    SignIn,
    Bearer,
}

fn map_status(status: u16, message: String, kind: CallKind) -> MobileError {
    if status == 401 && kind == CallKind::Bearer {
        MobileError::Unauthorized {
            message: "unauthorized: sign in again".into(),
        }
    } else {
        MobileError::Server { status, message }
    }
}

fn from_auth(error: AuthError, kind: CallKind) -> MobileError {
    match error {
        AuthError::Http(error) => MobileError::Network {
            message: error.to_string(),
        },
        AuthError::Server { status, message } => map_status(status.as_u16(), message, kind),
        AuthError::ProtocolMismatch { server, client } => {
            MobileError::ProtocolMismatch { server, client }
        }
    }
}

/// Every `ApiClient` call carries the bearer.
fn from_api(error: ApiError) -> MobileError {
    match error {
        ApiError::Http(error) => MobileError::Network {
            message: error.to_string(),
        },
        ApiError::Unauthorized => map_status(401, String::new(), CallKind::Bearer),
        ApiError::UnexpectedStatus { status, body } => {
            map_status(status.as_u16(), parse_error_body(&body), CallKind::Bearer)
        }
        other @ (ApiError::Crypto(_) | ApiError::Conflict { .. }) => MobileError::sync(other),
    }
}

fn from_sync(error: SyncEngineError) -> MobileError {
    match error {
        SyncEngineError::Api(error) => from_api(error),
        other => MobileError::sync(other),
    }
}

impl From<CryptoError> for MobileError {
    fn from(error: CryptoError) -> Self {
        MobileError::sync(error)
    }
}

/// Server error bodies are `{ "error": "<text>" }`; keep the text alone.
fn parse_error_body(body: &str) -> String {
    #[derive(serde::Deserialize)]
    struct Body {
        error: String,
    }
    serde_json::from_str::<Body>(body)
        .map(|body| body.error)
        .unwrap_or_else(|_| body.to_string())
}

/// Connection details for one vault, supplied by the host app. `device_id`
/// is this phone's stable id (spec §4.1); with it set, every checkpoint
/// reports its revision to the server. `Debug` redacts `bearer`.
#[derive(Clone, uniffi::Record)]
pub struct MobileVaultConfig {
    pub server_url: String,
    pub bearer: String,
    pub vault_id: String,
    pub local_path: String,
    pub device_id: Option<String>,
}

impl std::fmt::Debug for MobileVaultConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MobileVaultConfig")
            .field("server_url", &self.server_url)
            .field("bearer", &"..")
            .field("vault_id", &self.vault_id)
            .field("local_path", &self.local_path)
            .field("device_id", &self.device_id)
            .finish()
    }
}

impl From<MobileVaultConfig> for VaultConfig {
    fn from(value: MobileVaultConfig) -> Self {
        VaultConfig {
            server_url: value.server_url,
            bearer: value.bearer,
            vault_id: value.vault_id,
            local_path: value.local_path,
            device_id: value.device_id,
            ignore: Vec::new(),
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
    /// The files moved but the checkpoint failed; the next sync redoes the
    /// bookkeeping. Shown as its own row, not as a file failure.
    pub checkpoint_error: Option<String>,
}

/// A device that holds a vault, as `GET /vaults` reports it (spec §15.2).
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileVaultDevice {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub last_synced: Option<u64>,
    pub last_revision: Option<u64>,
}

/// One vault of the account as `GET /vaults` lists it (spec §4.3): the
/// server's revision and bytes, this account's wrapped key, the devices
/// that hold it.
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileVaultSummary {
    pub id: String,
    pub name: String,
    pub created: u64,
    pub max_file_size: u64,
    pub revision: u64,
    pub last_write: u64,
    pub bytes: u64,
    /// The vault key wrapped for this account (base64); `None` for a vault
    /// created before the account had a passphrase.
    pub wrapped_key: Option<String>,
    pub devices: Vec<MobileVaultDevice>,
}

impl From<VaultSummary> for MobileVaultSummary {
    fn from(v: VaultSummary) -> Self {
        Self {
            id: v.id,
            name: v.name,
            created: v.created,
            max_file_size: v.max_file_size,
            revision: v.revision,
            last_write: v.last_write,
            bytes: v.bytes,
            wrapped_key: v.wrapped_key,
            devices: v
                .devices
                .into_iter()
                .map(|device| MobileVaultDevice {
                    id: device.id,
                    name: device.name,
                    platform: device.platform,
                    last_synced: device.last_synced,
                    last_revision: device.last_revision,
                })
                .collect(),
        }
    }
}

/// A vault this device created: its listing and the raw vault key for the
/// host's keychain.
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileCreatedVault {
    pub vault: MobileVaultSummary,
    pub key: Vec<u8>,
}

/// One archived version of a file (spec §8.2).
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileVersion {
    /// What `fetch_version` takes (`<unix>[-n]`).
    pub name: String,
    pub ts: u64,
    /// The sealed size on the server, a few bytes over the file.
    pub size: u64,
}

/// One tombstone with its real path (spec §9.3).
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileTrashEntry {
    pub path: String,
    pub hash: String,
    pub size: u64,
    pub deleted_at: u64,
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

/// A server-only config (no vault id / local path) for account-level calls.
fn server_only(server_url: String, bearer: String, device_id: Option<String>) -> VaultConfig {
    VaultConfig {
        server_url,
        bearer,
        vault_id: String::new(),
        local_path: String::new(),
        device_id,
        ignore: Vec::new(),
    }
}

/// The wire format this build speaks (spec §15.5).
#[uniffi::export]
pub fn mobile_protocol_version() -> u32 {
    PROTOCOL_VERSION
}

/// A fresh device id for this phone (`dev_<32 hex>`), minted once and kept
/// in the keychain by the host (spec §4.1).
#[uniffi::export]
pub fn new_device_id() -> String {
    format!("dev_{}", hex(&new_key()[..16]))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A fresh random vault key (32 bytes).
#[uniffi::export]
pub fn new_vault_key() -> Vec<u8> {
    new_key().to_vec()
}

/// Wrap a vault key for an account (base64), the `wrapped_key` of a create.
#[uniffi::export]
pub fn wrap_vault_key_for(
    account_key: Vec<u8>,
    vault_key: Vec<u8>,
    vault_id: String,
) -> Result<String, MobileError> {
    let wrapped =
        obsink_core::wrap_vault_key(&key_bytes(account_key)?, &key_bytes(vault_key)?, &vault_id)?;
    Ok(encode_base64(&wrapped))
}

/// Recover a vault key from the member blob `list_vaults` returned (a
/// Download, spec §12.3).
#[uniffi::export]
pub fn unwrap_vault_key(
    account_key: Vec<u8>,
    wrapped: String,
    vault_id: String,
) -> Result<Vec<u8>, MobileError> {
    let blob = decode_base64(&wrapped)?;
    let key = obsink_core::unwrap_vault_key(&key_bytes(account_key)?, &blob, &vault_id)
        .map_err(|_| MobileError::sync("the vault key does not open with this account key"))?;
    Ok(key.to_vec())
}

/// Every vault of the account (spec §15.1), with this account's wrapped keys.
#[uniffi::export]
pub fn list_vaults(
    server_url: String,
    bearer: String,
) -> Result<Vec<MobileVaultSummary>, MobileError> {
    let vaults = block_on(ApiClient::new(server_only(server_url, bearer, None)).list_vaults())
        .map_err(from_api)?;
    Ok(vaults.into_iter().map(MobileVaultSummary::from).collect())
}

/// Spec §12.2: a fresh vault key wrapped under the account key, the vault on
/// the server under a client-minted id, this device attached. The host keeps
/// the returned key.
#[uniffi::export]
pub fn create_vault(
    server_url: String,
    bearer: String,
    device_id: String,
    name: String,
    account_key: Vec<u8>,
) -> Result<MobileCreatedVault, MobileError> {
    let account_key = key_bytes(account_key)?;
    let vault_key = new_key();
    // The wrap's AAD is the vault id, so the id is minted here (spec §4.3).
    let vault_id = new_vault_id();
    let wrapped = obsink_core::wrap_vault_key(&account_key, &vault_key, &vault_id)?;
    let client = ApiClient::new(VaultConfig {
        server_url,
        bearer,
        vault_id: vault_id.clone(),
        local_path: String::new(),
        device_id: Some(device_id),
        ignore: Vec::new(),
    });
    let request = CreateVaultRequest {
        id: Some(vault_id),
        name,
        max_file_size: 50 * 1024 * 1024,
        wrapped_key: Some(encode_base64(&wrapped)),
    };
    let response = block_on(client.create_vault(&request)).map_err(from_api)?;
    block_on(client.attach_device(None)).map_err(from_api)?;
    Ok(MobileCreatedVault {
        vault: response.vault.into(),
        key: vault_key.to_vec(),
    })
}

/// This device holds the vault now (a Download); the checkpoint reports come
/// from `VaultClient` afterwards.
#[uniffi::export]
pub fn attach_device(config: MobileVaultConfig) -> Result<(), MobileError> {
    let config: VaultConfig = config.into();
    block_on(ApiClient::new(config).attach_device(None)).map_err(from_api)
}

/// This device no longer holds the vault (Remove from this device).
#[uniffi::export]
pub fn detach_device(config: MobileVaultConfig) -> Result<(), MobileError> {
    let config: VaultConfig = config.into();
    block_on(ApiClient::new(config).detach_device()).map_err(from_api)
}

/// Spec §4.3 `PATCH /vaults/:id`.
#[uniffi::export]
pub fn rename_vault(config: MobileVaultConfig, name: String) -> Result<(), MobileError> {
    let config: VaultConfig = config.into();
    block_on(ApiClient::new(config).rename_vault(&name)).map_err(from_api)
}

fn key_bytes(key: Vec<u8>) -> Result<KeyBytes, MobileError> {
    key.as_slice()
        .try_into()
        .map_err(|_| MobileError::InvalidKey {
            length: key.len() as u64,
        })
}

// --- History (spec §8.2, §9.3) ------------------------------------------------

/// The archived versions of one file, newest first.
#[uniffi::export]
pub fn list_versions(
    config: MobileVaultConfig,
    key: Vec<u8>,
    path: String,
) -> Result<Vec<MobileVersion>, MobileError> {
    let keys = derive_keys(&key_bytes(key)?);
    let config: VaultConfig = config.into();
    let versions =
        block_on(ApiClient::new(config).list_versions(&path, &keys)).map_err(from_api)?;
    Ok(versions
        .into_iter()
        .map(|version| MobileVersion {
            name: version.name,
            ts: version.ts,
            size: version.size,
        })
        .collect())
}

/// One archived version, decrypted. The host writes it into the vault (a
/// restore) or shows it (a preview).
#[uniffi::export]
pub fn fetch_version(
    config: MobileVaultConfig,
    key: Vec<u8>,
    path: String,
    name: String,
) -> Result<Vec<u8>, MobileError> {
    let keys = derive_keys(&key_bytes(key)?);
    let config: VaultConfig = config.into();
    block_on(ApiClient::new(config).get_version(&path, &name, &keys)).map_err(from_api)
}

/// The vault's recently deleted files with their real paths.
#[uniffi::export]
pub fn list_trash(
    config: MobileVaultConfig,
    key: Vec<u8>,
) -> Result<Vec<MobileTrashEntry>, MobileError> {
    let keys = derive_keys(&key_bytes(key)?);
    let config: VaultConfig = config.into();
    let entries = block_on(ApiClient::new(config).list_trash(&keys)).map_err(from_api)?;
    Ok(entries
        .into_iter()
        .map(|entry| MobileTrashEntry {
            path: entry.path,
            hash: entry.hash,
            size: entry.size,
            deleted_at: entry.deleted_at,
        })
        .collect())
}

/// The newest trash blob of one path, decrypted.
#[uniffi::export]
pub fn fetch_trash(
    config: MobileVaultConfig,
    key: Vec<u8>,
    path: String,
) -> Result<Vec<u8>, MobileError> {
    let keys = derive_keys(&key_bytes(key)?);
    let config: VaultConfig = config.into();
    block_on(ApiClient::new(config).get_trash(&path, &keys)).map_err(from_api)
}

// --- Accounts ---------------------------------------------------------------

/// Canonical form of a server URL (the keychain account for its bearer).
#[uniffi::export]
pub fn canonical_server_url(url: String) -> String {
    normalize_server_url(&url)
}

/// Which sign-in methods a server offers (`GET /`), and the wire format it
/// speaks (`0` from a server too old to say).
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileCapabilities {
    pub email: bool,
    pub apple: bool,
    /// New accounts need an invite code once the server has any user.
    pub invite_required: bool,
    pub protocol: u32,
}

/// This phone as the server knows it (spec §4.1): the id the host keeps for
/// good, a display name, and `ios`.
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileDeviceIdentity {
    pub id: String,
    pub name: String,
}

impl MobileDeviceIdentity {
    fn core(&self) -> Device {
        Device {
            id: self.id.clone(),
            name: self.name.clone(),
            platform: DevicePlatform::Ios,
        }
    }
}

/// The wrapped account key as the server holds it (`GET /auth/keys`).
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileAccountKeyBlob {
    pub key_id: String,
    pub wrapped: String,
    pub salt: String,
}

/// What a first `set passphrase` did (spec §12.1).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum MobileKeysOutcome {
    /// This device set the passphrase; `key` is the account key.
    Created,
    /// Another device set it first and the passphrase given opened its key.
    Exists,
    /// Another device set it first with a different passphrase: the host
    /// turns the form into Unlock.
    Mismatch,
}

#[derive(Clone, uniffi::Record)]
pub struct MobileSetKeysResult {
    pub outcome: MobileKeysOutcome,
    /// The account key (empty on `Mismatch`).
    pub key: Vec<u8>,
    pub key_id: String,
}

impl std::fmt::Debug for MobileSetKeysResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MobileSetKeysResult")
            .field("outcome", &self.outcome)
            .field("key", &"..")
            .field("key_id", &self.key_id)
            .finish()
    }
}

/// An unlocked account key with the server's `key_id`, so the host can tell
/// later whether its stored key is still the account's.
#[derive(Clone, uniffi::Record)]
pub struct MobileUnlockedKey {
    pub key: Vec<u8>,
    pub key_id: String,
}

impl std::fmt::Debug for MobileUnlockedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MobileUnlockedKey")
            .field("key", &"..")
            .field("key_id", &self.key_id)
            .finish()
    }
}

/// A signed-in session: `token` is the bearer to store in the Keychain.
/// `Debug` redacts it.
#[derive(Clone, uniffi::Record)]
pub struct MobileSession {
    pub token: String,
    pub session_id: String,
    pub user_id: String,
    pub email: Option<String>,
}

impl std::fmt::Debug for MobileSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MobileSession")
            .field("token", &"..")
            .field("session_id", &self.session_id)
            .field("user_id", &self.user_id)
            .field("email", &self.email)
            .finish()
    }
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
    pub created: u64,
    pub expires: u64,
    /// `active`, `used`, or `expired` (the server decides).
    pub status: String,
    pub used_at: Option<u64>,
}

impl From<obsink_core::Invite> for MobileInvite {
    fn from(invite: obsink_core::Invite) -> Self {
        MobileInvite {
            code: invite.code,
            created: invite.created,
            expires: invite.expires,
            status: invite.status,
            used_at: invite.used_at,
        }
    }
}

/// A device of the account (`GET /auth/me`, spec §4.1).
#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileDevice {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub created: u64,
    pub last_seen: u64,
    pub current: bool,
    pub vault_ids: Vec<String>,
}

#[uniffi::export]
pub fn auth_capabilities(server_url: String) -> Result<MobileCapabilities, MobileError> {
    let caps = block_on(AuthClient::new(&server_url).capabilities())
        .map_err(|error| from_auth(error, CallKind::SignIn))?;
    Ok(MobileCapabilities {
        email: caps.auth.email,
        apple: caps.auth.apple,
        invite_required: caps.invite_required,
        protocol: caps.protocol,
    })
}

/// Send a one-time code to `email`. Returns the code only against a dev
/// server (`AUTH_DEV_RETURN_CODE=1`), otherwise `None`.
#[uniffi::export]
pub fn auth_email_start(server_url: String, email: String) -> Result<Option<String>, MobileError> {
    let result = block_on(AuthClient::new(&server_url).email_start(&email))
        .map_err(|error| from_auth(error, CallKind::SignIn))?;
    Ok(result.code)
}

/// Spec §4.1: sign in as this device; one session per device.
#[uniffi::export]
pub fn auth_email_verify(
    server_url: String,
    email: String,
    code: String,
    device: MobileDeviceIdentity,
    invite_code: Option<String>,
) -> Result<MobileSession, MobileError> {
    let device = device.core();
    let session = block_on(AuthClient::new(&server_url).email_verify(
        &email,
        &code,
        &device,
        clean_invite(invite_code.as_deref()),
    ))
    .map_err(|error| from_auth(error, CallKind::SignIn))?;
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
    device: MobileDeviceIdentity,
    email: Option<String>,
    code: Option<String>,
    invite_code: Option<String>,
) -> Result<MobileSession, MobileError> {
    let device = device.core();
    let session = block_on(
        AuthClient::new(&server_url).apple_sign_in(
            &identity_token,
            &device,
            email.as_deref(),
            code.as_deref()
                .map(str::trim)
                .filter(|code| !code.is_empty()),
            clean_invite(invite_code.as_deref()),
        ),
    )
    .map_err(|error| from_auth(error, CallKind::SignIn))?;
    Ok(to_mobile_session(session))
}

#[uniffi::export]
pub fn auth_me(server_url: String, token: String) -> Result<MobileAccount, MobileError> {
    let me = block_on(AuthClient::new(&server_url).me(&token))
        .map_err(|error| from_auth(error, CallKind::Bearer))?;
    let user = me
        .user
        .ok_or_else(|| MobileError::sync("this session has no account behind it"))?;
    Ok(MobileAccount {
        user_id: user.id,
        email: user.email,
        devices: me
            .devices
            .into_iter()
            .map(|device| MobileDevice {
                id: device.id,
                name: device.name,
                platform: device.platform,
                created: device.created,
                last_seen: device.last_seen,
                current: device.current,
                vault_ids: device.vault_ids,
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

/// The wrapped account key, or `None` before the first set (spec §12.1).
#[uniffi::export]
pub fn auth_get_keys(
    server_url: String,
    token: String,
) -> Result<Option<MobileAccountKeyBlob>, MobileError> {
    let blob = block_on(AuthClient::new(&server_url).get_keys(&token))
        .map_err(|error| from_auth(error, CallKind::Bearer))?;
    Ok(blob.map(|blob| MobileAccountKeyBlob {
        key_id: blob.key_id,
        wrapped: blob.wrapped,
        salt: blob.salt,
    }))
}

/// Spec §12.1: set the passphrase for the first time (create-only). On a
/// lost race the winner's key is opened with the same passphrase; when that
/// fails the outcome is `Mismatch` and the host asks for the passphrase.
#[uniffi::export]
pub fn auth_set_passphrase(
    server_url: String,
    token: String,
    user_id: String,
    passphrase: String,
) -> Result<MobileSetKeysResult, MobileError> {
    if passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(MobileError::sync(format!(
            "At least {MIN_PASSPHRASE_CHARS} characters."
        )));
    }
    let (key, material) = create_account_key(&passphrase, &user_id)?;
    let outcome = block_on(AuthClient::new(&server_url).set_keys(&token, &material))
        .map_err(|error| from_auth(error, CallKind::Bearer))?;
    Ok(match outcome {
        SetKeysOutcome::Created { key_id } => MobileSetKeysResult {
            outcome: MobileKeysOutcome::Created,
            key: key.to_vec(),
            key_id,
        },
        SetKeysOutcome::Exists(blob) => match blob.unlock(&passphrase, &user_id) {
            Ok(key) => MobileSetKeysResult {
                outcome: MobileKeysOutcome::Exists,
                key: key.to_vec(),
                key_id: blob.key_id,
            },
            Err(_) => MobileSetKeysResult {
                outcome: MobileKeysOutcome::Mismatch,
                key: Vec::new(),
                key_id: blob.key_id,
            },
        },
    })
}

/// Enter the passphrase on a device that does not hold the account key.
#[uniffi::export]
pub fn auth_unlock(
    server_url: String,
    token: String,
    user_id: String,
    passphrase: String,
) -> Result<MobileUnlockedKey, MobileError> {
    let blob = block_on(AuthClient::new(&server_url).get_keys(&token))
        .map_err(|error| from_auth(error, CallKind::Bearer))?
        .ok_or_else(|| MobileError::sync("Set a passphrase first."))?;
    let key = blob
        .unlock(&passphrase, &user_id)
        .map_err(|_| MobileError::sync("Passphrase does not match this account."))?;
    Ok(MobileUnlockedKey {
        key: key.to_vec(),
        key_id: blob.key_id,
    })
}

/// DESIGN.md §5 `Change passphrase`: the current passphrase must open the
/// stored blob; the account key stays, rewrapped under the new one.
#[uniffi::export]
pub fn auth_change_passphrase(
    server_url: String,
    token: String,
    user_id: String,
    account_key: Vec<u8>,
    current: String,
    next: String,
) -> Result<(), MobileError> {
    if next.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(MobileError::sync(format!(
            "At least {MIN_PASSPHRASE_CHARS} characters."
        )));
    }
    let account_key = key_bytes(account_key)?;
    let auth = AuthClient::new(&server_url);
    let blob = block_on(auth.get_keys(&token))
        .map_err(|error| from_auth(error, CallKind::Bearer))?
        .ok_or_else(|| MobileError::sync("Set a passphrase first."))?;
    let opened = blob
        .unlock(&current, &user_id)
        .map_err(|_| MobileError::sync("Passphrase does not match this account."))?;
    if opened != account_key {
        return Err(MobileError::sync("Passphrase does not match this account."));
    }
    let material = rewrap_account_key(&account_key, &next, &user_id)?;
    block_on(auth.rewrap_keys(&token, &material))
        .map_err(|error| from_auth(error, CallKind::Bearer))
}

/// Mint an invite code so someone else can create an account.
#[uniffi::export]
pub fn auth_create_invite(server_url: String, token: String) -> Result<MobileInvite, MobileError> {
    let invite = block_on(AuthClient::new(&server_url).create_invite(&token))
        .map_err(|error| from_auth(error, CallKind::Bearer))?;
    Ok(invite.into())
}

/// Invites this account has minted, newest first.
#[uniffi::export]
pub fn auth_list_invites(
    server_url: String,
    token: String,
) -> Result<Vec<MobileInvite>, MobileError> {
    let invites = block_on(AuthClient::new(&server_url).list_invites(&token))
        .map_err(|error| from_auth(error, CallKind::Bearer))?;
    Ok(invites.into_iter().map(MobileInvite::from).collect())
}

/// Revoke the current session (sign out this device).
#[uniffi::export]
pub fn auth_logout(server_url: String, token: String) -> Result<(), MobileError> {
    block_on(AuthClient::new(&server_url).logout(&token))
        .map_err(|error| from_auth(error, CallKind::Bearer))
}

/// Sign another device of the account out for good (`DELETE
/// /auth/devices/:id`); its folders and keys stay where they are.
#[uniffi::export]
pub fn auth_revoke_device(
    server_url: String,
    token: String,
    device_id: String,
) -> Result<(), MobileError> {
    block_on(AuthClient::new(&server_url).revoke_device(&token, &device_id))
        .map_err(|error| from_auth(error, CallKind::Bearer))
}

/// `PATCH /auth/devices/:id`.
#[uniffi::export]
pub fn auth_rename_device(
    server_url: String,
    token: String,
    device_id: String,
    name: String,
) -> Result<(), MobileError> {
    block_on(AuthClient::new(&server_url).rename_device(&token, &device_id, &name))
        .map_err(|error| from_auth(error, CallKind::Bearer))
}

/// Delete the account and every vault it owns. Irreversible.
#[uniffi::export]
pub fn auth_delete_account(server_url: String, token: String) -> Result<(), MobileError> {
    block_on(AuthClient::new(&server_url).delete_account(&token))
        .map_err(|error| from_auth(error, CallKind::Bearer))
}

/// Delete a vault (and its server-side blobs) the account owns.
#[uniffi::export]
pub fn delete_vault(
    server_url: String,
    bearer: String,
    vault_id: String,
) -> Result<(), MobileError> {
    let config = VaultConfig {
        server_url,
        bearer,
        vault_id,
        local_path: String::new(),
        device_id: None,
        ignore: Vec::new(),
    };
    block_on(ApiClient::new(config).delete_vault()).map_err(from_api)
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

/// Stateful sync client for one vault. Holds the vault key and the pending
/// plan between `prepare` and `complete`; with `config.device_id` set, every
/// checkpoint reports its revision to the server (best effort, in core).
#[derive(uniffi::Object)]
pub struct VaultClient {
    config: VaultConfig,
    key: KeyBytes,
    pending: Mutex<Option<SyncPlan>>,
}

#[uniffi::export]
impl VaultClient {
    /// Build a client from config and the 32-byte vault key (from `create_vault`
    /// or `unwrap_vault_key`).
    #[uniffi::constructor]
    pub fn new(
        config: MobileVaultConfig,
        key: Vec<u8>,
    ) -> Result<std::sync::Arc<Self>, MobileError> {
        let key = key_bytes(key)?;
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
                .map_err(from_api)?;
            let bytes = decrypt(&keys.content_enc, &blob)?;
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
        let ignore = self.config.ignore_rules();
        let local = load_local_state(Path::new(&self.config.local_path), &keys, &ignore)
            .map_err(from_sync)?;
        let remote = block_on(fetch_remote_manifest(
            &ApiClient::new(self.config.clone()),
            Path::new(&self.config.local_path),
            &keys,
        ))
        .map_err(from_sync)?;
        let diff = diff_local_and_remote(&local.base, &local.working, &remote, &ignore);
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
        let plan = block_on(prepare_sync(&self.config, &self.key, &sink)).map_err(from_sync)?;
        let outcome = SyncOutcome {
            uploaded: plan.upload.len() as u32,
            downloaded: plan.download.len() as u32,
            conflicts: plan.conflicts.iter().map(to_mobile_conflict).collect(),
            failures: to_mobile_failures(&plan.failures),
            completed: false,
            checkpoint_error: None,
        };
        *self.pending.lock().expect("pending lock") = Some(plan);
        Ok(outcome)
    }

    fn complete_with(
        &self,
        resolutions: Vec<MobileResolution>,
        listener: Arc<dyn ProgressListener>,
    ) -> Result<SyncOutcome, MobileError> {
        // The plan stays pending until the round succeeds, so a failed
        // attempt can be retried without a fresh prepare.
        let plan = self
            .pending
            .lock()
            .expect("pending lock")
            .clone()
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
        .map_err(from_sync)?;
        // Late 409s become a conflict-only plan so `complete` and
        // `conflict_preview` work for the next round; otherwise nothing pends.
        *self.pending.lock().expect("pending lock") = SyncPlan::from_late_conflicts(&result);
        Ok(SyncOutcome {
            uploaded: result.upload.len() as u32,
            downloaded: result.download.len() as u32,
            conflicts: result.conflicts.iter().map(to_mobile_conflict).collect(),
            failures: to_mobile_failures(&result.failures),
            completed: result.conflicts.is_empty(),
            checkpoint_error: result.checkpoint_error.clone(),
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

/// Run a future to completion on the shared runtime, keeping the FFI surface
/// synchronous. One multi-thread runtime lives for the process: the reqwest
/// connection pool and TLS sessions survive between `prepare` and `complete`,
/// `block_on` may be entered from several Swift threads at once, and the
/// blocking pool the sync engine's filesystem work runs on stays warm.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("failed to build Tokio runtime")
        })
        .block_on(future)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_redact_secrets_in_debug() {
        let config = MobileVaultConfig {
            server_url: "https://s.test".into(),
            bearer: "secret-bearer-xyz".into(),
            vault_id: "vault_1".into(),
            local_path: "/tmp/v".into(),
            device_id: Some("dev_1".into()),
        };
        let session = MobileSession {
            token: "bearer-secret".into(),
            session_id: "sess_1".into(),
            user_id: "user_1".into(),
            email: None,
        };
        let printed = format!("{config:?} {session:?}");
        assert!(printed.contains("vault_1") && printed.contains("sess_1"));
        assert!(!printed.contains("secret-bearer-xyz"));
        assert!(!printed.contains("bearer-secret"));
    }

    #[test]
    fn a_401_is_unauthorized_only_on_bearer_calls() {
        assert!(matches!(
            map_status(401, "x".into(), CallKind::Bearer),
            MobileError::Unauthorized { .. }
        ));
        // A wrong sign-in code is an ordinary server answer.
        assert!(matches!(
            map_status(401, "incorrect code".into(), CallKind::SignIn),
            MobileError::Server { status: 401, message } if message == "incorrect code"
        ));
        assert!(matches!(
            map_status(403, "invite code is required".into(), CallKind::Bearer),
            MobileError::Server { status: 403, .. }
        ));
    }

    #[test]
    fn server_error_bodies_are_unwrapped() {
        assert_eq!(
            parse_error_body(r#"{"error":"vault storage limit reached"}"#),
            "vault storage limit reached"
        );
        assert_eq!(parse_error_body("plain text"), "plain text");
        assert_eq!(parse_error_body(""), "");
    }

    #[test]
    fn api_and_sync_errors_map_by_kind() {
        assert!(matches!(
            from_api(ApiError::Unauthorized),
            MobileError::Unauthorized { .. }
        ));
        assert!(matches!(
            from_api(ApiError::Crypto(CryptoError::Decrypt)),
            MobileError::Sync { .. }
        ));
        assert!(matches!(
            from_sync(SyncEngineError::Api(ApiError::Unauthorized)),
            MobileError::Unauthorized { .. }
        ));
        assert!(matches!(
            from_sync(SyncEngineError::MissingResolution("a.md".into())),
            MobileError::Sync { message } if message.contains("a.md")
        ));
    }

    #[test]
    fn vault_keys_wrap_under_the_account_key() {
        let account = new_vault_key();
        let vault = new_vault_key();
        assert_eq!(account.len(), 32);
        let wrapped = wrap_vault_key_for(account.clone(), vault.clone(), "vault_a".into()).unwrap();
        assert_eq!(
            unwrap_vault_key(account.clone(), wrapped.clone(), "vault_a".into()).unwrap(),
            vault
        );
        // The AAD is the vault id: another vault's id does not open it.
        assert!(unwrap_vault_key(account.clone(), wrapped.clone(), "vault_b".into()).is_err());
        assert!(unwrap_vault_key(new_vault_key(), wrapped, "vault_a".into()).is_err());
        assert!(matches!(
            wrap_vault_key_for(vec![1, 2, 3], vault, "vault_a".into()),
            Err(MobileError::InvalidKey { length: 3 })
        ));
        assert_eq!(mobile_protocol_version(), 3);
        assert!(new_device_id().starts_with("dev_"));
    }

    #[test]
    fn a_protocol_mismatch_has_its_own_variant() {
        assert!(matches!(
            from_auth(
                AuthError::ProtocolMismatch {
                    server: 4,
                    client: 3
                },
                CallKind::SignIn
            ),
            MobileError::ProtocolMismatch {
                server: 4,
                client: 3
            }
        ));
    }

    #[test]
    fn a_refused_connection_is_a_network_error() {
        // Port 9 (discard) is closed on a developer machine; the connection is refused at once.
        let error = block_on(AuthClient::new("http://127.0.0.1:9").capabilities()).unwrap_err();
        assert!(matches!(
            from_auth(error, CallKind::SignIn),
            MobileError::Network { .. }
        ));
    }
}
