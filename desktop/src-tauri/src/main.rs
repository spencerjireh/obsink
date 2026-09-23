use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use dirs::home_dir;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow, WindowEvent,
};

use obsink_core::{
    complete_sync, create_account_key, daemon_channel, decode_base64, derive_keys,
    diff_local_and_remote, encode_base64, fetch_remote_manifest,
    keychain::{
        bearer_account, delete_account_key, delete_secret, load_account_key,
        load_bearer as load_stored_bearer, load_or_create_device_id, load_secret, load_secret_opt,
        save_account_key, save_secret, user_account,
    },
    load_local_state, new_key, new_vault_id, normalize_server_url, prepare_sync,
    rewrap_account_key, run_daemon, sync_manifest_path, unwrap_vault_key, wrap_vault_key,
    write_atomic, ApiClient, ApiError, AuthClient, AuthError, Conflict, ConflictResolution,
    CreateVaultRequest, DaemonCallError, DaemonEvent, DaemonHandle, DaemonOptions, Device,
    DevicePlatform, KeyBytes, ManifestDiff, ProgressEvent, ProgressSink, SetKeysOutcome,
    SyncEngineError, SyncPlan, SyncResult, VaultConfig, VaultSummary, PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};

mod activity;
#[cfg(debug_assertions)]
mod automation;
use activity::ActivityEvent;

const APP_CONFIG_FILE: &str = ".obsink/app.json";
const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;
/// Spec §6.1: the wrapped account key is the new exposure, so the passphrase
/// has a floor (the same one the shared UI enforces).
const MIN_PASSPHRASE_CHARS: usize = 12;

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

/// Stop one vault's daemon now (the folder is about to move); the next
/// `reconcile_daemons` starts a fresh one on the new path.
fn stop_daemon(state: &AppState, vault_id: &str) {
    if let Ok(mut daemons) = state.daemons.lock() {
        if let Some(handle) = daemons.remove(vault_id) {
            handle.stop();
        }
    }
}

/// Start daemons for every vault that can sync (key in the keychain, signed
/// in) and stop the ones whose vault no longer can. Called at launch and
/// after anything that changes that set.
fn reconcile_daemons(app: &AppHandle) {
    let state = app.state::<AppState>();
    let config = match load_app_config() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("daemons not started: {error}");
            return;
        }
    };
    let signed_in = load_stored_bearer(&default_server_url()).is_ok();
    let wanted: HashMap<String, StoredVault> = config
        .vaults
        .into_iter()
        .filter(|vault| signed_in && load_key_from_keychain(&vault.id).is_ok())
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
            return Err(CommandError::other(
                "A sync is running on this vault. Wait for it to finish.",
            ));
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

// --- Config -------------------------------------------------------------------

/// `~/.obsink/app.json`: the vaults this Mac holds. A pre-v3 file carried a
/// `server_url` per vault and an `active_vault_id`; both are ignored on read
/// and gone after the next write (every build talks to one server, and the
/// list has no notion of an active vault).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoredAppConfig {
    vaults: Vec<StoredVault>,
}

/// One vault this Mac holds. Nothing secret: the bearer, the account key and
/// the vault key live in the keychain.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredVault {
    id: String,
    name: String,
    local_path: String,
    /// Extra ignore patterns for this vault, on top of the built-in defaults.
    #[serde(default)]
    ignore: Vec<String>,
}

/// What `create_vault` and `download_vault` return (ui `LocalVault`).
#[derive(Debug, Clone, Serialize)]
struct LocalVault {
    id: String,
    name: String,
    local_path: String,
}

