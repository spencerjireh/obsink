use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::UNIX_EPOCH,
};

use dirs::home_dir;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, WindowEvent,
};

use obsink_core::{
    complete_sync, derive_key, derive_keys, diff_local_and_remote, fetch_remote_manifest,
    keychain::{delete_secret, load_secret, save_secret},
    load_local_state, normalize_server_url, prepare_sync, sync_manifest_path, write_atomic,
    ApiClient, AuthClient, Conflict, ConflictResolution, CreateVaultRequest, KeyBytes,
    ProgressEvent, ProgressSink, SyncPlan, SyncResult, VaultConfig, VaultSummary,
};
use serde::{Deserialize, Serialize};

const APP_CONFIG_FILE: &str = ".obsink/app.json";

#[derive(Default)]
struct AppState {
    pending_plans: Mutex<HashMap<String, SyncPlan>>,
    /// Vaults with a sync or resolution in progress. Two cycles on one vault
    /// would race on the same files and checkpoint, so the second is refused.
    in_flight: Mutex<HashSet<String>>,
}

/// Marks a vault as busy for the guard's lifetime.
struct InFlightGuard<'a> {
    state: &'a AppState,
    vault_id: String,
}

impl<'a> InFlightGuard<'a> {
    fn acquire(state: &'a AppState, vault_id: &str) -> Result<Self, String> {
        let mut in_flight = state
            .in_flight
            .lock()
            .map_err(|_| "in-flight lock poisoned".to_string())?;
        if !in_flight.insert(vault_id.to_string()) {
            return Err(format!("sync already running for vault {vault_id}"));
        }
        Ok(Self {
            state,
            vault_id: vault_id.to_string(),
        })
    }
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut in_flight) = self.state.in_flight.lock() {
            in_flight.remove(&self.vault_id);
        }
    }
}

/// Bridges core sync progress events to the frontend via the Tauri event bus.
/// Events flow as `sync://progress` (payload = the core `ProgressEvent`),
/// consumed by the React UI's live progress line.
#[derive(Clone)]
struct TauriProgressSink {
    app: AppHandle,
}

impl ProgressSink for TauriProgressSink {
    fn report(&self, event: ProgressEvent) {
        // Best-effort: a listener that has unmounted shouldn't fail the sync.
        let _ = self.app.emit("sync://progress", event);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredAppConfig {
    vaults: Vec<StoredVault>,
    active_vault_id: Option<String>,
}

impl Default for StoredAppConfig {
    fn default() -> Self {
        Self {
            vaults: Vec::new(),
            active_vault_id: None,
        }
    }
}

/// One configured vault. The server bearer (session token) is NOT stored
/// here — it lives in the keychain under `bearer:<server_url>`. The
/// `server_url` alias reads configs written before the server pivot.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredVault {
    id: String,
    name: String,
    #[serde(alias = "server_url")]
    server_url: String,
    local_path: String,
}

#[derive(Debug, Clone, Serialize)]
struct LocalVaultSummary {
    id: String,
    name: String,
    server_url: String,
    local_path: String,
    active: bool,
}

impl LocalVaultSummary {
    fn from_stored(vault: &StoredVault, active: bool) -> Self {
        Self {
            id: vault.id.clone(),
            name: vault.name.clone(),
            server_url: vault.server_url.clone(),
            local_path: vault.local_path.clone(),
            active,
        }
    }
}

fn bearer_account(server_url: &str) -> String {
    format!("bearer:{}", normalize_server_url(server_url))
}

/// The bearer stored for a server, or a "sign in first" error.
fn load_bearer(server_url: &str) -> Result<String, String> {
    load_secret(&bearer_account(server_url))
        .map_err(|_| format!("not signed in to {server_url} — sign in first"))
}

fn device_name() -> String {
    Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .map(|name| format!("{name} (ObSink Desktop)"))
        .unwrap_or_else(|| "ObSink Desktop".to_string())
}

// --- Accounts -----------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct AuthCapabilities {
    email: bool,
    apple: bool,
}

/// What the UI shows for a server's credential state.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AccountState {
    /// No credential stored for this server.
    SignedOut,
    /// A signed-in account; `devices` lists the account's sessions.
    Account {
        user_id: String,
        email: Option<String>,
        devices: Vec<DeviceInfo>,
        usage: Option<UsageInfo>,
    },
}

#[derive(Debug, Clone, Serialize)]
struct UsageInfo {
    total_bytes: u64,
    max_vault_bytes: Option<u64>,
    max_vaults: Option<u32>,
    vaults: Vec<VaultUsageInfo>,
}

