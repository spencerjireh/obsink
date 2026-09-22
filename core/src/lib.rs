// The pure modules (crypto, manifest, ignore rules, sync rules, pacing, the
// server URL) build for every target, including wasm32 for the browser client
// via `core-wasm`. Everything that touches the filesystem, tokio, or reqwest is
// native-only.
#[cfg(not(target_arch = "wasm32"))]
pub mod api_client;
#[cfg(not(target_arch = "wasm32"))]
pub mod auth;
pub mod crypto;
#[cfg(not(target_arch = "wasm32"))]
pub mod daemon;
pub mod fs_util;
pub mod hash_cache;
#[cfg(not(target_arch = "wasm32"))]
pub mod hasher;
pub mod ignore;
#[cfg(feature = "keychain")]
pub mod keychain;
pub mod manifest;
pub mod pacing;
pub mod progress;
pub mod server_url;
#[cfg(not(target_arch = "wasm32"))]
pub mod sync_engine;
pub mod sync_rules;
pub mod types;
#[cfg(not(target_arch = "wasm32"))]
pub mod watcher;

#[cfg(not(target_arch = "wasm32"))]
pub use api_client::{ApiClient, ApiError, ManifestFetch};
#[cfg(not(target_arch = "wasm32"))]
pub use auth::{
    AuthClient, AuthError, AuthMethods, Capabilities, EmailStartResult, Invite, Me, MeSession,
    MeUser, Session, SessionInfo, Usage, UserInfo, VaultUsage,
};
pub use crypto::{
    account_verifier, content_hmac, create_account_key, decode_base64, decrypt, decrypt_path,
    derive_key, derive_keys, encode_base64, encrypt, encrypt_path, new_key, new_salt, path_token,
    rewrap_account_key, unlock_account_key, unwrap_key, unwrap_vault_key, vault_wrap_key, wrap_key,
    wrap_vault_key, AccountKeyMaterial, CryptoError, CryptoKeys, KeyBytes, PROTOCOL_VERSION,
    SALT_LEN, WRAPPED_KEY_LEN,
};
#[cfg(not(target_arch = "wasm32"))]
pub use daemon::{
    daemon_channel, run_daemon, DaemonCallError, DaemonCommand, DaemonError, DaemonEvent,
    DaemonHandle, DaemonOptions,
};
#[cfg(not(target_arch = "wasm32"))]
pub use fs_util::write_atomic;
pub use fs_util::TEMP_SUFFIX;
#[cfg(not(target_arch = "wasm32"))]
pub use hash_cache::hash_cache_path;
pub use hash_cache::{hash_cache_key_id, HashCache, Stat};
#[cfg(not(target_arch = "wasm32"))]
pub use hasher::{build_manifest_from_dir, build_manifest_with_cache, hash_file, HasherError};
pub use ignore::{IgnoreRules, DEFAULT_IGNORE};
pub use manifest::{checkpoint_manifest, diff_manifests, ManifestDiff};
pub use pacing::{backoff_wait, Backoff, PollPacing};
pub use progress::{NoProgress, ProgressEvent, ProgressSink, SyncPhase};
pub use server_url::{legacy_server_urls, normalize_server_url, LEGACY_SERVER_ALIASES};
#[cfg(not(target_arch = "wasm32"))]
pub use sync_engine::{
    complete_sync, diff_local_and_remote, fetch_remote_manifest, load_local_state,
    load_manifest_from_disk, prepare_sync, remote_changed, remote_manifest_cache_path,
    save_manifest_to_disk, sync_manifest_path, LocalState, SyncEngineError,
};
pub use sync_rules::{
    chunk_uploads, conflict_copy_path, conflict_to_upload, effective_choice, parent_hash,
    BATCH_BYTE_BUDGET, BATCH_MAX_OPS,
};
pub use types::{
    BatchOp, BatchOperationResult, BatchResponse, Conflict, ConflictResolution,
    ConflictResolutionChoice, CreateVaultRequest, CreateVaultResponse, FileEntry,
    ListVaultsResponse, Manifest, ServerConflict, SyncAction, SyncActionKind, SyncFailure,
    SyncPlan, SyncResult, VaultConfig, VaultDevice, VaultSummary,
};