impl From<&StoredVault> for LocalVault {
    fn from(vault: &StoredVault) -> Self {
        Self {
            id: vault.id.clone(),
            name: vault.name.clone(),
            local_path: vault.local_path.clone(),
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

/// The bearer stored for a server (an entry under a legacy host is moved
/// forward by core), or a "sign in first" error.
fn load_bearer(server_url: &str) -> Result<String, CommandError> {
    load_stored_bearer(server_url)
        .map_err(|_| CommandError::other(format!("not signed in to {server_url} — sign in first")))
}

/// The signed-in account's user id: recorded at sign-in, or asked from the
/// server once and recorded then (an older build's session has no record).
async fn user_id_for(server_url: &str, bearer: &str) -> Result<String, CommandError> {
    if let Some(user_id) = load_secret_opt(&user_account(server_url))? {
        return Ok(user_id);
    }
    let me = bearer_call(server_url, AuthClient::new(server_url).me(bearer)).await?;
    let user = me
        .user
        .ok_or_else(|| CommandError::other("Sign in first."))?;
    save_secret(&user_account(server_url), &user.id)?;
    Ok(user.id)
}

/// The bearer and user id of the signed-in account.
async fn signed_in(server_url: &str) -> Result<(String, String), CommandError> {
    let bearer = load_bearer(server_url)?;
    let user_id = user_id_for(server_url, &bearer).await?;
    Ok((bearer, user_id))
}

/// The bearer, the user id and the unlocked account key, or the message the
/// UI shows for a vault action before the unlock (DESIGN.md §5).
async fn account_key_for(server_url: &str) -> Result<(String, String, KeyBytes), CommandError> {
    let (bearer, user_id) = signed_in(server_url).await?;
    let (key, _) =
        load_account_key(&user_id).map_err(|_| CommandError::other("Set a passphrase first."))?;
    Ok((bearer, user_id, key))
}

/// This Mac as the server knows it: the stable id from the keychain (shared
/// with the CLI), the computer's name, the platform.
fn this_device(server_url: &str) -> Result<Device, CommandError> {
    Ok(Device {
        id: load_or_create_device_id(server_url)?,
        name: device_name(),
        platform: DevicePlatform::Macos,
    })
}

fn device_name() -> String {
    Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Mac".to_string())
}

/// An API client for account-level vault calls (the list, a create) or for
/// one vault before it is stored.
fn api_client(server_url: &str, bearer: String, vault_id: &str, local_path: &str) -> ApiClient {
    ApiClient::new(VaultConfig {
        server_url: server_url.to_string(),
        bearer,
        vault_id: vault_id.to_string(),
        local_path: local_path.to_string(),
        device_id: load_or_create_device_id(server_url).ok(),
        ignore: Vec::new(),
    })
}

// --- Accounts -----------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct AuthCapabilities {
    email: bool,
    apple: bool,
    /// New accounts need an invite code once the server has any user.
    invite_required: bool,
}

/// The wire format the server speaks against this build's (spec §15.5).
#[derive(Debug, Clone, Serialize)]
struct ProtocolInfo {
    /// `None` when the server did not answer.
    server: Option<u32>,
    client: u32,
}

/// What the UI shows for the account: signed out, signed in without the
/// account key at hand (`has_key` says whether there is a passphrase to
/// enter or one to set), or unlocked.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AccountState {
    SignedOut,
    Locked {
        user_id: String,
        email: Option<String>,
        has_key: bool,
    },
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
    id: String,
    name: String,
    platform: String,
    created: u64,
    last_seen: u64,
    current: bool,
    vault_ids: Vec<String>,
}

/// What `set_passphrase` did: this Mac set it, or another device had already
/// set it and the passphrase given unlocked that key instead.
#[derive(Debug, Clone, Serialize)]
struct SetPassphraseResponse {
    outcome: &'static str,
    account: AccountState,
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

/// The protocol gate, checked once per launch by each window.
#[tauri::command]
async fn get_protocol() -> ProtocolInfo {
    let server = AuthClient::new(&default_server_url())
        .capabilities()
        .await
        .ok()
        .map(|caps| caps.protocol);
    ProtocolInfo {
        server,
        client: PROTOCOL_VERSION,
    }
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

/// Spec §12.1: sign in as this device; the result is `locked` until the
/// passphrase step is done (or `account` when this Mac already holds the key).
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
    let device = this_device(&server_url)?;
    let session = AuthClient::new(&server_url)
        .email_verify(email.trim(), code.trim(), &device, invite)
        .await?;
    save_secret(&bearer_account(&server_url), &session.token)?;
    save_secret(&user_account(&server_url), &session.user.id)?;
    get_account().await
}

#[tauri::command]
async fn get_account() -> Result<AccountState, CommandError> {
    let server_url = default_server_url();
    let Ok(bearer) = load_stored_bearer(&server_url) else {
        return Ok(AccountState::SignedOut);
    };
    let auth = AuthClient::new(&server_url);
    let me = match auth.me(&bearer).await {
        Ok(me) => me,
        Err(error) => {
            let error = forget_bearer_on_401(&server_url, Err::<(), _>(error.into())).unwrap_err();
            if error.kind == ErrorKind::Unauthorized {
                // Session revoked/expired elsewhere: signed out is the state.
                return Ok(AccountState::SignedOut);
            }
            return Err(error);
        }
    };
    let Some(user) = me.user else {
        return Ok(AccountState::SignedOut);
    };
    if load_secret_opt(&user_account(&server_url))?.as_deref() != Some(user.id.as_str()) {
        save_secret(&user_account(&server_url), &user.id)?;
    }
    // Unlocked means the keychain holds the account's key: the one the server
    // reports, not one from a lost first-set race.
    let blob = bearer_call(&server_url, auth.get_keys(&bearer)).await?;
    let unlocked = match load_account_key(&user.id) {
        Ok((_, key_id)) => blob.as_ref().is_some_and(|blob| blob.key_id == key_id),
        Err(_) => false,
    };
    if !unlocked {
        return Ok(AccountState::Locked {
            user_id: user.id,
            email: user.email,
            has_key: blob.is_some(),
        });
    }
    Ok(AccountState::Account {
        user_id: user.id,
        email: user.email,
        devices: me
            .devices
            .into_iter()
            .map(|device| DeviceInfo {
                id: device.id,
                name: device.name,
                platform: device.platform,
                created: device.created,
                last_seen: device.last_seen,
                current: device.current,
                vault_ids: device.vault_ids,
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
    })
}

#[tauri::command]
async fn set_passphrase(
    passphrase: String,
    app: AppHandle,
) -> Result<SetPassphraseResponse, CommandError> {
    let result = set_passphrase_inner(passphrase).await;
    reconcile_daemons(&app);
    emit_state_changed(&app, None);
    result
}

/// Spec §12.1: set the passphrase (create-only). On a lost race the winner's
/// key is unlocked with the same passphrase; when that fails the UI turns the
/// form into Unlock (a 409 with the DESIGN.md text).
async fn set_passphrase_inner(passphrase: String) -> Result<SetPassphraseResponse, CommandError> {
    if passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(CommandError::other(format!(
            "At least {MIN_PASSPHRASE_CHARS} characters."
        )));
    }
    let server_url = default_server_url();
    let (bearer, user_id) = signed_in(&server_url).await?;
    let auth = AuthClient::new(&server_url);
    let (key, material) = create_account_key(&passphrase, &user_id)?;
    let outcome = match bearer_call(&server_url, auth.set_keys(&bearer, &material)).await? {
        SetKeysOutcome::Created { key_id } => {
            save_account_key(&user_id, &key, &key_id)?;
            "created"
        }
        SetKeysOutcome::Exists(blob) => {
            let Ok(key) = blob.unlock(&passphrase, &user_id) else {
                return Err(CommandError {
                    kind: ErrorKind::Server,
                    message: "A passphrase was already set on another device. Enter it.".into(),
                    status: Some(409),
                });
            };
            save_account_key(&user_id, &key, &blob.key_id)?;
            "exists"
        }
    };
    Ok(SetPassphraseResponse {
        outcome,
        account: get_account().await?,
    })
}

#[tauri::command]
async fn unlock(passphrase: String, app: AppHandle) -> Result<AccountState, CommandError> {
    let result = unlock_inner(passphrase).await;
    reconcile_daemons(&app);
    emit_state_changed(&app, None);
    result
}

/// Enter the passphrase on a Mac that does not hold the account key (or
/// holds one from a lost race, which is replaced).
async fn unlock_inner(passphrase: String) -> Result<AccountState, CommandError> {
    let server_url = default_server_url();
    let (bearer, user_id) = signed_in(&server_url).await?;
    let blob = bearer_call(&server_url, AuthClient::new(&server_url).get_keys(&bearer))
        .await?
        .ok_or_else(|| CommandError::other("Set a passphrase first."))?;
    let key = blob
        .unlock(&passphrase, &user_id)
        .map_err(|_| CommandError::other("Passphrase does not match this account."))?;
    delete_account_key(&user_id);
    save_account_key(&user_id, &key, &blob.key_id)?;
    get_account().await
}

/// DESIGN.md §5 `Change passphrase`: the current passphrase must open the
/// stored blob; the account key itself is unchanged, rewrapped under the
/// new KEK (spec §6.1).
#[tauri::command]
async fn change_passphrase(current: String, next: String) -> Result<(), CommandError> {
    if next.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(CommandError::other(format!(
            "At least {MIN_PASSPHRASE_CHARS} characters."
        )));
    }
    let server_url = default_server_url();
    let (bearer, user_id, key) = account_key_for(&server_url).await?;
    let auth = AuthClient::new(&server_url);
    let blob = bearer_call(&server_url, auth.get_keys(&bearer))
        .await?
        .ok_or_else(|| CommandError::other("Set a passphrase first."))?;
    let opened = blob
        .unlock(&current, &user_id)
        .map_err(|_| CommandError::other("Passphrase does not match this account."))?;
    if opened != key {
        return Err(CommandError::other(
            "Passphrase does not match this account.",
        ));
    }
    let material = rewrap_account_key(&key, &next, &user_id)?;
    bearer_call(&server_url, auth.rewrap_keys(&bearer, &material)).await?;
    Ok(())
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

/// Sign another device of the account out for good (its folders and keys
/// stay). Returns the refreshed account so the UI gets the new device list
/// in one round trip.
#[tauri::command]
async fn revoke_device(device_id: String) -> Result<AccountState, CommandError> {
    let server_url = default_server_url();
    let bearer = load_bearer(&server_url)?;
    bearer_call(
        &server_url,
        AuthClient::new(&server_url).revoke_device(&bearer, &device_id),
    )
    .await?;
    get_account().await
}

#[tauri::command]
async fn rename_device(device_id: String, name: String) -> Result<AccountState, CommandError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CommandError::other("Enter a device name."));
    }
    let server_url = default_server_url();
    let bearer = load_bearer(&server_url)?;
    bearer_call(
        &server_url,
        AuthClient::new(&server_url).rename_device(&bearer, &device_id, name),
    )
    .await?;
    get_account().await
}

/// Sign out of the server. Vault entries and keys stay; sync will ask for a
/// sign-in again.
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
/// bearer, the account key and every vault entry. Vault folders on disk
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
    let (bearer, user_id) = signed_in(&server_url).await?;
    bearer_call(
        &server_url,
        AuthClient::new(&server_url).delete_account(&bearer),
    )
    .await?;
    delete_secret(&bearer_account(&server_url));
    delete_secret(&user_account(&server_url));
    delete_account_key(&user_id);
    forget_all_vaults()?;
    Ok(())
}

// --- Vaults -------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct CreateVaultCommand {
    vault_name: String,
    local_path: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DownloadVaultCommand {
    vault_id: String,
    local_path: String,
}

/// What one vault row shows (spec §15.1). Computed on demand; nothing here
/// is cached.
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
    /// The account owns it; this Mac does not hold it. `Download` on the row.
    NotOnDevice,
    /// This Mac holds a folder for a vault the server no longer lists.
    DeletedOnServer,
    /// No vault key in the keychain: a pre-v3 entry, or a download that did
    /// not finish. Download again once unlocked.
    Locked,
}

