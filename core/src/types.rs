use crate::ignore::IgnoreRules;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// HMAC-SHA256 of the plaintext contents (hex).
    pub hash: String,
    pub modified: u64,
    pub size: u64,
    #[serde(default)]
    pub deleted: bool,
    /// AES-GCM-encrypted real path (base64). Set by the server from the upload's
    /// `X-Enc-Path` header so a fresh device can recover filenames from the
    /// token-keyed manifest. Empty on locally-constructed entries.
    #[serde(default, rename = "encPath")]
    pub enc_path: String,
}

pub type Manifest = BTreeMap<String, FileEntry>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncActionKind {
    Upload,
    Download,
    DeleteLocal,
    DeleteRemote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncAction {
    pub path: String,
    pub kind: SyncActionKind,
    pub local: Option<FileEntry>,
    pub remote: Option<FileEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    pub path: String,
    pub local: FileEntry,
    pub remote: FileEntry,
}

/// A single file that could not be transferred during a sync. When `fatal` is
/// true the error was systemic (network down, auth failure) and aborted the
/// rest of the batch; otherwise it was file-specific (e.g. a too-large file)
/// and the sync continued past it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncFailure {
    pub path: String,
    pub kind: SyncActionKind,
    pub error: String,
    pub fatal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SyncResult {
    pub upload: Vec<SyncAction>,
    pub download: Vec<SyncAction>,
    pub conflicts: Vec<Conflict>,
    /// Per-file transfers that failed this cycle. Empty on a clean sync.
    #[serde(default)]
    pub failures: Vec<SyncFailure>,
    /// The checkpoint (re-fetch of the server manifest and the base write)
    /// failed after the transfers: the files moved, the next diff redoes the
    /// bookkeeping. Not a file failure, so it has no path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_error: Option<String>,
}

/// `Debug` redacts `bearer` (the session token).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultConfig {
    pub server_url: String,
    /// The session token. `api_key` is the v2 name still found in stored
    /// configs and the mobile facade.
    #[serde(alias = "api_key")]
    pub bearer: String,
    pub vault_id: String,
    pub local_path: String,
    /// This machine's device id (spec §4.1); when set, a checkpoint reports
    /// its revision to `PUT /vaults/:id/devices/self`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// Extra ignore patterns on top of [`crate::ignore::DEFAULT_IGNORE`].
    #[serde(default)]
    pub ignore: Vec<String>,
}

impl VaultConfig {
    /// The built-in ignore rules plus this vault's extras.
    pub fn ignore_rules(&self) -> IgnoreRules {
        IgnoreRules::defaults().with_extra(self.ignore.iter().map(String::as_str))
    }
}

impl std::fmt::Debug for VaultConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultConfig")
            .field("server_url", &self.server_url)
            .field("bearer", &"..")
            .field("vault_id", &self.vault_id)
            .field("local_path", &self.local_path)
            .field("ignore", &self.ignore)
            .finish()
    }
}

/// One vault of the account as `GET /vaults` lists it (spec §4.3). The v3
/// fields default so a v2 response still parses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultSummary {
    pub id: String,
    pub name: String,
    pub created: u64,
    #[serde(default = "default_max_file_size")]
    pub max_file_size: u64,
    /// The manifest revision (the ETag), for "n revisions behind".
    #[serde(default)]
    pub revision: u64,
    /// When the manifest last changed.
    #[serde(default)]
    pub last_write: u64,
    /// Live bytes on the server.
    #[serde(default)]
    pub bytes: u64,
    /// The vault key wrapped for this account (base64), or `None` for a vault
    /// created by a client from before wire format v3 (never on a v3 server).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_key: Option<String>,
    /// The devices that hold this vault.
    #[serde(default)]
    pub devices: Vec<VaultDevice>,
}

/// A device that holds a vault, as `GET /vaults` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultDevice {
    pub id: String,
    pub name: String,
    pub platform: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_revision: Option<u64>,
}

/// `GET /vaults`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListVaultsResponse {
    pub vaults: Vec<VaultSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateVaultRequest {
    /// A client-minted id (`new_vault_id`) so the wrapped key's AAD is the
    /// real id; the server mints one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    #[serde(default = "default_max_file_size")]
    pub max_file_size: u64,
    /// The new vault's key wrapped under the account key (base64); the server
    /// requires it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateVaultResponse {
    pub vault: VaultSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConflict {
    pub path: String,
    pub current: Option<FileEntry>,
}

/// One operation for [`ApiClient::batch`](crate::ApiClient::batch). Paths are
/// real (plaintext-side) paths; the client tokenises them on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchOp {
    Put {
        path: String,
        parent_hash: Option<String>,
        content_hash: String,
        /// Encrypted blob bytes, sent raw as a multipart part.
        content: Vec<u8>,
    },
    Delete {
        path: String,
        parent_hash: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchResponse {
    pub results: Vec<BatchOperationResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchOperationResult {
    pub path: String,
    pub status: u16,
    pub conflict: Option<ServerConflict>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictResolutionChoice {
    KeepLocal,
    KeepRemote,
    KeepBoth,
    /// Leave the conflict for the user: nothing is transferred, the path is
    /// held back from the checkpoint and comes back on `SyncResult.conflicts`.
    /// The daemon's answer to every conflict.
    Defer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictResolution {
    pub path: String,
    pub choice: ConflictResolutionChoice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPlan {
    pub upload: Vec<SyncAction>,
    pub download: Vec<SyncAction>,
    pub conflicts: Vec<Conflict>,
    /// Download-side failures captured during `prepare_sync` (best-effort).
    #[serde(default)]
    pub failures: Vec<SyncFailure>,
}

impl SyncPlan {
    /// A plan that carries only the late (409) conflicts of a completed sync,
    /// so the caller can collect resolutions and call `complete_sync` again.
    pub fn from_late_conflicts(result: &SyncResult) -> Option<SyncPlan> {
        (!result.conflicts.is_empty()).then(|| SyncPlan {
            upload: Vec::new(),
            download: Vec::new(),
            conflicts: result.conflicts.clone(),
            failures: Vec::new(),
        })
    }
}

const fn default_max_file_size() -> u64 {
    50 * 1024 * 1024
}

#[cfg(test)]
mod tests {
    use super::VaultConfig;

    #[test]
    fn vault_config_debug_redacts_the_bearer() {
        let config = VaultConfig {
            server_url: "https://s.test".into(),
            bearer: "secret-bearer-xyz".into(),
            device_id: None,
            vault_id: "vault_1".into(),
            local_path: "/tmp/v".into(),
            ignore: Vec::new(),
        };
        let printed = format!("{config:?}");
        assert!(printed.contains("vault_1"));
        assert!(printed.contains("bearer: \"..\""));
        assert!(!printed.contains("secret-bearer-xyz"));
    }
}
