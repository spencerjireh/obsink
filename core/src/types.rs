use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub hash: String,
    pub modified: u64,
    pub size: u64,
    #[serde(default)]
    pub deleted: bool,
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

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SyncResult {
    pub upload: Vec<SyncAction>,
    pub download: Vec<SyncAction>,
    pub conflicts: Vec<Conflict>,
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
}

const fn default_max_file_size() -> u64 {
    50 * 1024 * 1024
}