/// A device that holds a vault, as the server reports it.
#[derive(Debug, Clone, Serialize)]
struct VaultDeviceInfo {
    id: String,
    name: String,
    platform: String,
    last_synced: Option<u64>,
    last_revision: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct VaultStateInfo {
    id: String,
    name: String,
    /// `None` for a vault this Mac does not hold.
    local_path: Option<String>,
    state: VaultState,
    last_synced: Option<u64>,
    /// The server's manifest revision and live bytes (0 when it did not answer).
    revision: u64,
    bytes: u64,
    devices: Vec<VaultDeviceInfo>,
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

/// Forget a vault on this device only: the server stops listing this Mac
/// for it, the entry, the key and the log go, the folder stays. Refused
/// while a sync on it runs.
async fn remove_vault_inner(vault_id: &str, state: &AppState) -> Result<(), CommandError> {
    let vault = stored_vault(vault_id)?;
    let _guard = InFlightGuard::acquire(state, vault_id)?;
    // Best effort: the server learns on the next sign-in or attach.
    let _ = ApiClient::new(to_vault_config(&vault))
        .detach_device()
        .await;
    forget_vault(vault_id)?;
    set_pending_plan(state, vault_id, None)
}

#[tauri::command]
async fn remove_vault(
    vault_id: String,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<(), CommandError> {
    let result = remove_vault_inner(&vault_id, &state).await;
    reconcile_daemons(&app);
    emit_state_changed(&app, Some(&vault_id));
    result
}

/// Delete a vault and all its files on the server, then forget it here. A
/// 404 (already gone) is reported as-is; "Remove from this device" is the
/// way out in that case.
async fn delete_remote_vault_inner(vault_id: &str, state: &AppState) -> Result<(), CommandError> {
    let server_url = default_server_url();
    let bearer = load_bearer(&server_url)?;
    let held = stored_vault(vault_id).is_ok();
    let _guard = InFlightGuard::acquire(state, vault_id)?;
    bearer_call(
        &server_url,
        api_client(&server_url, bearer, vault_id, "").delete_vault(),
    )
    .await?;
    if held {
        forget_vault(vault_id)?;
    }
    set_pending_plan(state, vault_id, None)
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

/// A new vault lands on this Mac: its daemon starts and runs a first cycle
/// right away (the folder may already hold notes).
fn start_new_vault(app: &AppHandle, result: &Result<LocalVault, CommandError>) {
    reconcile_daemons(app);
    if let Ok(vault) = result {
        if let Some(handle) = daemon_handle(&app.state::<AppState>(), &vault.id) {
            handle.sync_now();
        }
    }
    emit_state_changed(app, result.as_ref().ok().map(|vault| vault.id.as_str()));
}

#[tauri::command]
async fn create_vault(
    request: CreateVaultCommand,
    app: AppHandle,
) -> Result<LocalVault, CommandError> {
    let result = create_vault_inner(request).await;
    start_new_vault(&app, &result);
    result
}

fn vault_folder(local_path: &str) -> Result<String, CommandError> {
    let path = local_path.trim();
    if path.is_empty() {
        return Err(CommandError::other("Choose a folder for the vault."));
    }
    fs::create_dir_all(path)?;
    Ok(path.to_string())
}

/// Spec §12.2: a fresh vault key wrapped under the account key, the vault
/// on the server under a client-minted id, this Mac attached.
async fn create_vault_inner(request: CreateVaultCommand) -> Result<LocalVault, CommandError> {
    let name = request.vault_name.trim().to_string();
    if name.is_empty() {
        return Err(CommandError::other("Enter a vault name."));
    }
    let local_path = vault_folder(&request.local_path)?;
    let server_url = default_server_url();
    let (bearer, _, account_key) = account_key_for(&server_url).await?;
    let vault_key = new_key();
    // The wrap's AAD is the vault id, so the id is minted here (spec §4.3).
    let vault_id = new_vault_id();
    let client = api_client(&server_url, bearer, &vault_id, &local_path);
    let response = bearer_call(
        &server_url,
        client.create_vault(&CreateVaultRequest {
            id: Some(vault_id.clone()),
            name,
            max_file_size: MAX_FILE_SIZE,
            wrapped_key: Some(encode_base64(&wrap_vault_key(
                &account_key,
                &vault_key,
                &vault_id,
            )?)),
        }),
    )
    .await?;
    let stored = StoredVault {
        id: response.vault.id,
        name: response.vault.name,
        local_path,
        ignore: Vec::new(),
    };
    save_key_to_keychain(&stored.id, &vault_key)?;
    upsert_vault(stored.clone())?;
    bearer_call(&server_url, client.attach_device(None)).await?;
    Ok(LocalVault::from(&stored))
}

#[tauri::command]
async fn download_vault(
    request: DownloadVaultCommand,
    app: AppHandle,
) -> Result<LocalVault, CommandError> {
    let result = download_vault_inner(request).await;
    start_new_vault(&app, &result);
    result
}

/// Spec §12.3: an existing vault of the account into a folder on this Mac.
async fn download_vault_inner(request: DownloadVaultCommand) -> Result<LocalVault, CommandError> {
    let local_path = vault_folder(&request.local_path)?;
    let server_url = default_server_url();
    let (bearer, _, account_key) = account_key_for(&server_url).await?;
    let client = api_client(&server_url, bearer, &request.vault_id, &local_path);
    let vault = bearer_call(&server_url, client.list_vaults())
        .await?
        .into_iter()
        .find(|vault| vault.id == request.vault_id)
        .ok_or_else(|| CommandError::other("This vault is not one of the account's."))?;
    let wrapped = vault.wrapped_key.ok_or_else(|| {
        CommandError::other(
            "This vault has no key for the account (it was created before the passphrase).",
        )
    })?;
    let vault_key =
        unwrap_vault_key(&account_key, &decode_base64(&wrapped)?, &vault.id).map_err(|_| {
            CommandError::other("The vault key does not open with this account key. Sign in again.")
        })?;
    let stored = StoredVault {
        id: vault.id,
        name: vault.name,
        local_path,
        ignore: Vec::new(),
    };
    save_key_to_keychain(&stored.id, &vault_key)?;
    upsert_vault(stored.clone())?;
    bearer_call(&server_url, client.attach_device(None)).await?;
    Ok(LocalVault::from(&stored))
}

/// Spec §4.3 `PATCH /vaults/:id`; the stored name follows.
#[tauri::command]
async fn rename_vault(vault_id: String, name: String, app: AppHandle) -> Result<(), CommandError> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(CommandError::other("Enter a vault name."));
    }
    let server_url = default_server_url();
    let bearer = load_bearer(&server_url)?;
    bearer_call(
        &server_url,
        api_client(&server_url, bearer, &vault_id, "").rename_vault(&name),
    )
    .await?;
    if let Ok(mut vault) = stored_vault(&vault_id) {
        vault.name = name;
        upsert_vault(vault)?;
    }
    emit_state_changed(&app, Some(&vault_id));
    Ok(())
}

#[tauri::command]
async fn move_vault_folder(
    vault_id: String,
    local_path: String,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<(), CommandError> {
    // The daemon watches the old path; it comes back on the new one.
    stop_daemon(&state, &vault_id);
    let result = move_vault_folder_inner(&vault_id, &local_path, &state);
    reconcile_daemons(&app);
    emit_state_changed(&app, Some(&vault_id));
    result
}

/// DESIGN.md §5 `Move folder`: move the folder (with its `.obsink/`
/// bookkeeping) to a path that does not exist yet, or re-point the vault at
/// a folder the user already moved (it holds this vault's manifest).
fn move_vault_folder_inner(
    vault_id: &str,
    local_path: &str,
    state: &AppState,
) -> Result<(), CommandError> {
    let mut vault = stored_vault(vault_id)?;
    let target = local_path.trim();
    if target.is_empty() {
        return Err(CommandError::other("Enter the new folder path."));
    }
    let from = PathBuf::from(&vault.local_path);
    let to = PathBuf::from(target);
    if to == from {
        return Ok(());
    }
    let _guard = InFlightGuard::acquire(state, vault_id)?;
    if to.exists() {
        if !sync_manifest_path(&to).exists() {
            return Err(CommandError::other(
                "That folder exists and is not this vault. Enter a new path, or the folder you moved the vault to.",
            ));
        }
    } else {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&from, &to).map_err(|error| {
            CommandError::other(format!(
                "Could not move the folder ({error}). Move it yourself, then enter the new path."
            ))
        })?;
    }
    vault.local_path = to.to_string_lossy().into_owned();
    upsert_vault(vault)?;
    Ok(())
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
        &default_server_url(),
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

/// The state of one vault this Mac holds, never an error: a vault that
/// cannot be checked reports why.
async fn vault_state(vault: &StoredVault, state: &AppState) -> VaultState {
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
        return VaultState::Locked;
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

fn vault_devices(summary: &VaultSummary) -> Vec<VaultDeviceInfo> {
    summary
        .devices
        .iter()
        .map(|device| VaultDeviceInfo {
            id: device.id.clone(),
            name: device.name.clone(),
            platform: device.platform.clone(),
            last_synced: device.last_synced,
            last_revision: device.last_revision,
        })
        .collect()
}

/// Spec §15.1: every vault of the account with its state on this Mac. The
/// vaults here come first, in config order; a server that does not answer
/// (signed out, offline) leaves the stored vaults with the state their own
/// check reports.
async fn list_vaults_inner(state: &AppState) -> Result<Vec<VaultStateInfo>, CommandError> {
    let server_url = default_server_url();
    let config = load_app_config()?;
    let summaries: Option<Vec<VaultSummary>> = match load_stored_bearer(&server_url) {
        Ok(bearer) => bearer_call(
            &server_url,
            api_client(&server_url, bearer, "", "").list_vaults(),
        )
        .await
        .ok(),
        Err(_) => None,
    };
    let mut states = Vec::with_capacity(config.vaults.len());
    for vault in &config.vaults {
        let summary = summaries
            .as_ref()
            .and_then(|list| list.iter().find(|entry| entry.id == vault.id));
        let vault_state = match (&summaries, summary) {
            (Some(_), None) => VaultState::DeletedOnServer,
            _ => vault_state(vault, state).await,
        };
        states.push(VaultStateInfo {
            id: vault.id.clone(),
            name: summary.map_or(vault.name.clone(), |entry| entry.name.clone()),
            local_path: Some(vault.local_path.clone()),
            state: vault_state,
            last_synced: activity::last_synced(&vault.id),
            revision: summary.map_or(0, |entry| entry.revision),
            bytes: summary.map_or(0, |entry| entry.bytes),
            devices: summary.map(vault_devices).unwrap_or_default(),
        });
    }
    for summary in summaries.iter().flatten() {
        if config.vaults.iter().any(|vault| vault.id == summary.id) {
            continue;
        }
        states.push(VaultStateInfo {
            id: summary.id.clone(),
            name: summary.name.clone(),
            local_path: None,
            state: VaultState::NotOnDevice,
            last_synced: None,
            revision: summary.revision,
            bytes: summary.bytes,
            devices: vault_devices(summary),
        });
    }
    Ok(states)
}

#[tauri::command]
async fn list_vaults(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<VaultStateInfo>, CommandError> {
    list_vaults_inner(&state).await
}

/// Reveal the vault folder in Finder.
#[tauri::command]
fn open_vault_folder(vault_id: String) -> Result<(), CommandError> {
    let vault = stored_vault(&vault_id)?;
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
    vault_id: &str,
    state: &AppState,
    progress: &dyn ProgressSink,
) -> Result<SyncCommandResponse, CommandError> {
    let vault = stored_vault(vault_id)?;
    // A vault with a daemon syncs through it: the daemon serialises cycles
    // and its event relay keeps the state and the activity log.
    if let Some(handle) = daemon_handle(state, &vault.id) {
        let result = bearer_call(&default_server_url(), handle.sync_and_wait()).await?;
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
    let server_url = default_server_url();
    // A fresh cycle supersedes any plan left over from an earlier one.
    set_pending_plan(state, &vault.id, None)?;
    let plan = bearer_call(
        &server_url,
        prepare_sync(&to_vault_config(vault), &key, progress),
    )
    .await?;

    if plan.conflicts.is_empty() {
        let result = bearer_call(
            &server_url,
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
    vault_id: String,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<SyncCommandResponse, CommandError> {
    let sink = TauriProgressSink {
        app: app.clone(),
        vault_id: vault_id.clone(),
    };
    let result = sync_vault_inner(&vault_id, &state, &sink).await;
    emit_state_changed(&app, Some(&vault_id));
    result
}

async fn resolve_conflict_inner(
    vault_id: String,
    resolutions: Vec<ConflictResolution>,
    state: &AppState,
    progress: &dyn ProgressSink,
) -> Result<SyncCommandResponse, CommandError> {
    let vault = stored_vault(&vault_id)?;
    if let Some(handle) = daemon_handle(state, &vault.id) {
        let result = bearer_call(&default_server_url(), handle.resolve(resolutions)).await?;
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
        &default_server_url(),
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
    let vault = stored_vault(&vault_id)?;
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
        let blob = bearer_call(
            &default_server_url(),
            client.get_file(&conflict.path, &keys),
        )
        .await?;
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

// --- History (spec §8.2, §9.3) --------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct VersionInfoOut {
    /// What the blob route takes (`<unix>[-n]`).
    name: String,
    ts: u64,
    /// The sealed size on the server, a few bytes over the file.
    size: u64,
}

#[derive(Debug, Clone, Serialize)]
struct TrashEntryOut {
    path: String,
    hash: String,
    size: u64,
    deleted_at: u64,
}

/// A decrypted file for the read-only preview; `text` is `None` when the
/// bytes are not UTF-8 text (restore is still offered).
#[derive(Debug, Clone, Serialize)]
struct FilePreview {
    text: Option<String>,
    size: u64,
}

fn preview_of(bytes: Vec<u8>) -> FilePreview {
    let size = bytes.len() as u64;
    let text = String::from_utf8(bytes)
        .ok()
        .filter(|text| !text.contains('\0'));
    FilePreview { text, size }
}

/// A vault-relative path from the server (or the picker) that stays inside
/// the folder.
fn safe_relative(path: &str) -> Result<PathBuf, CommandError> {
    let relative = Path::new(path);
    let plain = !path.is_empty()
        && relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if !plain {
        return Err(CommandError::other(format!("Refusing the path {path:?}.")));
    }
    Ok(relative.to_path_buf())
}

struct HeldVault {
    vault: StoredVault,
    keys: obsink_core::CryptoKeys,
    client: ApiClient,
}

fn held_vault(vault_id: &str) -> Result<HeldVault, CommandError> {
    let vault = stored_vault(vault_id)?;
    let keys = derive_keys(&load_key_from_keychain(vault_id)?);
    let client = ApiClient::new(to_vault_config(&vault));
    Ok(HeldVault {
        vault,
        keys,
        client,
    })
}

/// Every file the vault holds on this Mac (the working manifest, ignore
/// rules applied), for the history file picker.
#[tauri::command]
async fn list_files(vault_id: String) -> Result<Vec<String>, CommandError> {
    let held = held_vault(&vault_id)?;
    let root = PathBuf::from(&held.vault.local_path);
    let ignore = to_vault_config(&held.vault).ignore_rules();
    let keys = held.keys.clone();
    let local =
        tauri::async_runtime::spawn_blocking(move || load_local_state(&root, &keys, &ignore))
            .await
            .map_err(|error| CommandError::other(error.to_string()))??;
    let mut files: Vec<String> = local
        .working
        .iter()
        .filter(|(_, entry)| !entry.deleted)
        .map(|(path, _)| path.clone())
        .collect();
    files.sort();
    Ok(files)
}

#[tauri::command]
async fn list_versions(
    vault_id: String,
    path: String,
) -> Result<Vec<VersionInfoOut>, CommandError> {
    safe_relative(&path)?;
    let held = held_vault(&vault_id)?;
    let versions = bearer_call(
        &default_server_url(),
        held.client.list_versions(&path, &held.keys),
    )
    .await?;
    Ok(versions
        .into_iter()
        .map(|version| VersionInfoOut {
            name: version.name,
            ts: version.ts,
            size: version.size,
        })
        .collect())
}

async fn version_bytes(
    vault_id: &str,
    path: &str,
    name: &str,
) -> Result<(HeldVault, Vec<u8>), CommandError> {
    let held = held_vault(vault_id)?;
    let bytes = bearer_call(
        &default_server_url(),
        held.client.get_version(path, name, &held.keys),
    )
    .await?;
    Ok((held, bytes))
}

#[tauri::command]
async fn preview_version(
    vault_id: String,
    path: String,
    name: String,
) -> Result<FilePreview, CommandError> {
    safe_relative(&path)?;
    let (_, bytes) = version_bytes(&vault_id, &path, &name).await?;
    Ok(preview_of(bytes))
}

/// Restore writes the version into the folder; the next sync uploads it
/// through the ordinary conflict-gated path.
#[tauri::command]
async fn restore_version(
    vault_id: String,
    path: String,
    name: String,
    app: AppHandle,
) -> Result<(), CommandError> {
    let relative = safe_relative(&path)?;
    let (held, bytes) = version_bytes(&vault_id, &path, &name).await?;
    write_restored(&held.vault, &relative, &bytes)?;
    emit_state_changed(&app, Some(&vault_id));
    Ok(())
}

#[tauri::command]
async fn list_trash(vault_id: String) -> Result<Vec<TrashEntryOut>, CommandError> {
    let held = held_vault(&vault_id)?;
    let entries = bearer_call(&default_server_url(), held.client.list_trash(&held.keys)).await?;
    Ok(entries
        .into_iter()
        .map(|entry| TrashEntryOut {
            path: entry.path,
            hash: entry.hash,
            size: entry.size,
            deleted_at: entry.deleted_at,
        })
        .collect())
}

async fn trash_bytes(vault_id: &str, path: &str) -> Result<(HeldVault, Vec<u8>), CommandError> {
    let held = held_vault(vault_id)?;
    let bytes = bearer_call(
        &default_server_url(),
        held.client.get_trash(path, &held.keys),
    )
    .await?;
    Ok((held, bytes))
}

#[tauri::command]
async fn preview_trash(vault_id: String, path: String) -> Result<FilePreview, CommandError> {
    safe_relative(&path)?;
    let (_, bytes) = trash_bytes(&vault_id, &path).await?;
    Ok(preview_of(bytes))
}

/// A restored deletion syncs with the tombstone as its parent: the base
/// manifest still holds it, so the ordinary upload path presents its hash.
#[tauri::command]
async fn restore_trash(vault_id: String, path: String, app: AppHandle) -> Result<(), CommandError> {
    let relative = safe_relative(&path)?;
    let (held, bytes) = trash_bytes(&vault_id, &path).await?;
    write_restored(&held.vault, &relative, &bytes)?;
    emit_state_changed(&app, Some(&vault_id));
    Ok(())
}

fn write_restored(vault: &StoredVault, relative: &Path, bytes: &[u8]) -> Result<(), CommandError> {
    let target = Path::new(&vault.local_path).join(relative);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    write_atomic(&target, bytes)?;
    Ok(())
}

// --- Stored config and keychain -------------------------------------------------

fn stored_vault(vault_id: &str) -> Result<StoredVault, CommandError> {
    load_app_config()?
        .vaults
        .into_iter()
        .find(|vault| vault.id == vault_id)
        .ok_or_else(|| CommandError::other("This vault is not on this Mac. Download it first."))
}

/// The bearer comes from the keychain; if it is missing the request goes
/// out without one and the server's 401 surfaces as `ApiError::Unauthorized`
/// ("sign in again"), which is the message the user needs.
fn to_vault_config(vault: &StoredVault) -> VaultConfig {
    let server_url = default_server_url();
    VaultConfig {
        bearer: load_stored_bearer(&server_url).unwrap_or_default(),
        device_id: load_or_create_device_id(&server_url).ok(),
        server_url,
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
    serde_json::from_str(&contents)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
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
        *existing = vault;
    } else {
        config.vaults.push(vault);
    }
    save_app_config(&config)
}

/// Drop a vault from this device: config entry, keychain key, activity log.
/// The local folder (including `.obsink/`) is left alone; downloading it
/// again later resumes from that checkpoint.
fn forget_vault(vault_id: &str) -> Result<(), io::Error> {
    let mut config = load_app_config()?;
    config.vaults.retain(|vault| vault.id != vault_id);
    save_app_config(&config)?;
    delete_secret(vault_id);
    activity::forget(vault_id);
    Ok(())
}

/// The newest activity across every vault on this Mac, or one of them.
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

/// `forget_vault` for every vault (account deletion).
fn forget_all_vaults() -> Result<(), io::Error> {
    let ids: Vec<String> = load_app_config()?
        .vaults
        .iter()
        .map(|vault| vault.id.clone())
        .collect();
    for id in ids {
        forget_vault(&id)?;
    }
    Ok(())
}

/// The vault key under the vault id, as the CLI stores it.
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
/// status can be matched (403 invite gating, 409 passphrase race) without
/// parsing the text.
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
            // The windows show `Update ObSink` from `get_protocol`; a call
            // that still gets here reports the same sentence.
            error @ AuthError::ProtocolMismatch { .. } => Self::other(error.to_string()),
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

// --- Windows and tray -------------------------------------------------------------

/// Gap between the menu-bar icon and the popover, in physical pixels.
const POPOVER_GAP: i32 = 6;

/// Which tab (and vault, and flow) the settings window should show; sent by
/// the popover through `open_settings` and by the tray menu.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SettingsTarget {
    tab: String,
    #[serde(default)]
    vault_id: Option<String>,
    /// Open the Create vault flow.
    #[serde(default)]
    add_vault: bool,
    /// Open the Download flow for `vault_id` (a popover row's `Download`).
    #[serde(default)]
    download: bool,
}

impl SettingsTarget {
    fn vaults() -> Self {
        Self {
            tab: "vaults".to_string(),
            vault_id: None,
            add_vault: false,
            download: false,
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
                // version is in the settings window's Settings tab.
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
            auth_email_start,
            auth_email_verify,
            change_passphrase,
            create_invite,
            create_vault,
            delete_account,
            delete_remote_vault,
            download_vault,
            get_account,
            get_auth_capabilities,
            get_conflict_preview,
            get_protocol,
            get_server_url,
            list_activity,
            list_files,
            list_invites,
            list_trash,
            list_vaults,
            list_versions,
            move_vault_folder,
            open_settings,
            open_vault_folder,
            preview_trash,
            preview_version,
            remove_vault,
            rename_device,
            rename_vault,
            resolve_conflict,
            restore_trash,
            restore_version,
            revoke_device,
            set_passphrase,
            sign_out,
            sync_vault,
            unlock,
        ])
        .setup(|app| {
            // Menu-bar app: no Dock icon, no app switcher entry.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            setup_tray(app.handle())?;
            reconcile_daemons(app.handle());
            #[cfg(debug_assertions)]
            automation::start_if_requested(app.handle().clone());
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
mod config_tests {
    use super::*;
    use std::path::PathBuf;

    fn sandbox(name: &str) -> PathBuf {
        let dir = PathBuf::from(format!("/tmp/obsink-desktop-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("keyring")).unwrap();
        std::env::set_var("HOME", &dir);
        std::env::set_var("OBSINK_KEYRING_DIR", dir.join("keyring"));
        dir
    }

    /// A pre-v3 `app.json` (per-vault `server_url`, `active_vault_id`) reads
    /// as its vaults and loses both fields on the next write.
    #[test]
    fn config_migration_drops_v2_fields() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let dir = sandbox("config-v2");
        fs::create_dir_all(dir.join(".obsink")).unwrap();
        fs::write(
            dir.join(APP_CONFIG_FILE),
            r#"{"vaults":[{"id":"vault_a","name":"A","server_url":"https://old.example","local_path":"/tmp/a","ignore":["drafts/"]}],"active_vault_id":"vault_a"}"#,
        )
        .unwrap();
        let config = load_app_config().unwrap();
        assert_eq!(config.vaults.len(), 1);
        assert_eq!(config.vaults[0].id, "vault_a");
        assert_eq!(config.vaults[0].ignore, vec!["drafts/".to_string()]);
        save_app_config(&config).unwrap();
        let written = fs::read_to_string(dir.join(APP_CONFIG_FILE)).unwrap();
        assert!(!written.contains("server_url"), "{written}");
        assert!(!written.contains("active_vault_id"), "{written}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// `forget_vault` keeps the others and drops the key; `forget_all_vaults`
    /// empties the list.
    #[test]
    fn forget_vault_updates_config_and_keyring() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let dir = sandbox("forget");
        for id in ["vault_a", "vault_b", "vault_c"] {
            upsert_vault(StoredVault {
                id: id.to_string(),
                name: id.to_string(),
                local_path: dir.join(id).to_string_lossy().into_owned(),
                ignore: Vec::new(),
            })
            .unwrap();
            save_key_to_keychain(id, &[7_u8; 32]).unwrap();
        }
        forget_vault("vault_a").unwrap();
        let config = load_app_config().unwrap();
        assert_eq!(config.vaults.len(), 2);
        assert!(load_key_from_keychain("vault_a").is_err());
        assert!(load_key_from_keychain("vault_b").is_ok());
        forget_all_vaults().unwrap();
        assert!(load_app_config().unwrap().vaults.is_empty());
        assert!(load_key_from_keychain("vault_c").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn safe_relative_rejects_escapes() {
        assert!(safe_relative("notes/a.md").is_ok());
        assert!(safe_relative("").is_err());
        assert!(safe_relative("/etc/passwd").is_err());
        assert!(safe_relative("../x.md").is_err());
        assert!(safe_relative("notes/../../x.md").is_err());
    }

    #[test]
    fn preview_tells_text_from_binary() {
        assert_eq!(preview_of(b"# hi".to_vec()).text.as_deref(), Some("# hi"));
        assert_eq!(preview_of(vec![0xff, 0xfe, 0x00]).text, None);
        assert_eq!(preview_of(b"a\0b".to_vec()).text, None);
        assert_eq!(preview_of(Vec::new()).size, 0);
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use obsink_core::{derive_keys, ApiClient, ConflictResolutionChoice, VaultConfig};
    use std::{fs, path::PathBuf};

    /// One "device" of the live tests: its own HOME (`app.json`, the activity
    /// log) and keyring (bearer, user id, account key, device id, vault keys).
    struct DeviceEnv {
        home: PathBuf,
        keyring: PathBuf,
    }

    impl DeviceEnv {
        fn new(sandbox: &Path, name: &str) -> Self {
            let home = sandbox.join(name);
            let keyring = home.join("keyring");
            fs::create_dir_all(&keyring).unwrap();
            Self { home, keyring }
        }

        /// Make the process act as this device.
        fn activate(&self) {
            std::env::set_var("HOME", &self.home);
            std::env::set_var("OBSINK_KEYRING_DIR", &self.keyring);
        }
    }

    fn passphrase() -> String {
        std::env::var("OBSINK_TEST_PASSPHRASE")
            .unwrap_or_else(|_| "obsink-test-passphrase-2026".to_string())
    }

    /// The first account on an established server needs an invite minted by
    /// an existing account (`obsink invite`); a fresh database needs none.
    fn bootstrap_invite() -> Option<String> {
        std::env::var("OBSINK_TEST_INVITE_CODE")
            .ok()
            .filter(|code| !code.trim().is_empty())
    }

    /// Sign this device in, then set the passphrase (a new account) or unlock.
    async fn sign_in_and_unlock(email: &str, invite: Option<String>) -> AccountState {
        let code = start_code_after_cooldown(email).await;
        let state = auth_email_verify_inner(email.to_string(), code, invite)
            .await
            .unwrap();
        match state {
            AccountState::Locked { has_key: false, .. } => {
                set_passphrase_inner(passphrase()).await.unwrap().account
            }
            AccountState::Locked { has_key: true, .. } => unlock_inner(passphrase()).await.unwrap(),
            other => other,
        }
    }

    fn devices_of(state: &AccountState) -> Vec<DeviceInfo> {
        match state {
            AccountState::Account { devices, .. } => devices.clone(),
            other => panic!("expected an unlocked account, got {other:?}"),
        }
    }

    async fn sync(vault_id: &str, state: &AppState) -> SyncResult {
        sync_vault_inner(vault_id, state, &obsink_core::NoProgress)
            .await
            .unwrap()
            .completed_result
            .expect("no conflicts")
    }

    /// `#[ignore]`d live test: the vault flows of spec §12 and §15 through the
    /// desktop command functions, end to end, against a running server with
    /// `AUTH_DEV_RETURN_CODE=1` (the compose stack). Device A creates, B
    /// downloads, both sync, rename, move, history and trash restores, the
    /// three conflict choices, and the device rows. Run with:
    ///   OBSINK_TEST_SERVER_URL=http://localhost:18080 [OBSINK_TEST_INVITE_CODE=...] \
    ///   cargo test -p obsink-desktop live_tests -- --ignored --nocapture --test-threads=1
    #[ignore]
    #[tokio::test]
    async fn desktop_flows_live() {
        let server_url = normalize_server_url(&env_or_panic("OBSINK_TEST_SERVER_URL"));
        std::env::set_var("OBSINK_SERVER_URL", &server_url);
        let sandbox = PathBuf::from(format!("/tmp/obsink-desktop-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&sandbox);
        let device_a = DeviceEnv::new(&sandbox, "a");
        let device_b = DeviceEnv::new(&sandbox, "b");
        let dir_a = device_a.home.join("vault");
        let dir_b = device_b.home.join("vault");
        fs::create_dir_all(dir_a.join("notes")).unwrap();
        let state = AppState::default();
        let file_rel = "notes/a.md";
        let email = format!("desktop-{}@example.com", std::process::id());

        // ===== A: sign in, set the passphrase, create =====
        device_a.activate();
        assert!(matches!(
            get_account().await.unwrap(),
            AccountState::SignedOut
        ));
        let denied = create_vault_inner(CreateVaultCommand {
            vault_name: "denied".to_string(),
            local_path: dir_a.to_string_lossy().into_owned(),
        })
        .await
        .unwrap_err();
        assert!(denied.message.contains("sign in"), "{denied}");
        let account = sign_in_and_unlock(&email, bootstrap_invite()).await;
        let devices = devices_of(&account);
        assert_eq!(devices.len(), 1, "{devices:?}");
        assert!(devices[0].current);
        assert_eq!(devices[0].platform, "macos");
        let device_a_id = devices[0].id.clone();
        assert_eq!(
            load_or_create_device_id(&server_url).unwrap(),
            device_a_id,
            "the device id is the keychain's"
        );

        fs::write(dir_a.join(file_rel), "content-A").unwrap();
        let created = create_vault_inner(CreateVaultCommand {
            vault_name: "obsink-desktop-verify".to_string(),
            local_path: dir_a.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
        let vault_id = created.id.clone();
        println!("A: created vault {vault_id}");
        load_key_from_keychain(&vault_id).expect("vault key in the keychain after create");
        let app_json = fs::read_to_string(device_a.home.join(APP_CONFIG_FILE)).unwrap();
        assert!(
            !app_json.contains("server_url") && !app_json.contains("os_"),
            "{app_json}"
        );
        let result = sync(&vault_id, &state).await;
        assert_eq!(result.upload.len(), 1, "a.md uploads");

        let listed = list_vaults_inner(&state).await.unwrap();
        let mine = listed.iter().find(|entry| entry.id == vault_id).unwrap();
        assert!(
            matches!(mine.state, VaultState::UpToDate),
            "{:?}",
            mine.state
        );
        assert!(mine.local_path.is_some());
        assert!(mine.revision >= 1, "revision from the server: {mine:?}");
        assert!(
            mine.devices.iter().any(|device| device.id == device_a_id),
            "A is attached: {:?}",
            mine.devices
        );

        // ===== B: sign in, unlock, download =====
        device_b.activate();
        let account = sign_in_and_unlock(&email, None).await;
        let devices = devices_of(&account);
        assert_eq!(devices.len(), 2, "{devices:?}");
        let device_b_id = devices
            .iter()
            .find(|device| device.current)
            .unwrap()
            .id
            .clone();
        assert_ne!(device_a_id, device_b_id);
        let listed = list_vaults_inner(&state).await.unwrap();
        let elsewhere = listed.iter().find(|entry| entry.id == vault_id).unwrap();
        assert!(matches!(elsewhere.state, VaultState::NotOnDevice));
        assert!(elsewhere.local_path.is_none());
        let downloaded = download_vault_inner(DownloadVaultCommand {
            vault_id: vault_id.clone(),
            local_path: dir_b.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
        assert_eq!(downloaded.name, "obsink-desktop-verify");
        let result = sync(&vault_id, &state).await;
        assert_eq!(result.download.len(), 1, "a.md downloads on B");
        assert_eq!(
            fs::read_to_string(dir_b.join(file_rel)).unwrap(),
            "content-A"
        );
        println!("B: downloaded and synced");

        // ===== B: a new file; A sees it pending, the device rows say so =====
        fs::write(dir_b.join("notes/b.md"), "B-only").unwrap();
        sync(&vault_id, &state).await;
        device_a.activate();
        let listed = list_vaults_inner(&state).await.unwrap();
        let mine = listed.iter().find(|entry| entry.id == vault_id).unwrap();
        assert!(
            matches!(mine.state, VaultState::Pending { downloads, .. } if downloads >= 1),
            "remote change pending on A: {:?}",
            mine.state
        );
        let row_b = mine
            .devices
            .iter()
            .find(|device| device.id == device_b_id)
            .expect("B attached");
        assert_eq!(
            row_b.last_revision,
            Some(mine.revision),
            "B reported its checkpoint"
        );
        assert!(row_b.last_synced.is_some());
        sync(&vault_id, &state).await;
        assert_eq!(
            fs::read_to_string(dir_a.join("notes/b.md")).unwrap(),
            "B-only"
        );
        assert!(mine.last_synced.is_some());
        let activity = list_activity(Some(vault_id.clone()), None).unwrap();
        assert!(activity
            .iter()
            .any(|event| event.kind == activity::ActivityKind::Synced));

        // ===== Rename, move folder =====
        let bearer = load_bearer(&server_url).unwrap();
        bearer_call(
            &server_url,
            api_client(&server_url, bearer, &vault_id, "").rename_vault("renamed-vault"),
        )
        .await
        .unwrap();
        let listed = list_vaults_inner(&state).await.unwrap();
        assert_eq!(
            listed
                .iter()
                .find(|entry| entry.id == vault_id)
                .unwrap()
                .name,
            "renamed-vault"
        );
        let moved_dir = device_a.home.join("moved").join("vault");
        move_vault_folder_inner(&vault_id, &moved_dir.to_string_lossy(), &state).unwrap();
        assert!(!dir_a.exists());
        assert!(moved_dir.join(file_rel).exists());
        assert!(sync_manifest_path(&moved_dir).exists());
        assert_eq!(
            stored_vault(&vault_id).unwrap().local_path,
            moved_dir.to_string_lossy()
        );
        let refused = move_vault_folder_inner(
            &vault_id,
            &device_a.home.join("keyring").to_string_lossy(),
            &state,
        )
        .unwrap_err();
        assert!(refused.message.contains("not this vault"), "{refused}");
        let dir_a = moved_dir;
        let result = sync(&vault_id, &state).await;
        assert!(result.upload.is_empty() && result.download.is_empty());
        println!("A: renamed and moved");

        // ===== History: a version restored, a deletion restored =====
        fs::write(dir_a.join(file_rel), "content-A-v2").unwrap();
        sync(&vault_id, &state).await;
        let files = list_files(vault_id.clone()).await.unwrap();
        assert!(files.contains(&file_rel.to_string()), "{files:?}");
        let versions = list_versions(vault_id.clone(), file_rel.to_string())
            .await
            .unwrap();
        assert!(!versions.is_empty(), "the overwrite archived a version");
        let newest = versions[0].name.clone();
        let preview = preview_version(vault_id.clone(), file_rel.to_string(), newest.clone())
            .await
            .unwrap();
        assert_eq!(preview.text.as_deref(), Some("content-A"));
        restore_bytes_version(&vault_id, file_rel, &newest).await;
        assert_eq!(
            fs::read_to_string(dir_a.join(file_rel)).unwrap(),
            "content-A"
        );
        let result = sync(&vault_id, &state).await;
        assert_eq!(result.upload.len(), 1, "the restored version uploads");
        fs::remove_file(dir_a.join("notes/b.md")).unwrap();
        sync(&vault_id, &state).await;
        let trash = list_trash(vault_id.clone()).await.unwrap();
        let gone = trash
            .iter()
            .find(|entry| entry.path == "notes/b.md")
            .expect("the deletion is in the trash");
        assert!(gone.deleted_at > 0);
        assert_eq!(
            preview_trash(vault_id.clone(), "notes/b.md".to_string())
                .await
                .unwrap()
                .text
                .as_deref(),
            Some("B-only")
        );
        restore_bytes_trash(&vault_id, "notes/b.md").await;
        assert_eq!(
            fs::read_to_string(dir_a.join("notes/b.md")).unwrap(),
            "B-only"
        );
        let result = sync(&vault_id, &state).await;
        assert_eq!(result.upload.len(), 1, "the restored deletion uploads");
        assert!(result.conflicts.is_empty() && result.failures.is_empty());
        println!("A: history and trash restores verified");

        // ===== Conflict resolution: all three choices =====
        let bearer_a = load_bearer(&server_url).unwrap();
        for choice in [
            ConflictResolutionChoice::KeepLocal,
            ConflictResolutionChoice::KeepRemote,
            ConflictResolutionChoice::KeepBoth,
        ] {
            let base_text = format!("BASE-{choice:?}");
            fs::write(dir_a.join(file_rel), &base_text).unwrap();
            sync(&vault_id, &state).await;
            let local_text = format!("LOCAL-{choice:?}");
            put_remote_text(&bearer_a, &vault_id, file_rel, "REMOTE", &base_text).await;
            fs::write(dir_a.join(file_rel), &local_text).unwrap();
            let resp = sync_vault_inner(&vault_id, &state, &obsink_core::NoProgress)
                .await
                .unwrap();
            assert_eq!(resp.pending_conflicts.len(), 1, "{choice:?}: one conflict");
            let preview =
                get_conflict_preview_inner(vault_id.clone(), file_rel.to_string(), &state)
                    .await
                    .unwrap();
            assert_eq!(preview.local_text, local_text);
            assert_eq!(preview.remote_text, "REMOTE");
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
                "{choice:?}: no late 409"
            );
            match choice {
                ConflictResolutionChoice::KeepLocal => {
                    assert_eq!(
                        remote_text(&bearer_a, &vault_id, file_rel).await,
                        local_text
                    );
                }
                ConflictResolutionChoice::KeepRemote => {
                    assert_eq!(fs::read_to_string(dir_a.join(file_rel)).unwrap(), "REMOTE");
                }
                ConflictResolutionChoice::KeepBoth => {
                    assert_eq!(
                        fs::read_to_string(dir_a.join("notes/a.conflict.md")).unwrap(),
                        "REMOTE"
                    );
                    assert_eq!(
                        remote_text(&bearer_a, &vault_id, file_rel).await,
                        local_text
                    );
                    let _ = fs::remove_file(dir_a.join("notes/a.conflict.md"));
                }
                ConflictResolutionChoice::Defer => unreachable!("the UI never defers"),
            }
            println!("conflict ({choice:?}): verified");
        }

        // ===== Devices: rename B from A, then revoke it =====
        let renamed = rename_device(device_b_id.clone(), "Other Mac".to_string())
            .await
            .unwrap();
        assert!(devices_of(&renamed)
            .iter()
            .any(|device| device.id == device_b_id && device.name == "Other Mac"));
        let revoked = revoke_device(device_b_id.clone()).await.unwrap();
        assert_eq!(devices_of(&revoked).len(), 1);
        device_b.activate();
        assert!(matches!(
            get_account().await.unwrap(),
            AccountState::SignedOut
        ));
        // B keeps its folder and its keys.
        assert!(dir_b.join(file_rel).exists());
        assert!(load_key_from_keychain(&vault_id).is_ok());
        let listed = list_vaults_inner(&state).await.unwrap();
        let held = listed.iter().find(|entry| entry.id == vault_id).unwrap();
        assert!(
            matches!(
                &held.state,
                VaultState::Error {
                    error_kind: ErrorKind::Unauthorized,
                    ..
                }
            ),
            "B's vault reads Session expired: {:?}",
            held.state
        );
        println!("devices: rename and revoke verified");

        // ===== Change passphrase, then unlock on a third device with it =====
        device_a.activate();
        let wrong = change_passphrase(
            "not the passphrase".to_string(),
            "new passphrase 2026".to_string(),
        )
        .await
        .unwrap_err();
        assert!(wrong.message.contains("does not match"), "{wrong}");
        change_passphrase(passphrase(), "new passphrase 2026".to_string())
            .await
            .unwrap();
        let device_c = DeviceEnv::new(&sandbox, "c");
        device_c.activate();
        std::env::set_var("OBSINK_TEST_PASSPHRASE", "new passphrase 2026");
        let account = sign_in_and_unlock(&email, None).await;
        assert!(matches!(account, AccountState::Account { .. }));
        std::env::remove_var("OBSINK_TEST_PASSPHRASE");
        println!("passphrase change verified");

        // ===== Delete on server: A's entry goes, the list no longer has it =====
        device_a.activate();
        delete_remote_vault_inner(&vault_id, &state).await.unwrap();
        assert!(load_app_config().unwrap().vaults.is_empty());
        assert!(load_key_from_keychain(&vault_id).is_err());
        assert!(dir_a.join(file_rel).exists());
        assert!(!list_vaults_inner(&state)
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.id == vault_id));
        println!("ALL DESKTOP FLOWS VERIFIED: vault={vault_id}");
        let _ = fs::remove_dir_all(&sandbox);
    }

    /// `#[ignore]`d live test for the account path: sign-in, the passphrase
    /// race, invites and gating, a second account's isolation, remove and
    /// delete, account deletion, sign-out.
    #[ignore]
    #[tokio::test]
    async fn account_flow_live() {
        let server_url = normalize_server_url(&env_or_panic("OBSINK_TEST_SERVER_URL"));
        std::env::set_var("OBSINK_SERVER_URL", &server_url);
        let sandbox = PathBuf::from(format!("/tmp/obsink-desktop-acct-{}", std::process::id()));
        let _ = fs::remove_dir_all(&sandbox);
        let device_a = DeviceEnv::new(&sandbox, "a");
        let device_b = DeviceEnv::new(&sandbox, "b");
        let dir = device_a.home.join("vault");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("note.md"), "# note\n").unwrap();
        let state = AppState::default();
        let email = format!("desktop-acct-{}@example.com", std::process::id());

        device_a.activate();
        let code = auth_email_start(email.clone())
            .await
            .unwrap()
            .expect("dev server returns the code inline");
        let signed_in = auth_email_verify_inner(email.clone(), code, bootstrap_invite())
            .await
            .unwrap();
        match &signed_in {
            AccountState::Locked {
                email: got,
                has_key,
                ..
            } => {
                assert_eq!(got.as_deref(), Some(email.as_str()));
                assert!(!has_key, "a new account has no passphrase yet");
            }
            other => panic!("expected locked, got {other:?}"),
        }
        // Too short, then set; a second set is the race path and unlocks.
        let short = set_passphrase_inner("short".to_string()).await.unwrap_err();
        assert!(short.message.contains("12"), "{short}");
        let set = set_passphrase_inner(passphrase()).await.unwrap();
        assert_eq!(set.outcome, "created");
        assert!(matches!(set.account, AccountState::Account { .. }));
        let again = set_passphrase_inner(passphrase()).await.unwrap();
        assert_eq!(again.outcome, "exists");
        let lost = set_passphrase_inner("another passphrase".to_string())
            .await
            .unwrap_err();
        assert_eq!(lost.status, Some(409), "{lost}");
        assert!(lost.message.contains("already set"), "{lost}");
        // The key of the lost race never replaced the account's; still unlocked.
        assert!(matches!(
            get_account().await.unwrap(),
            AccountState::Account { .. }
        ));
        let wrong = unlock_inner("wrong passphrase".to_string())
            .await
            .unwrap_err();
        assert!(wrong.message.contains("does not match"), "{wrong}");
        assert!(matches!(
            unlock_inner(passphrase()).await.unwrap(),
            AccountState::Account { .. }
        ));

        let created = create_vault_inner(CreateVaultCommand {
            vault_name: "desktop-account-vault".to_string(),
            local_path: dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
        let app_json = fs::read_to_string(device_a.home.join(APP_CONFIG_FILE)).unwrap();
        assert!(!app_json.contains("os_"), "{app_json}");
        assert_eq!(sync(&created.id, &state).await.upload.len(), 1);

        // Invite gating: a second account needs a code minted by the first.
        let invite = create_invite().await.unwrap();
        assert!(!invite.code.is_empty());
        device_b.activate();
        let second = format!("desktop-acct-b-{}@example.com", std::process::id());
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
        assert!(matches!(
            accepted,
            AccountState::Locked { has_key: false, .. }
        ));
        set_passphrase_inner("account b passphrase".to_string())
            .await
            .unwrap();
        // B's account does not see A's vault, and cannot download or delete it.
        assert!(!list_vaults_inner(&state)
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.id == created.id));
        let denied = download_vault_inner(DownloadVaultCommand {
            vault_id: created.id.clone(),
            local_path: device_b.home.join("vault").to_string_lossy().into_owned(),
        })
        .await
        .unwrap_err();
        assert!(
            denied.message.contains("not one of the account"),
            "{denied}"
        );
        let cross = delete_remote_vault_inner(&created.id, &state)
            .await
            .unwrap_err();
        assert_eq!(cross.status, Some(404), "{cross}");

        // Capabilities and the invite list from A.
        device_a.activate();
        let caps = get_auth_capabilities().await.unwrap();
        assert!(caps.email && caps.invite_required, "{caps:?}");
        let protocol = get_protocol().await;
        assert_eq!(protocol.server, Some(PROTOCOL_VERSION));
        let invites = list_invites().await.unwrap();
        let used = invites
            .iter()
            .find(|item| item.code == invite.code)
            .expect("minted invite is listed");
        assert_eq!(used.status, "used");
        assert!(used.used_at.is_some());

        // A bogus bearer on a vault command: Unauthorized, bearer forgotten.
        let token_a = load_secret(&bearer_account(&server_url)).unwrap();
        save_secret(&bearer_account(&server_url), "os_bogus").unwrap();
        let err = sync_vault_inner(&created.id, &state, &obsink_core::NoProgress)
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unauthorized, "{err}");
        assert!(load_secret(&bearer_account(&server_url)).is_err());
        assert!(matches!(
            get_account().await.unwrap(),
            AccountState::SignedOut
        ));
        save_secret(&bearer_account(&server_url), &token_a).unwrap();
        assert!(matches!(
            get_account().await.unwrap(),
            AccountState::Account { .. }
        ));

        // Remove from this device: entry and key go, the folder stays, the
        // server still lists the vault (as not on this device).
        let dir2 = device_a.home.join("vault2");
        fs::create_dir_all(&dir2).unwrap();
        let second_vault = create_vault_inner(CreateVaultCommand {
            vault_name: "desktop-second-vault".to_string(),
            local_path: dir2.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
        remove_vault_inner(&second_vault.id, &state).await.unwrap();
        assert_eq!(load_app_config().unwrap().vaults.len(), 1);
        assert!(load_key_from_keychain(&second_vault.id).is_err());
        assert!(dir2.exists());
        let listed = list_vaults_inner(&state).await.unwrap();
        let gone = listed
            .iter()
            .find(|entry| entry.id == second_vault.id)
            .unwrap();
        assert!(matches!(gone.state, VaultState::NotOnDevice));
        assert!(
            gone.devices.is_empty(),
            "detached on remove: {:?}",
            gone.devices
        );
        // Download it again: the key comes back from the account.
        download_vault_inner(DownloadVaultCommand {
            vault_id: second_vault.id.clone(),
            local_path: dir2.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
        assert!(load_key_from_keychain(&second_vault.id).is_ok());

        // Delete on server: gone from the account, folder intact.
        delete_remote_vault_inner(&created.id, &state)
            .await
            .unwrap();
        assert!(load_key_from_keychain(&created.id).is_err());
        assert!(dir.join("note.md").exists());
        assert!(!list_vaults_inner(&state)
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.id == created.id));
        let err = delete_remote_vault_inner(&created.id, &state)
            .await
            .unwrap_err();
        assert_eq!(err.status, Some(404), "{err}");

        // Delete account B: signed out, bearer and key gone, session dead.
        device_b.activate();
        let dir_b = device_b.home.join("vault-b");
        fs::create_dir_all(&dir_b).unwrap();
        create_vault_inner(CreateVaultCommand {
            vault_name: "desktop-b-vault".to_string(),
            local_path: dir_b.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
        let token_b = load_secret(&bearer_account(&server_url)).unwrap();
        let user_b = load_secret(&user_account(&server_url)).unwrap();
        delete_account_inner().await.unwrap();
        assert!(matches!(
            get_account().await.unwrap(),
            AccountState::SignedOut
        ));
        assert!(load_secret(&bearer_account(&server_url)).is_err());
        assert!(load_account_key(&user_b).is_err());
        assert!(load_app_config().unwrap().vaults.is_empty());
        let gone = AuthClient::new(&server_url).me(&token_b).await.unwrap_err();
        assert!(
            matches!(gone, AuthError::Server { status, .. } if status.as_u16() == 401),
            "{gone}"
        );

        // Sign out A: the entry and the keys stay, the account reads signed out.
        device_a.activate();
        sign_out_inner().await.unwrap();
        assert!(matches!(
            get_account().await.unwrap(),
            AccountState::SignedOut
        ));
        assert_eq!(load_app_config().unwrap().vaults.len(), 1);
        assert!(load_key_from_keychain(&second_vault.id).is_ok());

        println!("ACCOUNT FLOW VERIFIED: vault={}", created.id);
        let _ = fs::remove_dir_all(&sandbox);
    }

    async fn restore_bytes_version(vault_id: &str, path: &str, name: &str) {
        let relative = safe_relative(path).unwrap();
        let (held, bytes) = version_bytes(vault_id, path, name).await.unwrap();
        write_restored(&held.vault, &relative, &bytes).unwrap();
    }

    async fn restore_bytes_trash(vault_id: &str, path: &str) {
        let relative = safe_relative(path).unwrap();
        let (held, bytes) = trash_bytes(vault_id, path).await.unwrap();
        write_restored(&held.vault, &relative, &bytes).unwrap();
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

    fn vault_client(bearer: &str, vault_id: &str) -> ApiClient {
        ApiClient::new(VaultConfig {
            server_url: default_server_url(),
            bearer: bearer.to_string(),
            device_id: None,
            vault_id: vault_id.to_string(),
            local_path: String::new(),
            ignore: Vec::new(),
        })
    }

    /// Overwrite `path` on the server as another device would, gated on the
    /// hash of `parent_text` (the version both sides last agreed on).
    async fn put_remote_text(
        bearer: &str,
        vault_id: &str,
        path: &str,
        text: &str,
        parent_text: &str,
    ) {
        let keys = derive_keys(&load_key_from_keychain(vault_id).unwrap());
        let parent = obsink_core::content_hmac(&keys.content_mac, parent_text.as_bytes());
        let content_hash = obsink_core::content_hmac(&keys.content_mac, text.as_bytes());
        let ciphertext = obsink_core::encrypt(&keys.content_enc, text.as_bytes()).unwrap();
        vault_client(bearer, vault_id)
            .put_file(path, Some(&parent), &content_hash, ciphertext, &keys)
            .await
            .unwrap();
    }

    async fn remote_text(bearer: &str, vault_id: &str, path: &str) -> String {
        let keys = derive_keys(&load_key_from_keychain(vault_id).unwrap());
        let blob = vault_client(bearer, vault_id)
            .get_file(path, &keys)
            .await
            .unwrap();
        let bytes = obsink_core::decrypt(&keys.content_enc, &blob).unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}
