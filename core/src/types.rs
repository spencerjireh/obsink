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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultConfig {
    pub worker_url: String,
    pub api_key: String,
    pub vault_id: String,
    pub local_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultSummary {
    pub id: String,
    pub name: String,
    pub created: u64,
    #[serde(default = "default_max_file_size")]
    pub max_file_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateVaultRequest {
    pub name: String,
    #[serde(default = "default_max_file_size")]
    pub max_file_size: u64,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchRequest {
    pub operations: Vec<BatchOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum BatchOperation {
    Put {
        path: String,
        #[serde(rename = "parentHash")]
        parent_hash: Option<String>,
        #[serde(rename = "contentHash")]
        content_hash: String,
        content: String,
        #[serde(default, rename = "encPath")]
        enc_path: String,
    },
    Delete {
        path: String,
        #[serde(rename = "parentHash")]
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

const fn default_max_file_size() -> u64 {
    50 * 1024 * 1024
}
