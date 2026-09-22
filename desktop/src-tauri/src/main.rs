use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant, UNIX_EPOCH},
};

use dirs::home_dir;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow, WindowEvent,
};

use obsink_core::{
    complete_sync, daemon_channel, derive_key, derive_keys, diff_local_and_remote,
    fetch_remote_manifest,
    keychain::{delete_secret, load_bearer as load_stored_bearer, load_secret, save_secret},
    load_local_state, normalize_server_url, prepare_sync, run_daemon, write_atomic, ApiClient,
    ApiError, AuthClient, AuthError, Conflict, ConflictResolution, CreateVaultRequest,
    DaemonCallError, DaemonEvent, DaemonHandle, DaemonOptions, KeyBytes, ManifestDiff,
    ProgressEvent, ProgressSink, SyncEngineError, SyncPlan, SyncResult, VaultConfig, VaultSummary,
};
use serde::{Deserialize, Serialize};

mod activity;
use activity::ActivityEvent;

const APP_CONFIG_FILE: &str = ".obsink/app.json";

#[derive(Default)]
struct AppState {
    /// Prepared plans awaiting conflict resolution, keyed by vault id.
    pending_plans: Mutex<HashMap<String, SyncPlan>>,
    /// Vaults with a sync or resolution in progress. Two cycles on one vault
    /// would race on the same files and checkpoint, so the second is refused.
    in_flight: Mutex<HashSet<String>>,
    /// When the popover last hid itself on focus loss. The tray click that
    /// closes it fires that first, so the click must not reopen it.
    popover_hidden_at: Mutex<Option<Instant>>,
    /// The running daemon per vault. A vault with a daemon syncs through it
    /// (`Sync now` and resolutions are commands to it), so no two cycles
    /// can overlap.
    daemons: Mutex<HashMap<String, DaemonHandle>>,
}

fn daemon_handle(state: &AppState, vault_id: &str) -> Option<DaemonHandle> {
    state
        .daemons
        .lock()
        .ok()
        .and_then(|daemons| daemons.get(vault_id).cloned())
}

/// Start daemons for every vault that can sync (on this server, key in the
/// keychain, signed in) and stop the ones whose vault no longer can. Called
/// at launch and after anything that changes that set.
fn reconcile_daemons(app: &AppHandle) {
    let state = app.state::<AppState>();
    let config = match load_app_config() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("daemons not started: {error}");
            return;
        }
    };
    let wanted: HashMap<String, StoredVault> = config
        .vaults
        .into_iter()
        .filter(|vault| {
            !is_foreign(vault)
                && load_key_from_keychain(&vault.id).is_ok()
                && load_bearer(&vault.server_url).is_ok()
        })
        .map(|vault| (vault.id.clone(), vault))
        .collect();
    let Ok(mut daemons) = state.daemons.lock() else {
        return;
    };
    let stale: Vec<String> = daemons
        .keys()
        .filter(|id| !wanted.contains_key(*id))
        .cloned()
        .collect();
    for id in stale {
        if let Some(handle) = daemons.remove(&id) {
            handle.stop();
        }
    }
    for (id, vault) in wanted {
        if daemons.contains_key(&id) {
            continue;
        }
        let Ok(key) = load_key_from_keychain(&id) else {
            continue;
        };
        let (handle, commands) = daemon_channel();
        let (events_tx, events_rx) = tokio::sync::mpsc::channel(32);
        let sink = TauriProgressSink {
            app: app.clone(),
            vault_id: id.clone(),
        };
        tauri::async_runtime::spawn(run_daemon(
            to_vault_config(&vault),
            key,
            DaemonOptions::default(),
            commands,
            events_tx,
            Arc::new(sink),
        ));
        tauri::async_runtime::spawn(relay_daemon_events(app.clone(), id.clone(), events_rx));
        daemons.insert(id, handle);
    }
}

/// Mirror a daemon's events into the state the popover reads: `in_flight`
/// while a cycle runs (`Syncing…`), `pending_plans` for conflicts the user
/// has to resolve, the activity log, and a `state://changed` per event.
async fn relay_daemon_events(
    app: AppHandle,
    vault_id: String,
    mut events: tokio::sync::mpsc::Receiver<DaemonEvent>,
) {
    let state = app.state::<AppState>();
    while let Some(event) = events.recv().await {
        match event {
            DaemonEvent::Started => continue,
            DaemonEvent::SyncStarted => {
                if let Ok(mut in_flight) = state.in_flight.lock() {
                    in_flight.insert(vault_id.clone());
                }
            }
            DaemonEvent::SyncFinished(result) => {
                if let Ok(mut in_flight) = state.in_flight.lock() {
                    in_flight.remove(&vault_id);
                }
                let _ = set_pending_plan(&state, &vault_id, SyncPlan::from_late_conflicts(&result));
                if let Err(io_error) = activity::record_sync(&vault_id, &result) {
                    eprintln!("activity log for {vault_id} not written: {io_error}");
                }
            }
            DaemonEvent::ConflictsPending { plan } => {
                let _ = set_pending_plan(&state, &vault_id, Some(plan));
            }
            DaemonEvent::Failed { message, .. } => {
                if let Ok(mut in_flight) = state.in_flight.lock() {
                    in_flight.remove(&vault_id);
                }
                if let Err(io_error) = activity::record_error(&vault_id, &message) {
                    eprintln!("activity log for {vault_id} not written: {io_error}");
                }
            }
            DaemonEvent::Stopped => {
                if let Ok(mut in_flight) = state.in_flight.lock() {
                    in_flight.remove(&vault_id);
                }
                if let Ok(mut daemons) = state.daemons.lock() {
                    daemons.remove(&vault_id);
                }
                emit_state_changed(&app, Some(&vault_id));
                break;
            }
        }
        emit_state_changed(&app, Some(&vault_id));
    }
}

/// Marks a vault as busy for the guard's lifetime.
struct InFlightGuard<'a> {
    state: &'a AppState,
    vault_id: String,
}

