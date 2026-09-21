pub mod api_client;
pub mod auth;
pub mod crypto;
pub mod daemon;
pub mod fs_util;
pub mod hash_cache;
pub mod hasher;
pub mod ignore;
#[cfg(feature = "keychain")]
pub mod keychain;
pub mod manifest;
pub mod progress;
pub mod sync_engine;
pub mod types;
pub mod watcher;

pub use api_client::{ApiClient, ApiError, ManifestFetch};
pub use auth::{
    normalize_server_url, AuthClient, AuthError, AuthMethods, Capabilities, EmailStartResult,
    Invite, Me, MeSession, MeUser, Session, SessionInfo, Usage, UserInfo, VaultUsage,
};
pub use crypto::{
    content_hmac, decrypt, decrypt_path, derive_key, derive_keys, encrypt, encrypt_path,
    path_token, CryptoError, CryptoKeys, KeyBytes, PROTOCOL_VERSION,
};
pub use daemon::{
    daemon_channel, run_daemon, DaemonCallError, DaemonCommand, DaemonError, DaemonEvent,
    DaemonHandle, DaemonOptions,
};
pub use fs_util::{write_atomic, TEMP_SUFFIX};
pub use hash_cache::{hash_cache_path, HashCache, Stat};
pub use hasher::{
    build_manifest_from_dir, build_manifest_with_cache, hash_bytes, hash_file, HasherError,
};
pub use ignore::{IgnoreRules, DEFAULT_IGNORE};
pub use manifest::{checkpoint_manifest, diff_manifests, ManifestDiff};
pub use progress::{NoProgress, ProgressEvent, ProgressSink, SyncPhase};
pub use sync_engine::{
    complete_sync, diff_local_and_remote, fetch_remote_manifest, load_local_state,
    load_manifest_from_disk, prepare_sync, remote_changed, remote_manifest_cache_path,
    save_manifest_to_disk, sync_manifest_path, LocalState, SyncEngineError,
};
pub use types::{
    BatchOp, BatchOperationResult, BatchResponse, Conflict, ConflictResolution,
    ConflictResolutionChoice, CreateVaultRequest, CreateVaultResponse, FileEntry, Manifest,
    ServerConflict, SyncAction, SyncActionKind, SyncFailure, SyncPlan, SyncResult, VaultConfig,
    VaultSummary,
};