#[derive(Debug, Clone, Serialize)]
struct VaultUsageInfo {
    id: String,
    bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
struct InviteInfo {
    code: String,
    expires: u64,
}

#[derive(Debug, Clone, Serialize)]
struct DeviceInfo {
    session_id: String,
    device_name: String,
    created: u64,
    current: bool,
}

#[tauri::command]
async fn get_auth_capabilities(server_url: String) -> Result<AuthCapabilities, String> {
    let caps = AuthClient::new(&server_url)
        .capabilities()
        .await
        .map_err(err_string)?;
    Ok(AuthCapabilities {
        email: caps.auth.email,
        apple: caps.auth.apple,
    })
}

/// Send a one-time code. Returns the code itself only against a dev server
/// (`AUTH_DEV_RETURN_CODE=1`) so harnesses can complete the flow.
#[tauri::command]
async fn auth_email_start(server_url: String, email: String) -> Result<Option<String>, String> {
    let result = AuthClient::new(&server_url)
        .email_start(email.trim())
        .await
        .map_err(err_string)?;
    Ok(result.code)
}

#[tauri::command]
async fn auth_email_verify(
    server_url: String,
    email: String,
    code: String,
    invite_code: Option<String>,
) -> Result<AccountState, String> {
    let invite = invite_code
        .as_deref()
        .map(str::trim)
        .filter(|code| !code.is_empty());
    let session = AuthClient::new(&server_url)
        .email_verify(email.trim(), code.trim(), &device_name(), invite)
        .await
        .map_err(err_string)?;
    save_secret(&bearer_account(&server_url), &session.token).map_err(err_string)?;
    get_account(server_url).await
}

#[tauri::command]
async fn get_account(server_url: String) -> Result<AccountState, String> {
    let Ok(bearer) = load_secret(&bearer_account(&server_url)) else {
        return Ok(AccountState::SignedOut);
    };
    match AuthClient::new(&server_url).me(&bearer).await {
        Ok(me) => match me.user {
            Some(user) => Ok(AccountState::Account {
                user_id: user.id,
                email: user.email,
                devices: me
                    .sessions
                    .into_iter()
                    .map(|session| DeviceInfo {
                        session_id: session.id,
                        device_name: session.device_name,
                        created: session.created,
                        current: session.current,
                    })
                    .collect(),
                usage: me.usage.map(|usage| UsageInfo {
                    total_bytes: usage.total_bytes,
                    max_vault_bytes: usage.max_vault_bytes,
                    max_vaults: usage.max_vaults,
                    vaults: usage
                        .vaults
                        .into_iter()
                        .map(|vault| VaultUsageInfo {
                            id: vault.id,
                            bytes: vault.bytes,
                        })
                        .collect(),
                }),
            }),
            // The operator bearer has no account behind it; the desktop only
            // works with accounts.
            None => Ok(AccountState::SignedOut),
        },
        Err(obsink_core::AuthError::Server { status, .. }) if status.as_u16() == 401 => {
            // Session revoked/expired elsewhere: forget it locally.
            delete_secret(&bearer_account(&server_url));
            Ok(AccountState::SignedOut)
        }
        Err(error) => Err(err_string(error)),
    }
}

/// Mint an invite code for someone else to create an account on this server.
#[tauri::command]
async fn create_invite(server_url: String) -> Result<InviteInfo, String> {
    let bearer = load_bearer(&server_url)?;
    let invite = AuthClient::new(&server_url)
        .create_invite(&bearer)
        .await
        .map_err(err_string)?;
    Ok(InviteInfo {
        code: invite.code,
        expires: invite.expires,
    })
}

/// Sign out of a server. Vault configs stay; sync will ask for a credential
/// again.
#[tauri::command]
async fn sign_out(server_url: String) -> Result<(), String> {
    let account = bearer_account(&server_url);
    if let Ok(bearer) = load_secret(&account) {
        // Best effort: the local credential goes away regardless.
        let _ = AuthClient::new(&server_url).logout(&bearer).await;
    }
    delete_secret(&account);
    Ok(())
}

/// Vaults the current credential can see on a server (for the Connect picker).
#[tauri::command]
async fn list_remote_vaults(server_url: String) -> Result<Vec<VaultSummary>, String> {
    let bearer = load_bearer(&server_url)?;
    ApiClient::new(VaultConfig {
        server_url: normalize_server_url(&server_url),
        api_key: bearer,
        vault_id: String::new(),
        local_path: String::new(),
    })
    .list_vaults()
    .await
    .map_err(err_string)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
enum AddVaultMode {
    Create,
    Connect,
}

#[derive(Debug, Clone, Deserialize)]
struct AddVaultRequest {
    mode: AddVaultMode,
    server_url: String,
    local_path: String,
    vault_name: String,
    vault_id: String,
    passphrase: String,
}

#[derive(Debug, Clone, Serialize)]
struct SyncStatus {
    active_vault_id: Option<String>,
    configured_vaults: usize,
    pending_uploads: usize,
    pending_downloads: usize,
    pending_conflicts: usize,
    last_sync_manifest_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SyncCommandResponse {
    completed_result: Option<SyncResult>,
    pending_conflicts: Vec<Conflict>,
}

#[derive(Debug, Clone, Serialize)]
struct ConflictPreview {
    path: String,
    local_text: String,
    remote_text: String,
    local_deleted: bool,
    remote_deleted: bool,
}

#[tauri::command]
fn get_vaults() -> Result<Vec<LocalVaultSummary>, String> {
    let config = load_app_config().map_err(err_string)?;
    Ok(config
        .vaults
        .iter()
        .map(|vault| {
            LocalVaultSummary::from_stored(
                vault,
                config.active_vault_id.as_deref() == Some(vault.id.as_str()),
            )
        })
        .collect())
}

#[tauri::command]
fn set_active_vault(vault_id: String) -> Result<LocalVaultSummary, String> {
    let mut config = load_app_config().map_err(err_string)?;
    let vault = config
        .vaults
        .iter()
        .find(|vault| vault.id == vault_id)
        .cloned()
        .ok_or_else(|| format!("vault {} not configured locally", vault_id))?;

    config.active_vault_id = Some(vault.id.clone());
    save_app_config(&config).map_err(err_string)?;

    Ok(LocalVaultSummary::from_stored(&vault, true))
}

#[tauri::command]
async fn add_vault(request: AddVaultRequest) -> Result<LocalVaultSummary, String> {
    validate_request(&request)?;
    let server_url = normalize_server_url(&request.server_url);
    let bearer = load_bearer(&server_url)?;

    let client = ApiClient::new(VaultConfig {
        server_url: server_url.clone(),
        api_key: bearer,
        vault_id: String::new(),
        local_path: request.local_path.clone(),
    });

    let (vault_id, vault_name) = match request.mode {
        AddVaultMode::Create => {
            let response = client
                .create_vault(&CreateVaultRequest {
                    name: request.vault_name.clone(),
                    max_file_size: 50 * 1024 * 1024,
                })
                .await
                .map_err(err_string)?;
            (response.vault.id, response.vault.name)
        }
        AddVaultMode::Connect => {
            let vaults = client.list_vaults().await.map_err(err_string)?;
            let vault = vaults
                .into_iter()
                .find(|vault| vault.id == request.vault_id)
                .ok_or_else(|| format!("vault {} not found", request.vault_id))?;
            (vault.id, vault.name)
        }
    };

    let key = derive_key(&request.passphrase, vault_id.as_bytes()).map_err(err_string)?;
    let stored = StoredVault {
        id: vault_id.clone(),
        name: vault_name.clone(),
        server_url,
        local_path: request.local_path.clone(),
    };

    validate_passphrase(&stored, &key).await?;
    save_key_to_keychain(&vault_id, &key).map_err(err_string)?;
    upsert_vault(stored.clone()).map_err(err_string)?;

    Ok(LocalVaultSummary::from_stored(&stored, true))
}

#[tauri::command]
async fn get_status() -> Result<SyncStatus, String> {
    let config = load_app_config().map_err(err_string)?;
    let Some(vault) = active_vault(&config) else {
        return Ok(SyncStatus {
            active_vault_id: None,
            configured_vaults: config.vaults.len(),
            pending_uploads: 0,
            pending_downloads: 0,
            pending_conflicts: 0,
            last_sync_manifest_path: None,
        });
    };

    let local_root = PathBuf::from(&vault.local_path);
    let manifest_path = sync_manifest_path(&local_root);
    let keys = derive_keys(&load_key_from_keychain(&vault.id).map_err(err_string)?);
    let vault_config = to_vault_config(vault);
    let remote_manifest = fetch_remote_manifest(&ApiClient::new(vault_config), &local_root, &keys)
        .await
        .map_err(err_string)?;
    let local = load_local_state(&local_root, &keys).map_err(err_string)?;
    let diff = diff_local_and_remote(&local.base, &local.working, &remote_manifest);

    Ok(SyncStatus {
        active_vault_id: Some(vault.id.clone()),
        configured_vaults: config.vaults.len(),
        pending_uploads: diff.upload.len(),
        pending_downloads: diff.download.len(),
        pending_conflicts: diff.conflicts.len(),
        last_sync_manifest_path: manifest_path
            .exists()
            .then(|| manifest_path.display().to_string()),
    })
}

#[tauri::command]
async fn get_manifest_diff(vault_id: Option<String>) -> Result<SyncResult, String> {
    let vault = selected_vault(vault_id).map_err(err_string)?;
    let keys = derive_keys(&load_key_from_keychain(&vault.id).map_err(err_string)?);
    let local = load_local_state(Path::new(&vault.local_path), &keys).map_err(err_string)?;
    let remote_manifest = fetch_remote_manifest(
        &ApiClient::new(to_vault_config(&vault)),
        Path::new(&vault.local_path),
        &keys,
    )
    .await
    .map_err(err_string)?;
    Ok(diff_local_and_remote(
        &local.base,
        &local.working,
        &remote_manifest,
    ))
}

async fn sync_vault_inner(
    vault_id: Option<String>,
    state: &AppState,
    progress: &dyn ProgressSink,
) -> Result<SyncCommandResponse, String> {
    let vault = selected_vault(vault_id).map_err(err_string)?;
    let _guard = InFlightGuard::acquire(state, &vault.id)?;
    let key = load_key_from_keychain(&vault.id).map_err(err_string)?;
    // A fresh cycle supersedes any plan left over from an earlier one.
    set_pending_plan(state, &vault.id, None)?;
    let plan = prepare_sync(&to_vault_config(&vault), &key, progress)
        .await
        .map_err(err_string)?;

    if plan.conflicts.is_empty() {
        let result = complete_sync(&to_vault_config(&vault), &key, &plan, &[], progress)
            .await
            .map_err(err_string)?;
        return finish_cycle(state, &vault.id, result);
    }

    let pending_conflicts = plan.conflicts.clone();
    set_pending_plan(state, &vault.id, Some(plan))?;

    Ok(SyncCommandResponse {
        completed_result: None,
        pending_conflicts,
    })
}

fn set_pending_plan(
    state: &AppState,
    vault_id: &str,
    plan: Option<SyncPlan>,
) -> Result<(), String> {
    let mut plans = state
        .pending_plans
        .lock()
        .map_err(|_| "pending plan lock poisoned".to_string())?;
    match plan {
        Some(plan) => {
            plans.insert(vault_id.to_string(), plan);
        }
        None => {
            plans.remove(vault_id);
        }
    }
    Ok(())
}

/// After `complete_sync`: late 409s become a conflict-only plan the UI can
/// resolve in another round; otherwise the vault has no pending plan.
fn finish_cycle(
    state: &AppState,
    vault_id: &str,
    result: SyncResult,
) -> Result<SyncCommandResponse, String> {
    let late_plan = SyncPlan::from_late_conflicts(&result);
    let pending_conflicts = result.conflicts.clone();
    set_pending_plan(state, vault_id, late_plan)?;
    Ok(SyncCommandResponse {
        completed_result: Some(result),
        pending_conflicts,
    })
}

#[tauri::command]
async fn sync_vault(
    vault_id: Option<String>,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<SyncCommandResponse, String> {
    let sink = TauriProgressSink { app };
    sync_vault_inner(vault_id, &state, &sink).await
}

async fn resolve_conflict_inner(
    vault_id: String,
    resolutions: Vec<ConflictResolution>,
    state: &AppState,
    progress: &dyn ProgressSink,
) -> Result<SyncCommandResponse, String> {
    let vault = selected_vault(Some(vault_id.clone())).map_err(err_string)?;
    let _guard = InFlightGuard::acquire(state, &vault.id)?;
    // The plan stays in place until the round succeeds, so a failed attempt
    // (network, keychain) can be retried without a fresh sync.
    let plan = state
        .pending_plans
        .lock()
        .map_err(|_| "pending plan lock poisoned".to_string())?
        .get(&vault_id)
        .cloned()
        .ok_or_else(|| format!("no pending conflict set for {}", vault_id))?;
    let key = load_key_from_keychain(&vault.id).map_err(err_string)?;

    let result = complete_sync(
        &to_vault_config(&vault),
        &key,
        &plan,
        &resolutions,
        progress,
    )
    .await
    .map_err(err_string)?;
    finish_cycle(state, &vault.id, result)
}

#[tauri::command]
async fn resolve_conflict(
    vault_id: String,
    resolutions: Vec<ConflictResolution>,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<SyncCommandResponse, String> {
    let sink = TauriProgressSink { app };
    resolve_conflict_inner(vault_id, resolutions, &state, &sink).await
}

async fn get_conflict_preview_inner(
    vault_id: String,
    path: String,
    state: &AppState,
) -> Result<ConflictPreview, String> {
    let vault = selected_vault(Some(vault_id.clone())).map_err(err_string)?;
    let conflict = {
        let pending_plans = state
            .pending_plans
            .lock()
            .map_err(|_| "pending plan lock poisoned".to_string())?;
        let plan = pending_plans
            .get(&vault_id)
            .ok_or_else(|| format!("no pending conflict set for {}", vault_id))?;
        plan.conflicts
            .iter()
            .find(|conflict| conflict.path == path)
            .cloned()
            .ok_or_else(|| format!("no pending conflict preview for {}", path))?
    };

    let keys = derive_keys(&load_key_from_keychain(&vault.id).map_err(err_string)?);
    let client = ApiClient::new(to_vault_config(&vault));

    let local_text = if conflict.local.deleted {
        String::new()
    } else {
        let bytes =
            fs::read(Path::new(&vault.local_path).join(&conflict.path)).map_err(err_string)?;
        String::from_utf8_lossy(&bytes).into_owned()
    };

    let remote_text = if conflict.remote.deleted {
        String::new()
    } else {
        let blob = client
            .get_file(&conflict.path, &keys)
            .await
            .map_err(err_string)?;
        let bytes = obsink_core::decrypt(&keys.content_enc, &blob).map_err(err_string)?;
        String::from_utf8_lossy(&bytes).into_owned()
    };

    Ok(ConflictPreview {
        path: conflict.path,
        local_text,
        remote_text,
        local_deleted: conflict.local.deleted,
        remote_deleted: conflict.remote.deleted,
    })
}

#[tauri::command]
async fn get_conflict_preview(
    vault_id: String,
    path: String,
    state: tauri::State<'_, AppState>,
) -> Result<ConflictPreview, String> {
    get_conflict_preview_inner(vault_id, path, &state).await
}

fn validate_request(request: &AddVaultRequest) -> Result<(), String> {
    if request.server_url.trim().is_empty() {
        return Err("server URL is required".into());
    }
    if request.local_path.trim().is_empty() {
        return Err("local vault path is required".into());
    }
    if request.passphrase.is_empty() {
        return Err("passphrase is required".into());
    }

    match request.mode {
        AddVaultMode::Create if request.vault_name.trim().is_empty() => {
            Err("vault name is required".into())
        }
        AddVaultMode::Connect if request.vault_id.trim().is_empty() => {
            Err("vault ID is required".into())
        }
        _ => Ok(()),
    }
}

async fn validate_passphrase(vault: &StoredVault, key: &KeyBytes) -> Result<(), String> {
    let keys = derive_keys(key);
    let client = ApiClient::new(to_vault_config(vault));
    let manifest = fetch_remote_manifest(&client, Path::new(&vault.local_path), &keys)
        .await
        .map_err(err_string)?;
    if let Some((path, _)) = manifest.iter().find(|(_, entry)| !entry.deleted) {
        let blob = client.get_file(path, &keys).await.map_err(err_string)?;
        obsink_core::decrypt(&keys.content_enc, &blob).map_err(err_string)?;
    }
    Ok(())
}

fn selected_vault(vault_id: Option<String>) -> Result<StoredVault, io::Error> {
    let config = load_app_config()?;
    let desired_id = vault_id
        .or(config.active_vault_id.clone())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no configured vaults available"))?;
    config
        .vaults
        .into_iter()
        .find(|vault| vault.id == desired_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "vault not configured locally"))
}

fn active_vault(config: &StoredAppConfig) -> Option<&StoredVault> {
    config
        .active_vault_id
        .as_ref()
        .and_then(|vault_id| config.vaults.iter().find(|vault| vault.id == *vault_id))
}

/// Bearer comes from the keychain; if it is missing the request goes out
/// without one and the server's 401 surfaces as `ApiError::Unauthorized`
/// ("sign in again"), which is the message the user needs.
fn to_vault_config(vault: &StoredVault) -> VaultConfig {
    VaultConfig {
        server_url: vault.server_url.clone(),
        api_key: load_secret(&bearer_account(&vault.server_url)).unwrap_or_default(),
        vault_id: vault.id.clone(),
        local_path: vault.local_path.clone(),
    }
}

fn app_config_path() -> Result<PathBuf, io::Error> {
    let home = home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory not found"))?;
    Ok(home.join(APP_CONFIG_FILE))
}

fn load_app_config() -> Result<StoredAppConfig, io::Error> {
    let path = app_config_path()?;
    if !path.exists() {
        return Ok(StoredAppConfig::default());
    }

    let contents = fs::read_to_string(path)?;
    let mut config: StoredAppConfig = serde_json::from_str(&contents)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    for vault in &mut config.vaults {
        vault.server_url = normalize_server_url(&vault.server_url);
    }
    Ok(config)
}

fn save_app_config(config: &StoredAppConfig) -> Result<(), io::Error> {
    let path = app_config_path()?;
    let bytes = serde_json::to_vec_pretty(config)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_atomic(&path, &bytes)
}

fn upsert_vault(vault: StoredVault) -> Result<(), io::Error> {
    let mut config = load_app_config()?;
    if let Some(existing) = config.vaults.iter_mut().find(|item| item.id == vault.id) {
        *existing = vault.clone();
    } else {
        config.vaults.push(vault.clone());
    }

    config.active_vault_id = Some(vault.id);
    save_app_config(&config)
}

fn save_key_to_keychain(vault_id: &str, key: &KeyBytes) -> Result<(), io::Error> {
    save_secret(vault_id, &hex::encode(key))
}

fn load_key_from_keychain(vault_id: &str) -> Result<KeyBytes, io::Error> {
    let bytes = hex::decode(load_secret(vault_id)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if bytes.len() != 32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "stored key has invalid length",
        ));
    }
    let mut key = [0_u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}

fn err_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[allow(dead_code)]
fn manifest_timestamp(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

/// Bring the main window to the foreground, creating no new windows.
fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Build the menu-bar tray icon and wire its menu and click behavior.
fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let sync_now = MenuItem::with_id(app, "sync_now", "Sync Now", true, None::<&str>)?;
    let show = MenuItem::with_id(app, "show", "Show ObSink", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit ObSink", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&sync_now, &show, &separator, &quit])?;

    let mut builder = TrayIconBuilder::with_id("obsink-tray")
        .tooltip("ObSink")
        .menu(&menu)
        // Left click toggles the window; the menu stays on right click.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "sync_now" => {
                // The frontend owns the sync flow (conflict state, refresh),
                // so the tray just asks it to run and surfaces the window.
                let _ = app.emit("tray://sync-now", ());
                show_main_window(app);
            }
            "show" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        // Template rendering makes the icon adopt the macOS menu-bar tint.
        builder = builder.icon(icon.clone()).icon_as_template(true);
    }

    builder.build(app)?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            add_vault,
            auth_email_start,
            auth_email_verify,
            create_invite,
            get_account,
            get_auth_capabilities,
            list_remote_vaults,
            sign_out,
            get_conflict_preview,
            get_manifest_diff,
            get_status,
            get_vaults,
            resolve_conflict,
            set_active_vault,
            sync_vault,
        ])
        .setup(|app| {
            setup_tray(app.handle())?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Menu-bar behavior: closing the window hides it to the tray
            // instead of quitting, so background sync keeps working.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use obsink_core::{derive_keys, ApiClient, ConflictResolutionChoice, VaultConfig};
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    /// `#[ignore]`d live integration test: drives the real desktop command
    /// functions (add_vault / set_active_vault / get_status / sync_vault_inner /
    /// get_conflict_preview_inner / resolve_conflict_inner) end-to-end against a
    /// running server, using the operator bearer. Run with:
    ///   OBSINK_TEST_SERVER_URL=... OBSINK_TEST_API_KEY=... \
    ///   cargo test -p obsink-desktop live_tests -- --ignored --nocapture
    #[ignore]
    #[tokio::test]
    async fn desktop_flows_live() {
        let server_url = normalize_server_url(&env_or_panic("OBSINK_TEST_SERVER_URL"));
        let api_key = env_or_panic("OBSINK_TEST_API_KEY");
        let passphrase = std::env::var("OBSINK_TEST_PASSPHRASE")
            .unwrap_or_else(|_| "obsink-test-passphrase-2026".to_string());

        // Sandbox HOME so the desktop's ~/.obsink/app.json is isolated from the
        // user's real config. (Keychain is real and keyed per vault id.)
        let sandbox = PathBuf::from(format!("/tmp/obsink-desktop-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&sandbox);
        let dir_a = sandbox.join("deviceA");
        let dir_b = sandbox.join("deviceB");
        let dir_c = sandbox.join("deviceC");
        fs::create_dir_all(dir_a.join("notes")).unwrap();
        fs::create_dir_all(dir_b.join("notes")).unwrap();
        fs::create_dir_all(&dir_c).unwrap();
        std::env::set_var("HOME", &sandbox);
        // Use the file-backed keyring so the live test never prompts the macOS keychain.
        let keyring_dir = sandbox.join("keyring");
        fs::create_dir_all(&keyring_dir).unwrap();
        std::env::set_var("OBSINK_KEYRING_DIR", &keyring_dir);
        // The desktop has no API-key entry any more; seed the operator bearer
        // the way a sign-in would.
        save_secret(&bearer_account(&server_url), &api_key).unwrap();

        let state = AppState::default();
        let file_rel = "notes/a.md";

        // ===== OBS-3: add (Create) + upload + cross-device download =====
        let summary = add_vault(AddVaultRequest {
            mode: AddVaultMode::Create,
            server_url: server_url.clone(),
            local_path: dir_a.to_string_lossy().into_owned(),
            vault_name: "obsink-desktop-verify".to_string(),
            vault_id: String::new(),
            passphrase: passphrase.clone(),
        })
        .await
        .unwrap();
        let vault_id = summary.id.clone();
        println!("OBS-3: created vault {vault_id}");
        load_key_from_keychain(&vault_id).expect("OBS-3: keychain entry present after add");
        assert!(get_vaults()
            .unwrap()
            .iter()
            .any(|v| v.id == vault_id && v.active));

        fs::write(dir_a.join(file_rel), "content-A").unwrap();
        let resp = sync_vault_inner(Some(vault_id.clone()), &state, &obsink_core::NoProgress)
            .await
            .unwrap();
        assert!(resp.pending_conflicts.is_empty());
        assert_eq!(
            resp.completed_result.unwrap().upload.len(),
            1,
            "OBS-3: a.md should upload"
        );

        // Connect device B (same passphrase -> same key) and pull.
        add_vault(AddVaultRequest {
            mode: AddVaultMode::Connect,
            server_url: server_url.clone(),
            local_path: dir_b.to_string_lossy().into_owned(),
            vault_name: String::new(),
            vault_id: vault_id.clone(),
            passphrase: passphrase.clone(),
        })
        .await
        .unwrap(); // validate_passphrase decrypts a.md -> proves the key works
        let resp_b = sync_vault_inner(Some(vault_id.clone()), &state, &obsink_core::NoProgress)
            .await
            .unwrap();
        assert_eq!(
            resp_b.completed_result.unwrap().download.len(),
            1,
            "OBS-3: a.md should download on B"
        );
        assert_eq!(
            fs::read_to_string(dir_b.join(file_rel)).unwrap(),
            "content-A",
            "OBS-3: content propagated Mac -> server -> iOS-equivalent"
        );

        // ===== OBS-5: stale-vault detection (server ahead of client) =====
        // B uploads a new file the A-side view doesn't have.
        fs::write(dir_b.join("notes/b.md"), "B-only").unwrap();
        sync_vault_inner(Some(vault_id.clone()), &state, &obsink_core::NoProgress)
            .await
            .unwrap();
        // Repoint the active local folder at A (which is now behind the server).
        connect_local(&server_url, &vault_id, &passphrase, &dir_a).await;
        let status = get_status().await.unwrap();
        assert_eq!(status.active_vault_id.as_deref(), Some(vault_id.as_str()));
        assert!(
            status.pending_downloads >= 1,
            "OBS-5: expected remote changes pending (b.md) -> banner source"
        );

        // ===== OBS-4: conflict resolution — all three choices =====
        let choices = [
            ConflictResolutionChoice::KeepLocal,
            ConflictResolutionChoice::KeepRemote,
            ConflictResolutionChoice::KeepBoth,
        ];
        for choice in choices {
            // Rebaseline: A's a.md == BASE, synced, so base == local == remote.
            let base_text = format!("BASE-{choice:?}");
            fs::write(dir_a.join(file_rel), &base_text).unwrap();
            let _ = sync_vault_inner(Some(vault_id.clone()), &state, &obsink_core::NoProgress)
                .await
                .unwrap();

            // Engineer a three-way conflict: another device overwrites the
            // server copy while A edits locally.
            let local_text = format!("LOCAL-{choice:?}");
            put_remote_text(
                &server_url,
                &api_key,
                &vault_id,
                file_rel,
                "REMOTE",
                &base_text,
            )
            .await;
            fs::write(dir_a.join(file_rel), &local_text).unwrap();

            let resp = sync_vault_inner(Some(vault_id.clone()), &state, &obsink_core::NoProgress)
                .await
                .unwrap();
            assert_eq!(
                resp.pending_conflicts.len(),
                1,
                "OBS-4 ({choice:?}): expected 1 conflict"
            );
            assert!(resp.completed_result.is_none());

            // Side-by-side preview decrypts the remote blob via the desktop path.
            let preview =
                get_conflict_preview_inner(vault_id.clone(), file_rel.to_string(), &state)
                    .await
                    .unwrap();
            assert_eq!(
                preview.local_text, local_text,
                "OBS-4 ({choice:?}): local preview"
            );
            assert_eq!(
                preview.remote_text, "REMOTE",
                "OBS-4 ({choice:?}): remote preview"
            );

            let result = resolve_conflict_inner(
                vault_id.clone(),
                vec![ConflictResolution {
                    path: file_rel.to_string(),
                    choice: choice.clone(),
                }],
                &state,
                &obsink_core::NoProgress,
            )
            .await
            .unwrap();
            assert!(
                result.pending_conflicts.is_empty(),
                "OBS-4 ({choice:?}): no late 409 expected"
            );

            match choice {
                ConflictResolutionChoice::KeepLocal => {
                    let remote = remote_text(&server_url, &api_key, &vault_id, file_rel).await;
                    assert_eq!(
                        remote, local_text,
                        "OBS-4 (KeepLocal): server should hold the local version"
                    );
                }
                ConflictResolutionChoice::KeepRemote => {
                    assert_eq!(
                        fs::read_to_string(dir_a.join(file_rel)).unwrap(),
                        "REMOTE",
                        "OBS-4 (KeepRemote): local should hold the remote version"
                    );
                }
                ConflictResolutionChoice::KeepBoth => {
                    assert_eq!(
                        fs::read_to_string(dir_a.join("notes/a.conflict.md")).unwrap(),
                        "REMOTE",
                        "OBS-4 (KeepBoth): a.conflict.md should hold the remote version"
                    );
                    assert_eq!(
                        remote_text(&server_url, &api_key, &vault_id, file_rel).await,
                        local_text,
                        "OBS-4 (KeepBoth): server should hold the local version"
                    );
                    let _ = fs::remove_file(dir_a.join("notes/a.conflict.md"));
                }
            }
            println!("OBS-4 ({choice:?}): resolution verified");
        }

        // ===== OBS-6: multiple-vault switching =====
        let s2 = add_vault(AddVaultRequest {
            mode: AddVaultMode::Create,
            server_url: server_url.clone(),
            local_path: dir_c.to_string_lossy().into_owned(),
            vault_name: "obsink-desktop-verify-2".to_string(),
            vault_id: String::new(),
            passphrase: passphrase.clone(),
        })
        .await
        .unwrap();
        let vault_id_2 = s2.id.clone();
        // Both vaults' keys live in the keyring simultaneously.
        load_key_from_keychain(&vault_id).expect("OBS-6: vault 1 key resolves");
        load_key_from_keychain(&vault_id_2).expect("OBS-6: vault 2 key resolves");
        assert_eq!(
            get_vaults().unwrap().len(),
            2,
            "OBS-6: two vaults configured"
        );

        // Switch active back to vault 1; keychain lookup must follow the active id.
        let active = set_active_vault(vault_id.clone()).unwrap();
        assert!(active.active);
        assert_eq!(active.id, vault_id);
        let st = get_status().await.unwrap();
        assert_eq!(
            st.active_vault_id.as_deref(),
            Some(vault_id.as_str()),
            "OBS-6: active vault switched"
        );
        println!("OBS-6: multi-vault switching verified");

        println!("ALL DESKTOP FLOWS VERIFIED: vaults={vault_id}, {vault_id_2}");
        let _ = fs::remove_dir_all(&sandbox);
    }

    /// `#[ignore]`d live test for the account path: email code sign-in, vault
    /// creation under the account, `get_account`, invites, sign-out. Needs a
    /// server running with `AUTH_DEV_RETURN_CODE=1` (the local docker compose).
    /// On a server that already has accounts, set `OBSINK_TEST_API_KEY` so the
    /// test can mint the invite its first sign-up needs:
    ///   OBSINK_TEST_SERVER_URL=http://localhost:8080 OBSINK_TEST_API_KEY=... \
    ///   cargo test -p obsink-desktop account_flow_live -- --ignored --nocapture
    #[ignore]
    #[tokio::test]
    async fn account_flow_live() {
        let server_url = normalize_server_url(&env_or_panic("OBSINK_TEST_SERVER_URL"));
        let sandbox = PathBuf::from(format!("/tmp/obsink-desktop-acct-{}", std::process::id()));
        let _ = fs::remove_dir_all(&sandbox);
        let dir = sandbox.join("vault");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("note.md"), "# cloud\n").unwrap();
        std::env::set_var("HOME", &sandbox);
        let keyring_dir = sandbox.join("keyring");
        fs::create_dir_all(&keyring_dir).unwrap();
        std::env::set_var("OBSINK_KEYRING_DIR", &keyring_dir);

        assert!(matches!(
            get_account(server_url.clone()).await.unwrap(),
            AccountState::SignedOut
        ));
        // Without a credential, adding a vault must fail with a sign-in hint.
        let denied = add_vault(AddVaultRequest {
            mode: AddVaultMode::Create,
            server_url: server_url.clone(),
            local_path: dir.to_string_lossy().into_owned(),
            vault_name: "denied".to_string(),
            vault_id: String::new(),
            passphrase: "pw".to_string(),
        })
        .await
        .unwrap_err();
        assert!(denied.contains("sign in"), "{denied}");

        // A fresh server lets the first account in without an invite; an
        // established one needs a code, which the operator bearer can mint.
        let bootstrap_invite = match std::env::var("OBSINK_TEST_API_KEY") {
            Ok(api_key) if !api_key.is_empty() => Some(
                AuthClient::new(&server_url)
                    .create_invite(&api_key)
                    .await
                    .unwrap()
                    .code,
            ),
            _ => None,
        };
        let email = format!("desktop-{}@example.com", std::process::id());
        let code = auth_email_start(server_url.clone(), email.clone())
            .await
            .unwrap()
            .expect("dev server returns the code inline");
        let state = auth_email_verify(server_url.clone(), email.clone(), code, bootstrap_invite)
            .await
            .unwrap();
        match &state {
            AccountState::Account {
                email: got,
                devices,
                usage,
                ..
            } => {
                assert_eq!(got.as_deref(), Some(email.as_str()));
                assert_eq!(devices.len(), 1);
                assert!(devices[0].current);
                assert!(usage.is_some(), "server reports usage");
            }
            other => panic!("expected account, got {other:?}"),
        }
        assert!(!fs::read_to_string(keyring_dir.join(format!(
            "bearer_{}",
            normalize_server_url(&server_url).replace(['/', ':'], "_")
        )))
        .unwrap()
        .is_empty());

        let summary = add_vault(AddVaultRequest {
            mode: AddVaultMode::Create,
            server_url: server_url.clone(),
            local_path: dir.to_string_lossy().into_owned(),
            vault_name: "cloud-vault".to_string(),
            vault_id: String::new(),
            passphrase: "pw".to_string(),
        })
        .await
        .unwrap();
        // app.json must not contain the bearer.
        let app_json = fs::read_to_string(sandbox.join(".obsink/app.json")).unwrap();
        assert!(!app_json.contains("os_"), "{app_json}");
        assert!(!app_json.contains("api_key"), "{app_json}");

        let listed = list_remote_vaults(server_url.clone()).await.unwrap();
        assert_eq!(listed.iter().filter(|v| v.id == summary.id).count(), 1);

        let state = AppState::default();
        let response = sync_vault_inner(Some(summary.id.clone()), &state, &obsink_core::NoProgress)
            .await
            .unwrap();
        assert!(response.completed_result.is_some());

        // Invite gating: a second account needs a code minted by the first.
        let invite = create_invite(server_url.clone()).await.unwrap();
        assert!(!invite.code.is_empty());
        let keyring_b = sandbox.join("keyring-b");
        fs::create_dir_all(&keyring_b).unwrap();
        std::env::set_var("OBSINK_KEYRING_DIR", &keyring_b);
        let second = format!("desktop-b-{}@example.com", std::process::id());
        let code = auth_email_start(server_url.clone(), second.clone())
            .await
            .unwrap()
            .unwrap();
        let refused = auth_email_verify(server_url.clone(), second.clone(), code.clone(), None)
            .await
            .unwrap_err();
        assert!(refused.contains("invite"), "{refused}");
        let accepted =
            auth_email_verify(server_url.clone(), second.clone(), code, Some(invite.code))
                .await
                .unwrap();
        assert!(matches!(accepted, AccountState::Account { .. }));
        std::env::set_var("OBSINK_KEYRING_DIR", &keyring_dir);

        sign_out(server_url.clone()).await.unwrap();
        assert!(matches!(
            get_account(server_url.clone()).await.unwrap(),
            AccountState::SignedOut
        ));
        let err = sync_vault_inner(Some(summary.id.clone()), &state, &obsink_core::NoProgress)
            .await
            .unwrap_err();
        assert!(err.to_lowercase().contains("unauthorized"), "{err}");

        println!("ACCOUNT FLOW VERIFIED: vault={}", summary.id);
        let _ = fs::remove_dir_all(&sandbox);
    }

    fn env_or_panic(key: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| panic!("set {key}"))
    }

    async fn connect_local(server_url: &str, vault_id: &str, passphrase: &str, local_path: &Path) {
        add_vault(AddVaultRequest {
            mode: AddVaultMode::Connect,
            server_url: server_url.to_string(),
            local_path: local_path.to_string_lossy().into_owned(),
            vault_name: String::new(),
            vault_id: vault_id.to_string(),
            passphrase: passphrase.to_string(),
        })
        .await
        .unwrap();
    }

    /// Overwrite `path` on the server as another device would, gated on the
    /// hash of `parent_text` (the version both sides last agreed on).
    async fn put_remote_text(
        server_url: &str,
        api_key: &str,
        vault_id: &str,
        path: &str,
        text: &str,
        parent_text: &str,
    ) {
        let key = load_key_from_keychain(vault_id).unwrap();
        let keys = derive_keys(&key);
        let config = VaultConfig {
            server_url: server_url.to_string(),
            api_key: api_key.to_string(),
            vault_id: vault_id.to_string(),
            local_path: String::new(),
        };
        let parent = obsink_core::content_hmac(&keys.content_mac, parent_text.as_bytes());
        let content_hash = obsink_core::content_hmac(&keys.content_mac, text.as_bytes());
        let ciphertext = obsink_core::encrypt(&keys.content_enc, text.as_bytes()).unwrap();
        ApiClient::new(config)
            .put_file(path, Some(&parent), &content_hash, ciphertext, &keys)
            .await
            .unwrap();
    }

    async fn remote_text(server_url: &str, api_key: &str, vault_id: &str, path: &str) -> String {
        let key = load_key_from_keychain(vault_id).unwrap();
        let keys = derive_keys(&key);
        let config = VaultConfig {
            server_url: server_url.to_string(),
            api_key: api_key.to_string(),
            vault_id: vault_id.to_string(),
            local_path: String::new(),
        };
        let blob = ApiClient::new(config).get_file(path, &keys).await.unwrap();
        let bytes = obsink_core::decrypt(&keys.content_enc, &blob).unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}