impl<'a> InFlightGuard<'a> {
    fn acquire(state: &'a AppState, vault_id: &str) -> Result<Self, CommandError> {
        let mut in_flight = state
            .in_flight
            .lock()
            .map_err(|_| "in-flight lock poisoned".to_string())?;
        if !in_flight.insert(vault_id.to_string()) {
            return Err(CommandError::other(format!(
                "sync already running for vault {vault_id}"
            )));
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
/// Events flow as `sync://progress` (payload = [`ProgressEnvelope`]) so a
/// window that shows several vaults can attribute each line.
#[derive(Clone)]
struct TauriProgressSink {
    app: AppHandle,
    vault_id: String,
}

#[derive(Clone, Serialize)]
struct ProgressEnvelope<'a> {
    vault_id: &'a str,
    event: ProgressEvent,
}

impl ProgressSink for TauriProgressSink {
    fn report(&self, event: ProgressEvent) {
        // Best-effort: a listener that has unmounted shouldn't fail the sync.
        let _ = self.app.emit(
            "sync://progress",
            ProgressEnvelope {
                vault_id: &self.vault_id,
                event,
            },
        );
    }
}

/// Something about a vault or the account changed (sync finished, vault
/// added or removed, signed in or out). Every window re-reads its state.
#[derive(Clone, Serialize)]
struct StateChanged {
    vault_id: Option<String>,
}

fn emit_state_changed(app: &AppHandle, vault_id: Option<&str>) {
    let _ = app.emit(
        "state://changed",
        StateChanged {
            vault_id: vault_id.map(str::to_string),
        },
    );
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoredAppConfig {
    vaults: Vec<StoredVault>,
    active_vault_id: Option<String>,
}

/// One configured vault. The server bearer (session token) is NOT stored
/// here — it lives in the keychain under `bearer:<server_url>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredVault {
    id: String,
    name: String,
    server_url: String,
    local_path: String,
    /// Extra ignore patterns for this vault, on top of the built-in defaults.
    #[serde(default)]
    ignore: Vec<String>,
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

/// The public server every build talks to unless `OBSINK_SERVER_URL` says
/// otherwise at build time (self-hosters) or at launch (tests, harnesses).
const FALLBACK_SERVER_URL: &str = "https://obsink-api.spencerjireh.com";

/// The website with the downloads; the tray's "Check for updates" opens it.
/// There is no in-app updater: a menu-bar app with no Dock icon would
/// otherwise never tell its user a newer version exists.
const SITE_URL: &str = match option_env!("OBSINK_SITE_URL") {
    Some(url) => url,
    None => "https://obsink.spencerjireh.com",
};

/// The one server this app talks to. The UI never shows or edits it.
fn default_server_url() -> String {
    let raw = std::env::var("OBSINK_SERVER_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| option_env!("OBSINK_SERVER_URL").map(str::to_string))
        .unwrap_or_else(|| FALLBACK_SERVER_URL.to_string());
    normalize_server_url(&raw)
}

#[tauri::command]
fn get_server_url() -> String {
    default_server_url()
}

/// A vault configured against another server (an older build, a moved
/// `.env`). It is shown read-only; only "Remove from this device" applies.
fn is_foreign(vault: &StoredVault) -> bool {
    vault.server_url != default_server_url()
}

/// The configured vault, refused when it belongs to another server.
fn own_vault(vault_id: Option<String>) -> Result<StoredVault, CommandError> {
    let vault = selected_vault(vault_id)?;
    if is_foreign(&vault) {
        return Err(CommandError::other(
            "Vault is on another server. Remove it from this device.",
        ));
    }
    Ok(vault)
}

fn bearer_account(server_url: &str) -> String {
    format!("bearer:{}", normalize_server_url(server_url))
}

/// The bearer stored for a server (an entry under a legacy host is moved
/// forward by core), or a "sign in first" error.
fn load_bearer(server_url: &str) -> Result<String, CommandError> {
    load_stored_bearer(server_url)
        .map_err(|_| CommandError::other(format!("not signed in to {server_url} — sign in first")))
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
    /// New accounts need an invite code once the server has any user.
    invite_required: bool,
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
    created: u64,
    expires: u64,
    /// `active`, `used`, or `expired` (the server decides).
    status: String,
    used_at: Option<u64>,
}

impl From<obsink_core::Invite> for InviteInfo {
    fn from(invite: obsink_core::Invite) -> Self {
        Self {
            code: invite.code,
            created: invite.created,
            expires: invite.expires,
            status: invite.status,
            used_at: invite.used_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct DeviceInfo {
    session_id: String,
    device_name: String,
    created: u64,
    current: bool,
}

#[tauri::command]
async fn get_auth_capabilities() -> Result<AuthCapabilities, CommandError> {
    let server_url = default_server_url();
    let caps = AuthClient::new(&server_url).capabilities().await?;
    Ok(AuthCapabilities {
        email: caps.auth.email,
        apple: caps.auth.apple,
        invite_required: caps.invite_required,
    })
}

/// Send a one-time code. Returns the code itself only against a dev server
/// (`AUTH_DEV_RETURN_CODE=1`) so harnesses can complete the flow.
#[tauri::command]
async fn auth_email_start(email: String) -> Result<Option<String>, CommandError> {
    let server_url = default_server_url();
    let result = AuthClient::new(&server_url)
        .email_start(email.trim())
        .await?;
    Ok(result.code)
}

#[tauri::command]
async fn auth_email_verify(
    email: String,
    code: String,
    invite_code: Option<String>,
    app: AppHandle,
) -> Result<AccountState, CommandError> {
    let result = auth_email_verify_inner(email, code, invite_code).await;
    reconcile_daemons(&app);
    emit_state_changed(&app, None);
    result
}

async fn auth_email_verify_inner(
    email: String,
    code: String,
    invite_code: Option<String>,
) -> Result<AccountState, CommandError> {
    let server_url = default_server_url();
    let invite = invite_code
        .as_deref()
        .map(str::trim)
        .filter(|code| !code.is_empty());
    let session = AuthClient::new(&server_url)
        .email_verify(email.trim(), code.trim(), &device_name(), invite)
        .await?;
    save_secret(&bearer_account(&server_url), &session.token)?;
    get_account().await
}

#[tauri::command]
async fn get_account() -> Result<AccountState, CommandError> {
    let server_url = default_server_url();
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
        Err(error) => {
            let error = forget_bearer_on_401(&server_url, Err::<(), _>(error.into())).unwrap_err();
            if error.kind == ErrorKind::Unauthorized {
                // Session revoked/expired elsewhere: signed out is the state.
                Ok(AccountState::SignedOut)
            } else {
                Err(error)
            }
        }
    }
}

/// Mint an invite code for someone else to create an account on this server.
#[tauri::command]
async fn create_invite() -> Result<InviteInfo, CommandError> {
    let server_url = default_server_url();
    let bearer = load_bearer(&server_url)?;
    let invite = bearer_call(
        &server_url,
        AuthClient::new(&server_url).create_invite(&bearer),
    )
    .await?;
    Ok(invite.into())
}

/// Every invite this account minted, newest first, with its status.
#[tauri::command]
async fn list_invites() -> Result<Vec<InviteInfo>, CommandError> {
    let server_url = default_server_url();
    let bearer = load_bearer(&server_url)?;
    let invites = bearer_call(
        &server_url,
        AuthClient::new(&server_url).list_invites(&bearer),
    )
    .await?;
    Ok(invites.into_iter().map(InviteInfo::from).collect())
}

/// Sign out another device of the same account. Returns the refreshed
/// account so the UI gets the new device list in one round trip.
#[tauri::command]
async fn revoke_session(session_id: String) -> Result<AccountState, CommandError> {
    let server_url = default_server_url();
    let bearer = load_bearer(&server_url)?;
    bearer_call(
        &server_url,
        AuthClient::new(&server_url).revoke_session(&bearer, &session_id),
    )
    .await?;
    get_account().await
}

/// Sign out of a server. Vault configs stay; sync will ask for a credential
/// again.
#[tauri::command]
async fn sign_out(app: AppHandle) -> Result<(), CommandError> {
    let result = sign_out_inner().await;
    reconcile_daemons(&app);
    emit_state_changed(&app, None);
    result
}

async fn sign_out_inner() -> Result<(), CommandError> {
    let server_url = default_server_url();
    let account = bearer_account(&server_url);
    if let Ok(bearer) = load_secret(&account) {
        // Best effort: the local credential goes away regardless.
        let _ = AuthClient::new(&server_url).logout(&bearer).await;
    }
    delete_secret(&account);
    Ok(())
}

/// Delete the account and everything it owns on the server, then forget the
/// bearer and every vault configured for that server. Vault folders on disk
/// stay: the files are the user's, and nothing here can recover a key.
#[tauri::command]
async fn delete_account(app: AppHandle) -> Result<(), CommandError> {
    let result = delete_account_inner().await;
    reconcile_daemons(&app);
    emit_state_changed(&app, None);
    result
}

async fn delete_account_inner() -> Result<(), CommandError> {
    let server_url = default_server_url();
    let bearer = load_bearer(&server_url)?;
    bearer_call(
        &server_url,
        AuthClient::new(&server_url).delete_account(&bearer),
    )
    .await?;
    delete_secret(&bearer_account(&server_url));
    forget_vaults_for_server(&server_url)?;
    Ok(())
}

/// Vaults the current credential can see on a server (for the Connect picker).
#[tauri::command]
async fn list_remote_vaults() -> Result<Vec<VaultSummary>, CommandError> {
    let server_url = default_server_url();
    let bearer = load_bearer(&server_url)?;
    let client = ApiClient::new(VaultConfig {
        server_url: normalize_server_url(&server_url),
        api_key: bearer,
        vault_id: String::new(),
        local_path: String::new(),
        ignore: Vec::new(),
    });
    bearer_call(&server_url, client.list_vaults()).await
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
    local_path: String,
    vault_name: String,
    vault_id: String,
    passphrase: String,
}

/// What one vault row shows. Computed on demand; nothing here is cached.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum VaultState {
    UpToDate,
    Pending {
        uploads: usize,
        downloads: usize,
    },
    /// `awaiting_resolution` when a sync stopped on these and holds a plan
    /// the user must answer; otherwise the diff predicts them and a sync
    /// will surface them.
    Conflicts {
        count: usize,
        awaiting_resolution: bool,
    },
    Syncing,
    Error {
        error_kind: ErrorKind,
        message: String,
    },
    /// Configured against another server; read-only here.
    Foreign,
    /// No key in the keychain: connect again with the passphrase.
    NoKey,
}

#[derive(Debug, Clone, Serialize)]
struct VaultStateInfo {
    id: String,
    name: String,
    local_path: String,
    server_url: String,
    state: VaultState,
    last_synced: Option<u64>,
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
fn get_vaults() -> Result<Vec<LocalVaultSummary>, CommandError> {
    let config = load_app_config()?;
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

/// Forget a vault on this device only. Refused while a sync on it runs.
fn remove_vault_inner(vault_id: &str, state: &AppState) -> Result<(), CommandError> {
    let _guard = InFlightGuard::acquire(state, vault_id)?;
    forget_vault(vault_id)?;
    set_pending_plan(state, vault_id, None)
}

#[tauri::command]
fn remove_vault(
    vault_id: String,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<(), CommandError> {
    let result = remove_vault_inner(&vault_id, &state);
    reconcile_daemons(&app);
    emit_state_changed(&app, Some(&vault_id));
    result
}

/// Delete a vault and all its files on the server, then forget it here. A
/// 404 (already gone) is reported as-is; "Remove from this device" is the
/// way out in that case.
async fn delete_remote_vault_inner(vault_id: &str, state: &AppState) -> Result<(), CommandError> {
    let vault = own_vault(Some(vault_id.to_string()))?;
    let _guard = InFlightGuard::acquire(state, &vault.id)?;
    bearer_call(
        &vault.server_url,
        ApiClient::new(to_vault_config(&vault)).delete_vault(),
    )
    .await?;
    forget_vault(&vault.id)?;
    set_pending_plan(state, &vault.id, None)
}

#[tauri::command]
async fn delete_remote_vault(
    vault_id: String,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<(), CommandError> {
    let result = delete_remote_vault_inner(&vault_id, &state).await;
    reconcile_daemons(&app);
    emit_state_changed(&app, Some(&vault_id));
    result
}

#[tauri::command]
async fn add_vault(
    request: AddVaultRequest,
    app: AppHandle,
) -> Result<LocalVaultSummary, CommandError> {
    let result = add_vault_inner(request).await;
    reconcile_daemons(&app);
    emit_state_changed(&app, result.as_ref().ok().map(|vault| vault.id.as_str()));
    result
}

async fn add_vault_inner(request: AddVaultRequest) -> Result<LocalVaultSummary, CommandError> {
    validate_request(&request)?;
    let server_url = default_server_url();
    let bearer = load_bearer(&server_url)?;

    let client = ApiClient::new(VaultConfig {
        server_url: server_url.clone(),
        api_key: bearer,
        vault_id: String::new(),
        local_path: request.local_path.clone(),
        ignore: Vec::new(),
    });

    let (vault_id, vault_name) = match request.mode {
        AddVaultMode::Create => {
            let response = bearer_call(
                &server_url,
                client.create_vault(&CreateVaultRequest {
                    name: request.vault_name.clone(),
                    max_file_size: 50 * 1024 * 1024,
                }),
            )
            .await?;
            (response.vault.id, response.vault.name)
        }
        AddVaultMode::Connect => {
            let vaults = bearer_call(&server_url, client.list_vaults()).await?;
            let vault = vaults
                .into_iter()
                .find(|vault| vault.id == request.vault_id)
                .ok_or_else(|| format!("vault {} not found", request.vault_id))?;
            (vault.id, vault.name)
        }
    };

    let key = derive_key(&request.passphrase, vault_id.as_bytes())?;
    let stored = StoredVault {
        id: vault_id.clone(),
        name: vault_name.clone(),
        server_url,
        local_path: request.local_path.clone(),
        ignore: Vec::new(),
    };

    validate_passphrase(&stored, &key).await?;
    save_key_to_keychain(&vault_id, &key)?;
    upsert_vault(stored.clone())?;

    Ok(LocalVaultSummary::from_stored(&stored, true))
}

/// The pending diff for one vault, without transferring anything.
async fn vault_diff(vault: &StoredVault) -> Result<ManifestDiff, CommandError> {
    let keys = derive_keys(&load_key_from_keychain(&vault.id)?);
    let local_root = Path::new(&vault.local_path);
    let config = to_vault_config(vault);
    let ignore = config.ignore_rules();
    // The walk hashes every file; keep it off the async runtime.
    let local = {
        let root = local_root.to_path_buf();
        let keys = keys.clone();
        let ignore = ignore.clone();
        tauri::async_runtime::spawn_blocking(move || load_local_state(&root, &keys, &ignore))
            .await
            .map_err(|error| CommandError::other(error.to_string()))??
    };
    let remote_manifest = bearer_call(
        &vault.server_url,
        fetch_remote_manifest(&ApiClient::new(config), local_root, &keys),
    )
    .await?;
    Ok(diff_local_and_remote(
        &local.base,
        &local.working,
        &remote_manifest,
        &ignore,
    ))
}

/// The state of one vault, never an error: a vault that cannot be checked
/// reports why.
async fn vault_state(vault: &StoredVault, state: &AppState) -> VaultState {
    if is_foreign(vault) {
        return VaultState::Foreign;
    }
    let in_flight = state
        .in_flight
        .lock()
        .map(|set| set.contains(&vault.id))
        .unwrap_or(false);
    if in_flight {
        return VaultState::Syncing;
    }
    let pending = state
        .pending_plans
        .lock()
        .ok()
        .and_then(|plans| plans.get(&vault.id).map(|plan| plan.conflicts.len()))
        .unwrap_or(0);
    if pending > 0 {
        return VaultState::Conflicts {
            count: pending,
            awaiting_resolution: true,
        };
    }
    if load_key_from_keychain(&vault.id).is_err() {
        return VaultState::NoKey;
    }
    match vault_diff(vault).await {
        Ok(diff) if !diff.conflicts.is_empty() => VaultState::Conflicts {
            count: diff.conflicts.len(),
            awaiting_resolution: false,
        },
        Ok(diff) if !diff.upload.is_empty() || !diff.download.is_empty() => VaultState::Pending {
            uploads: diff.upload.len(),
            downloads: diff.download.len(),
        },
        Ok(_) => VaultState::UpToDate,
        Err(error) => VaultState::Error {
            error_kind: error.kind,
            message: error.message,
        },
    }
}

/// Every configured vault with its state, in config order. Vaults are
/// checked one after another; an unchanged vault costs one conditional GET
/// (the remote manifest cache answers 304).
async fn get_vault_states_inner(state: &AppState) -> Result<Vec<VaultStateInfo>, CommandError> {
    let config = load_app_config()?;
    let mut states = Vec::with_capacity(config.vaults.len());
    for vault in &config.vaults {
        states.push(VaultStateInfo {
            id: vault.id.clone(),
            name: vault.name.clone(),
            local_path: vault.local_path.clone(),
            server_url: vault.server_url.clone(),
            state: vault_state(vault, state).await,
            last_synced: activity::last_synced(&vault.id),
        });
    }
    Ok(states)
}

#[tauri::command]
async fn get_vault_states(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<VaultStateInfo>, CommandError> {
    get_vault_states_inner(&state).await
}

/// Reveal the vault folder in Finder.
#[tauri::command]
fn open_vault_folder(vault_id: String) -> Result<(), CommandError> {
    let vault = selected_vault(Some(vault_id))?;
    let status = Command::new("open").arg(&vault.local_path).status()?;
    if !status.success() {
        return Err(CommandError::other(format!(
            "Could not open {}.",
            vault.local_path
        )));
    }
    Ok(())
}

async fn sync_vault_inner(
    vault_id: Option<String>,
    state: &AppState,
    progress: &dyn ProgressSink,
) -> Result<SyncCommandResponse, CommandError> {
    let vault = own_vault(vault_id)?;
    // A vault with a daemon syncs through it: the daemon serialises cycles
    // and its event relay keeps the state and the activity log.
    if let Some(handle) = daemon_handle(state, &vault.id) {
        let result = bearer_call(&vault.server_url, handle.sync_and_wait()).await?;
        return Ok(SyncCommandResponse {
            pending_conflicts: result.conflicts.clone(),
            completed_result: Some(result),
        });
    }
    let result = run_sync(&vault, state, progress).await;
    note_failure(&vault.id, &result);
    result
}

/// A failed cycle lands in the activity log too, so the popover can say
/// why a vault is not up to date.
fn note_failure<T>(vault_id: &str, result: &Result<T, CommandError>) {
    if let Err(error) = result {
        if let Err(io_error) = activity::record_error(vault_id, &error.message) {
            eprintln!("activity log for {vault_id} not written: {io_error}");
        }
    }
}

async fn run_sync(
    vault: &StoredVault,
    state: &AppState,
    progress: &dyn ProgressSink,
) -> Result<SyncCommandResponse, CommandError> {
    let _guard = InFlightGuard::acquire(state, &vault.id)?;
    let key = load_key_from_keychain(&vault.id)?;
    // A fresh cycle supersedes any plan left over from an earlier one.
    set_pending_plan(state, &vault.id, None)?;
    let plan = bearer_call(
        &vault.server_url,
        prepare_sync(&to_vault_config(vault), &key, progress),
    )
    .await?;

    if plan.conflicts.is_empty() {
        let result = bearer_call(
            &vault.server_url,
            complete_sync(&to_vault_config(vault), &key, &plan, &[], progress),
        )
        .await?;
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
) -> Result<(), CommandError> {
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
) -> Result<SyncCommandResponse, CommandError> {
    let late_plan = SyncPlan::from_late_conflicts(&result);
    let pending_conflicts = result.conflicts.clone();
    set_pending_plan(state, vault_id, late_plan)?;
    if let Err(io_error) = activity::record_sync(vault_id, &result) {
        eprintln!("activity log for {vault_id} not written: {io_error}");
    }
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
) -> Result<SyncCommandResponse, CommandError> {
    let vault_id = selected_vault(vault_id)?.id;
    let sink = TauriProgressSink {
        app: app.clone(),
        vault_id: vault_id.clone(),
    };
    let result = sync_vault_inner(Some(vault_id.clone()), &state, &sink).await;
    emit_state_changed(&app, Some(&vault_id));
    result
}

async fn resolve_conflict_inner(
    vault_id: String,
    resolutions: Vec<ConflictResolution>,
    state: &AppState,
    progress: &dyn ProgressSink,
) -> Result<SyncCommandResponse, CommandError> {
    let vault = own_vault(Some(vault_id))?;
    if let Some(handle) = daemon_handle(state, &vault.id) {
        let result = bearer_call(&vault.server_url, handle.resolve(resolutions)).await?;
        return Ok(SyncCommandResponse {
            pending_conflicts: result.conflicts.clone(),
            completed_result: Some(result),
        });
    }
    let result = run_resolve(&vault, resolutions, state, progress).await;
    note_failure(&vault.id, &result);
    result
}

async fn run_resolve(
    vault: &StoredVault,
    resolutions: Vec<ConflictResolution>,
    state: &AppState,
    progress: &dyn ProgressSink,
) -> Result<SyncCommandResponse, CommandError> {
    let _guard = InFlightGuard::acquire(state, &vault.id)?;
    // The plan stays in place until the round succeeds, so a failed attempt
    // (network, keychain) can be retried without a fresh sync.
    let plan = state
        .pending_plans
        .lock()
        .map_err(|_| "pending plan lock poisoned".to_string())?
        .get(&vault.id)
        .cloned()
        .ok_or_else(|| format!("no pending conflict set for {}", vault.id))?;
    let key = load_key_from_keychain(&vault.id)?;

    let result = bearer_call(
        &vault.server_url,
        complete_sync(&to_vault_config(vault), &key, &plan, &resolutions, progress),
    )
    .await?;
    finish_cycle(state, &vault.id, result)
}

#[tauri::command]
async fn resolve_conflict(
    vault_id: String,
    resolutions: Vec<ConflictResolution>,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<SyncCommandResponse, CommandError> {
    let sink = TauriProgressSink {
        app: app.clone(),
        vault_id: vault_id.clone(),
    };
    let result = resolve_conflict_inner(vault_id.clone(), resolutions, &state, &sink).await;
    emit_state_changed(&app, Some(&vault_id));
    result
}

async fn get_conflict_preview_inner(
    vault_id: String,
    path: String,
    state: &AppState,
) -> Result<ConflictPreview, CommandError> {
    let vault = own_vault(Some(vault_id.clone()))?;
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

    let keys = derive_keys(&load_key_from_keychain(&vault.id)?);
    let client = ApiClient::new(to_vault_config(&vault));

    let local_text = if conflict.local.deleted {
        String::new()
    } else {
        let bytes = fs::read(Path::new(&vault.local_path).join(&conflict.path))?;
        String::from_utf8_lossy(&bytes).into_owned()
    };

    let remote_text = if conflict.remote.deleted {
        String::new()
    } else {
        let blob = bearer_call(&vault.server_url, client.get_file(&conflict.path, &keys)).await?;
        let bytes = obsink_core::decrypt(&keys.content_enc, &blob)?;
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
) -> Result<ConflictPreview, CommandError> {
    get_conflict_preview_inner(vault_id, path, &state).await
}

fn validate_request(request: &AddVaultRequest) -> Result<(), CommandError> {
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

async fn validate_passphrase(vault: &StoredVault, key: &KeyBytes) -> Result<(), CommandError> {
    let keys = derive_keys(key);
    let client = ApiClient::new(to_vault_config(vault));
    let manifest = bearer_call(
        &vault.server_url,
        fetch_remote_manifest(&client, Path::new(&vault.local_path), &keys),
    )
    .await?;
    if let Some((path, _)) = manifest.iter().find(|(_, entry)| !entry.deleted) {
        let blob = bearer_call(&vault.server_url, client.get_file(path, &keys)).await?;
        obsink_core::decrypt(&keys.content_enc, &blob)?;
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

/// Bearer comes from the keychain; if it is missing the request goes out
/// without one and the server's 401 surfaces as `ApiError::Unauthorized`
/// ("sign in again"), which is the message the user needs.
fn to_vault_config(vault: &StoredVault) -> VaultConfig {
    VaultConfig {
        server_url: vault.server_url.clone(),
        api_key: load_stored_bearer(&vault.server_url).unwrap_or_default(),
        vault_id: vault.id.clone(),
        local_path: vault.local_path.clone(),
        ignore: vault.ignore.clone(),
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

/// Drop a vault from this device: config entry and keychain key. The local
/// folder (including `.obsink/`) is left alone; reconnecting later needs the
/// passphrase again and resumes from that checkpoint.
fn forget_vault(vault_id: &str) -> Result<(), io::Error> {
    let mut config = load_app_config()?;
    config.vaults.retain(|vault| vault.id != vault_id);
    if config.active_vault_id.as_deref() == Some(vault_id) {
        config.active_vault_id = config.vaults.first().map(|vault| vault.id.clone());
    }
    save_app_config(&config)?;
    delete_secret(vault_id);
    activity::forget(vault_id);
    Ok(())
}

/// The newest activity across every configured vault, or one of them.
#[tauri::command]
fn list_activity(
    vault_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<ActivityEvent>, CommandError> {
    let ids: Vec<String> = load_app_config()?
        .vaults
        .into_iter()
        .map(|vault| vault.id)
        .collect();
    Ok(activity::list(
        &ids,
        vault_id.as_deref(),
        limit.unwrap_or(activity::MAX_EVENTS),
    ))
}

/// `forget_vault` for every vault on one server (account deletion).
fn forget_vaults_for_server(server_url: &str) -> Result<(), io::Error> {
    let server_url = normalize_server_url(server_url);
    let ids: Vec<String> = load_app_config()?
        .vaults
        .iter()
        .filter(|vault| vault.server_url == server_url)
        .map(|vault| vault.id.clone())
        .collect();
    for id in ids {
        forget_vault(&id)?;
    }
    Ok(())
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

// --- Errors -------------------------------------------------------------------

/// What the UI needs to know about a failed command beyond the message: a
/// 401 opens the sign-in form, a transport failure is retryable, a server
/// status can be matched (403 invite gating) without parsing the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ErrorKind {
    /// The bearer was rejected; the local credential has been forgotten.
    Unauthorized,
    /// The request never got a response (DNS, refused, timeout, TLS).
    Network,
    /// The server answered with an error status; `status` carries it.
    Server,
    Other,
}

#[derive(Debug, Clone, Serialize)]
struct CommandError {
    kind: ErrorKind,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
}

impl CommandError {
    fn other(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Other,
            message: message.into(),
            status: None,
        }
    }

    fn from_status(status: u16, message: String) -> Self {
        if status == 401 {
            Self {
                kind: ErrorKind::Unauthorized,
                message: "unauthorized: sign in again".into(),
                status: Some(status),
            }
        } else {
            Self {
                kind: ErrorKind::Server,
                message,
                status: Some(status),
            }
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self::other(message)
    }
}

impl From<&str> for CommandError {
    fn from(message: &str) -> Self {
        Self::other(message)
    }
}

impl From<io::Error> for CommandError {
    fn from(error: io::Error) -> Self {
        Self::other(error.to_string())
    }
}

impl From<obsink_core::CryptoError> for CommandError {
    fn from(error: obsink_core::CryptoError) -> Self {
        Self::other(error.to_string())
    }
}

impl From<AuthError> for CommandError {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::Http(error) => Self {
                kind: ErrorKind::Network,
                message: error.to_string(),
                status: None,
            },
            AuthError::Server { status, message } => Self::from_status(status.as_u16(), message),
        }
    }
}

impl From<ApiError> for CommandError {
    fn from(error: ApiError) -> Self {
        match error {
            ApiError::Http(error) => Self {
                kind: ErrorKind::Network,
                message: error.to_string(),
                status: None,
            },
            ApiError::Unauthorized => Self::from_status(401, String::new()),
            ApiError::UnexpectedStatus { status, body } => {
                Self::from_status(status.as_u16(), server_error_message(&body))
            }
            other @ (ApiError::Crypto(_) | ApiError::Conflict { .. }) => {
                Self::other(other.to_string())
            }
        }
    }
}

impl From<SyncEngineError> for CommandError {
    fn from(error: SyncEngineError) -> Self {
        match error {
            SyncEngineError::Api(error) => error.into(),
            other => Self::other(other.to_string()),
        }
    }
}

impl From<DaemonCallError> for CommandError {
    fn from(error: DaemonCallError) -> Self {
        match error {
            DaemonCallError::Sync(error) => error.into(),
            DaemonCallError::Stopped => Self::other(error.to_string()),
        }
    }
}

/// Server error bodies are `{ "error": "<text>" }`; show the text alone.
fn server_error_message(body: &str) -> String {
    #[derive(Deserialize)]
    struct Body {
        error: String,
    }
    serde_json::from_str::<Body>(body)
        .map(|body| body.error)
        .unwrap_or_else(|_| body.to_string())
}

/// A 401 means the session is gone (revoked, expired, account deleted), so
/// the stored bearer is useless: forget it, and `get_account` reports
/// signed-out until the user signs in again.
fn forget_bearer_on_401<T>(
    server_url: &str,
    result: Result<T, CommandError>,
) -> Result<T, CommandError> {
    if let Err(error) = &result {
        if error.kind == ErrorKind::Unauthorized {
            delete_secret(&bearer_account(server_url));
        }
    }
    result
}

/// Run one bearer-authenticated request and apply the 401 policy to it.
async fn bearer_call<T, E: Into<CommandError>>(
    server_url: &str,
    request: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, CommandError> {
    forget_bearer_on_401(server_url, request.await.map_err(Into::into))
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
/// Gap between the menu-bar icon and the popover, in physical pixels.
const POPOVER_GAP: i32 = 6;

/// Which tab (and vault) the settings window should show; sent by the
/// popover through `open_settings` and by the tray menu.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SettingsTarget {
    tab: String,
    #[serde(default)]
    vault_id: Option<String>,
    #[serde(default)]
    add_vault: bool,
}

impl SettingsTarget {
    fn vaults() -> Self {
        Self {
            tab: "vaults".to_string(),
            vault_id: None,
            add_vault: false,
        }
    }
}

fn show_settings(app: &AppHandle, target: SettingsTarget) -> tauri::Result<()> {
    if let Some(popover) = app.get_webview_window("popover") {
        let _ = popover.hide();
    }
    let Some(window) = app.get_webview_window("settings") else {
        return Ok(());
    };
    // An accessory app has no Dock icon to click; activate it explicitly so
    // the window takes keyboard focus.
    #[cfg(target_os = "macos")]
    let _ = app.show();
    window.show()?;
    window.unminimize()?;
    window.set_focus()?;
    app.emit_to("settings", "settings://navigate", target)?;
    Ok(())
}

/// Open (or raise) the settings window at a tab, from the popover.
#[tauri::command]
fn open_settings(target: SettingsTarget, app: AppHandle) -> Result<(), CommandError> {
    show_settings(&app, target).map_err(|error| CommandError::other(error.to_string()))
}

/// Put the popover centred under the menu-bar icon, kept inside the work
/// area of the display the icon is on.
fn position_popover(
    app: &AppHandle,
    window: &WebviewWindow,
    anchor: tauri::Rect,
) -> tauri::Result<()> {
    let probe = anchor.position.to_physical::<f64>(1.0);
    let monitor = match app.monitor_from_point(probe.x, probe.y)? {
        Some(monitor) => Some(monitor),
        None => app.primary_monitor()?,
    };
    let Some(monitor) = monitor else {
        return Ok(());
    };
    let scale = monitor.scale_factor();
    let icon_pos = anchor.position.to_physical::<i32>(scale);
    let icon_size = anchor.size.to_physical::<i32>(scale);
    let size = window.outer_size()?;
    let area = monitor.work_area();
    let min_x = area.position.x;
    let max_x = (area.position.x + area.size.width as i32 - size.width as i32).max(min_x);
    let x = (icon_pos.x + icon_size.width / 2 - size.width as i32 / 2).clamp(min_x, max_x);
    let y = (icon_pos.y + icon_size.height + POPOVER_GAP).max(area.position.y);
    window.set_position(PhysicalPosition::new(x, y))
}

fn recently_hidden(app: &AppHandle, within: Duration) -> bool {
    app.state::<AppState>()
        .popover_hidden_at
        .lock()
        .ok()
        .and_then(|at| *at)
        .is_some_and(|at| at.elapsed() < within)
}

fn note_popover_hidden(app: &AppHandle) {
    if let Ok(mut at) = app.state::<AppState>().popover_hidden_at.lock() {
        *at = Some(Instant::now());
    }
}

/// Tray click: show the popover under the icon, or hide it if it is up.
fn toggle_popover(app: &AppHandle, anchor: tauri::Rect) {
    let Some(window) = app.get_webview_window("popover") else {
        return;
    };
    if recently_hidden(app, Duration::from_millis(300)) {
        return;
    }
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        return;
    }
    let _ = position_popover(app, &window, anchor);
    let _ = window.show();
    let _ = window.set_focus();
    let _ = app.emit_to("popover", "popover://opened", ());
}

/// Build the menu-bar tray icon and wire its menu and click behavior.
fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let sync_now = MenuItem::with_id(app, "sync_now", "Sync now", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Open settings", true, None::<&str>)?;
    let updates = MenuItem::with_id(
        app,
        "check_updates",
        "Check for updates",
        true,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit ObSink", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&sync_now, &settings, &updates, &separator, &quit])?;

    let builder = TrayIconBuilder::with_id("obsink-tray")
        .tooltip("ObSink")
        .menu(&menu)
        // Left click toggles the popover; the menu stays on right click.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "sync_now" => {
                // The popover owns the sync-all flow; it runs whether or not
                // it is visible.
                let _ = app.emit_to("popover", "tray://sync-now", ());
            }
            "settings" => {
                let _ = show_settings(app, SettingsTarget::vaults());
            }
            "check_updates" => {
                // The download page shows the latest release; the app's own
                // version is in the settings window's Account tab.
                let _ = tauri_plugin_opener::open_url(format!("{SITE_URL}/#mac"), None::<&str>);
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                toggle_popover(tray.app_handle(), rect);
            }
        });

    // The menu bar renders template images as a silhouette tinted to the bar
    // colour, so the tray gets its own monochrome mark (design/tray.svg via
    // scripts/gen-icons.sh) instead of the full-colour app icon.
    builder
        .icon(tauri::include_image!("icons/tray.png"))
        .icon_as_template(true)
        .build(app)?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        // Opens the download page in the default browser (Rust side only; no
        // capability exposes it to the webview).
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            add_vault,
            auth_email_start,
            auth_email_verify,
            create_invite,
            list_invites,
            revoke_session,
            delete_account,
            get_account,
            get_auth_capabilities,
            get_server_url,
            list_activity,
            list_remote_vaults,
            sign_out,
            get_conflict_preview,
            get_vault_states,
            open_vault_folder,
            open_settings,
            get_vaults,
            resolve_conflict,
            remove_vault,
            delete_remote_vault,
            sync_vault,
        ])
        .setup(|app| {
            // Menu-bar app: no Dock icon, no app switcher entry.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            setup_tray(app.handle())?;
            reconcile_daemons(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| match (window.label(), event) {
            // The popover is transient: it goes away with focus.
            ("popover", WindowEvent::Focused(false)) => {
                let _ = window.hide();
                note_popover_hidden(window.app_handle());
            }
            // Closing settings hides it; the app lives in the menu bar.
            ("settings", WindowEvent::CloseRequested { api, .. }) => {
                let _ = window.hide();
                api.prevent_close();
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Tests that sandbox `HOME` share the process environment, so they take
/// this lock first.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod live_tests {
    use super::*;
    use obsink_core::{derive_keys, ApiClient, ConflictResolutionChoice, VaultConfig};
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    /// `#[ignore]`d live integration test: drives the real desktop command
    /// functions (add_vault / get_vault_states / sync_vault_inner /
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
        std::env::set_var("OBSINK_SERVER_URL", &server_url);
        // The desktop has no API-key entry any more; seed the operator bearer
        // the way a sign-in would.
        save_secret(&bearer_account(&server_url), &api_key).unwrap();

        let state = AppState::default();
        let file_rel = "notes/a.md";

        // ===== OBS-3: add (Create) + upload + cross-device download =====
        let summary = add_vault_inner(AddVaultRequest {
            mode: AddVaultMode::Create,
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
        add_vault_inner(AddVaultRequest {
            mode: AddVaultMode::Connect,
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
        connect_local(&vault_id, &passphrase, &dir_a).await;
        let states = get_vault_states_inner(&state).await.unwrap();
        let mine = states.iter().find(|s| s.id == vault_id).unwrap();
        assert!(
            matches!(mine.state, VaultState::Pending { downloads, .. } if downloads >= 1),
            "OBS-5: expected remote changes pending (b.md) -> banner source, got {:?}",
            mine.state
        );
        assert!(
            mine.last_synced.is_some(),
            "last_synced set by the sync above"
        );
        let activity = list_activity(Some(vault_id.clone()), None).unwrap();
        assert!(
            activity
                .iter()
                .any(|e| e.kind == activity::ActivityKind::Synced),
            "activity log holds the Synced summary"
        );
        assert!(
            activity
                .iter()
                .any(|e| e.kind == activity::ActivityKind::Uploaded
                    && e.path.as_deref() == Some(file_rel)),
            "activity log holds the upload of {file_rel}"
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
                ConflictResolutionChoice::Defer => unreachable!("the UI never defers"),
            }
            println!("OBS-4 ({choice:?}): resolution verified");
        }

        // ===== OBS-6: multiple-vault switching =====
        let s2 = add_vault_inner(AddVaultRequest {
            mode: AddVaultMode::Create,
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

        // Both vaults report a state; a vault pointed at another server is
        // read-only and never contacted.
        let states = get_vault_states_inner(&state).await.unwrap();
        assert_eq!(states.len(), 2, "OBS-6: two vault states");
        assert!(states
            .iter()
            .all(|s| !matches!(s.state, VaultState::Error { .. })));
        let mut config = load_app_config().unwrap();
        config
            .vaults
            .iter_mut()
            .find(|v| v.id == vault_id_2)
            .unwrap()
            .server_url = "https://elsewhere.example".to_string();
        save_app_config(&config).unwrap();
        let states = get_vault_states_inner(&state).await.unwrap();
        let foreign = states.iter().find(|s| s.id == vault_id_2).unwrap();
        assert!(matches!(foreign.state, VaultState::Foreign));
        let refused = sync_vault_inner(Some(vault_id_2.clone()), &state, &obsink_core::NoProgress)
            .await
            .unwrap_err();
        assert!(refused.message.contains("another server"));
        println!("OBS-6: multi-vault states verified");

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
        fs::write(dir.join("note.md"), "# note\n").unwrap();
        std::env::set_var("HOME", &sandbox);
        let keyring_dir = sandbox.join("keyring");
        fs::create_dir_all(&keyring_dir).unwrap();
        std::env::set_var("OBSINK_KEYRING_DIR", &keyring_dir);
        std::env::set_var("OBSINK_SERVER_URL", &server_url);

        assert!(matches!(
            get_account().await.unwrap(),
            AccountState::SignedOut
        ));
        // Without a credential, adding a vault must fail with a sign-in hint.
        let denied = add_vault_inner(AddVaultRequest {
            mode: AddVaultMode::Create,
            local_path: dir.to_string_lossy().into_owned(),
            vault_name: "denied".to_string(),
            vault_id: String::new(),
            passphrase: "pw".to_string(),
        })
        .await
        .unwrap_err();
        assert!(denied.message.contains("sign in"), "{denied}");

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
        let code = auth_email_start(email.clone())
            .await
            .unwrap()
            .expect("dev server returns the code inline");
        let state = auth_email_verify_inner(email.clone(), code, bootstrap_invite)
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

        let summary = add_vault_inner(AddVaultRequest {
            mode: AddVaultMode::Create,
            local_path: dir.to_string_lossy().into_owned(),
            vault_name: "desktop-account-vault".to_string(),
            vault_id: String::new(),
            passphrase: "pw".to_string(),
        })
        .await
        .unwrap();
        // app.json must not contain the bearer.
        let app_json = fs::read_to_string(sandbox.join(".obsink/app.json")).unwrap();
        assert!(!app_json.contains("os_"), "{app_json}");
        assert!(!app_json.contains("api_key"), "{app_json}");

        let listed = list_remote_vaults().await.unwrap();
        assert_eq!(listed.iter().filter(|v| v.id == summary.id).count(), 1);

        let state = AppState::default();
        let response = sync_vault_inner(Some(summary.id.clone()), &state, &obsink_core::NoProgress)
            .await
            .unwrap();
        assert!(response.completed_result.is_some());

        // Invite gating: a second account needs a code minted by the first.
        let invite = create_invite().await.unwrap();
        assert!(!invite.code.is_empty());
        let keyring_b = sandbox.join("keyring-b");
        fs::create_dir_all(&keyring_b).unwrap();
        std::env::set_var("OBSINK_KEYRING_DIR", &keyring_b);
        let second = format!("desktop-b-{}@example.com", std::process::id());
        let code = auth_email_start(second.clone()).await.unwrap().unwrap();
        let refused = auth_email_verify_inner(second.clone(), code.clone(), None)
            .await
            .unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Server, "{refused}");
        assert_eq!(refused.status, Some(403), "{refused}");
        assert!(refused.message.contains("invite"), "{refused}");
        let accepted = auth_email_verify_inner(second.clone(), code, Some(invite.code.clone()))
            .await
            .unwrap();
        assert!(matches!(accepted, AccountState::Account { .. }));
        std::env::set_var("OBSINK_KEYRING_DIR", &keyring_dir);

        // Capabilities: with an account on the server, new sign-ups need an
        // invite, and the invite list shows the redeemed code as used.
        let caps = get_auth_capabilities().await.unwrap();
        assert!(caps.email && caps.invite_required, "{caps:?}");
        let invites = list_invites().await.unwrap();
        let used = invites
            .iter()
            .find(|item| item.code == invite.code)
            .expect("minted invite is listed");
        assert_eq!(used.status, "used");
        assert!(used.used_at.is_some());
        let fresh = create_invite().await.unwrap();
        assert_eq!(fresh.status, "active");
        assert!(list_invites()
            .await
            .unwrap()
            .iter()
            .any(|item| item.code == fresh.code && item.status == "active"));

        // Devices: a second session of account A, revoked from the first.
        let token_a = load_secret(&bearer_account(&server_url)).unwrap();
        let keyring_a2 = sandbox.join("keyring-a2");
        fs::create_dir_all(&keyring_a2).unwrap();
        std::env::set_var("OBSINK_KEYRING_DIR", &keyring_a2);
        let code = start_code_after_cooldown(&email).await;
        auth_email_verify_inner(email.clone(), code, None)
            .await
            .unwrap();
        std::env::set_var("OBSINK_KEYRING_DIR", &keyring_dir);
        let (own_session, other_session) = match get_account().await.unwrap() {
            AccountState::Account { devices, .. } => {
                assert_eq!(devices.len(), 2, "{devices:?}");
                assert_eq!(devices.iter().filter(|device| device.current).count(), 1);
                let pick = |current: bool| {
                    devices
                        .iter()
                        .find(|device| device.current == current)
                        .unwrap()
                        .session_id
                        .clone()
                };
                (pick(true), pick(false))
            }
            other => panic!("expected account, got {other:?}"),
        };
        match revoke_session(other_session).await.unwrap() {
            AccountState::Account { devices, .. } => assert_eq!(devices.len(), 1),
            other => panic!("expected account, got {other:?}"),
        }
        // The revoked session is signed out, and its 401 forgets the bearer.
        std::env::set_var("OBSINK_KEYRING_DIR", &keyring_a2);
        assert!(matches!(
            get_account().await.unwrap(),
            AccountState::SignedOut
        ));
        assert!(load_secret(&bearer_account(&server_url)).is_err());
        // Another account cannot revoke A's session: the server says 404.
        std::env::set_var("OBSINK_KEYRING_DIR", &keyring_b);
        let cross = revoke_session(own_session).await.unwrap_err();
        assert_eq!(cross.kind, ErrorKind::Server, "{cross}");
        assert_eq!(cross.status, Some(404), "{cross}");
        std::env::set_var("OBSINK_KEYRING_DIR", &keyring_dir);

        // A bogus bearer on a vault command: Unauthorized, bearer forgotten.
        save_secret(&bearer_account(&server_url), "os_bogus").unwrap();
        let err = sync_vault_inner(Some(summary.id.clone()), &state, &obsink_core::NoProgress)
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unauthorized, "{err}");
        assert!(load_secret(&bearer_account(&server_url)).is_err());
        save_secret(&bearer_account(&server_url), &token_a).unwrap();

        // Remove from this device: config and key go, files stay, the other
        // vault becomes active.
        let dir2 = sandbox.join("vault2");
        fs::create_dir_all(&dir2).unwrap();
        let second_vault = add_vault_inner(AddVaultRequest {
            mode: AddVaultMode::Create,
            local_path: dir2.to_string_lossy().into_owned(),
            vault_name: "desktop-second-vault".to_string(),
            vault_id: String::new(),
            passphrase: "pw".to_string(),
        })
        .await
        .unwrap();
        assert!(second_vault.active);
        remove_vault_inner(&second_vault.id, &state).unwrap();
        let remaining = get_vaults().unwrap();
        assert_eq!(remaining.len(), 1);
        assert!(remaining[0].active && remaining[0].id == summary.id);
        assert!(load_key_from_keychain(&second_vault.id).is_err());
        assert!(dir2.exists());
        assert!(list_remote_vaults()
            .await
            .unwrap()
            .iter()
            .any(|vault| vault.id == second_vault.id));

        // Delete on server: gone from the account, folder intact.
        delete_remote_vault_inner(&summary.id, &state)
            .await
            .unwrap();
        assert!(get_vaults().unwrap().is_empty());
        assert!(load_key_from_keychain(&summary.id).is_err());
        assert!(!list_remote_vaults()
            .await
            .unwrap()
            .iter()
            .any(|vault| vault.id == summary.id));
        assert!(dir.join("note.md").exists());
        let err = delete_remote_vault_inner(&summary.id, &state)
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Other, "not configured: {err}");

        // Delete account B: signed out, bearer gone, account unknown.
        std::env::set_var("OBSINK_KEYRING_DIR", &keyring_b);
        let dir_b = sandbox.join("vault-b");
        fs::create_dir_all(&dir_b).unwrap();
        add_vault_inner(AddVaultRequest {
            mode: AddVaultMode::Create,
            local_path: dir_b.to_string_lossy().into_owned(),
            vault_name: "desktop-b-vault".to_string(),
            vault_id: String::new(),
            passphrase: "pw".to_string(),
        })
        .await
        .unwrap();
        let token_b = load_secret(&bearer_account(&server_url)).unwrap();
        delete_account_inner().await.unwrap();
        assert!(matches!(
            get_account().await.unwrap(),
            AccountState::SignedOut
        ));
        assert!(load_secret(&bearer_account(&server_url)).is_err());
        assert!(get_vaults().unwrap().is_empty());
        // The old session is dead on the server too.
        let gone = AuthClient::new(&server_url).me(&token_b).await.unwrap_err();
        assert!(
            matches!(gone, AuthError::Server { status, .. } if status.as_u16() == 401),
            "{gone}"
        );
        std::env::set_var("OBSINK_KEYRING_DIR", &keyring_dir);

        sign_out_inner().await.unwrap();
        assert!(matches!(
            get_account().await.unwrap(),
            AccountState::SignedOut
        ));

        println!("ACCOUNT FLOW VERIFIED: vault={}", summary.id);
        let _ = fs::remove_dir_all(&sandbox);
    }

    /// No server: `forget_vault` keeps the others, reassigns the active id,
    /// and drops the key; `forget_vaults_for_server` is per server.
    #[tokio::test]
    async fn forget_vault_updates_config_and_keyring() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let sandbox = PathBuf::from(format!("/tmp/obsink-desktop-forget-{}", std::process::id()));
        let _ = fs::remove_dir_all(&sandbox);
        fs::create_dir_all(sandbox.join("keyring")).unwrap();
        std::env::set_var("HOME", &sandbox);
        std::env::set_var("OBSINK_KEYRING_DIR", sandbox.join("keyring"));

        for (id, url) in [
            ("vault_a", "https://one.example"),
            ("vault_b", "https://one.example"),
            ("vault_c", "https://two.example"),
        ] {
            upsert_vault(StoredVault {
                id: id.to_string(),
                name: id.to_string(),
                server_url: url.to_string(),
                local_path: sandbox.join(id).to_string_lossy().into_owned(),
                ignore: Vec::new(),
            })
            .unwrap();
            save_key_to_keychain(id, &[7_u8; 32]).unwrap();
        }
        let mut config = load_app_config().unwrap();
        config.active_vault_id = Some("vault_a".to_string());
        save_app_config(&config).unwrap();

        forget_vault("vault_a").unwrap();
        let config = load_app_config().unwrap();
        assert_eq!(config.active_vault_id.as_deref(), Some("vault_b"));
        assert_eq!(config.vaults.len(), 2);
        assert!(load_key_from_keychain("vault_a").is_err());
        assert!(load_key_from_keychain("vault_b").is_ok());

        forget_vaults_for_server("https://ONE.example/").unwrap();
        let config = load_app_config().unwrap();
        assert_eq!(config.vaults.len(), 1);
        assert_eq!(config.vaults[0].id, "vault_c");
        assert_eq!(config.active_vault_id.as_deref(), Some("vault_c"));
        assert!(load_key_from_keychain("vault_c").is_ok());

        forget_vault("vault_c").unwrap();
        assert!(load_app_config().unwrap().active_vault_id.is_none());
        let _ = fs::remove_dir_all(&sandbox);
    }

    /// `POST /auth/email/start` refuses a second code within 60 s; a live
    /// test that signs the same address in twice waits it out.
    async fn start_code_after_cooldown(email: &str) -> String {
        for _ in 0..40 {
            match auth_email_start(email.to_string()).await {
                Ok(code) => return code.expect("dev server returns the code inline"),
                Err(error) if error.status == Some(429) => {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                Err(error) => panic!("{error}"),
            }
        }
        panic!("email cooldown never cleared");
    }

    fn env_or_panic(key: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| panic!("set {key}"))
    }

    async fn connect_local(vault_id: &str, passphrase: &str, local_path: &Path) {
        add_vault_inner(AddVaultRequest {
            mode: AddVaultMode::Connect,
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
            ignore: Vec::new(),
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
            ignore: Vec::new(),
        };
        let blob = ApiClient::new(config).get_file(path, &keys).await.unwrap();
        let bytes = obsink_core::decrypt(&keys.content_enc, &blob).unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}
