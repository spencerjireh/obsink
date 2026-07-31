pub mod api_client;
pub mod crypto;
pub mod hasher;
pub mod manifest;
pub mod progress;
pub mod sync_engine;
pub mod types;

pub use api_client::{ApiClient, ApiError};
pub use crypto::{
    content_hmac, decrypt, decrypt_path, derive_key, derive_keys, encrypt, encrypt_path,
    path_token, CryptoError, CryptoKeys, KeyBytes, PROTOCOL_VERSION,
};
pub use hasher::{build_manifest_from_dir, hash_bytes, hash_file, HasherError};
pub use manifest::{diff_manifests, ManifestDiff};
pub use progress::{NoProgress, ProgressEvent, ProgressSink, SyncPhase};
pub use sync_engine::{
    build_working_manifest_for_path, complete_sync, diff_local_and_remote, load_manifest_from_disk,
    prepare_sync, save_manifest_to_disk, sync_manifest_path, SyncEngineError,
};
pub use types::{
    BatchOperation, BatchOperationResult, BatchRequest, BatchResponse, Conflict,
    ConflictResolution, ConflictResolutionChoice, CreateVaultRequest, CreateVaultResponse,
    FileEntry, Manifest, ServerConflict, SyncAction, SyncActionKind, SyncFailure, SyncPlan,
    SyncResult, VaultConfig, VaultSummary,
};
