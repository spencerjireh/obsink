use std::{
    collections::HashMap,
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
    build_working_manifest_for_path, complete_sync, derive_key, derive_keys, diff_local_and_remote,
    prepare_sync, sync_manifest_path, ApiClient, Conflict, ConflictResolution, CreateVaultRequest,
    KeyBytes, SyncPlan, SyncResult, VaultConfig,
};
use serde::{Deserialize, Serialize};

const APP_CONFIG_FILE: &str = ".obsink/app.json";
const KEYCHAIN_SERVICE: &str = "obsink";

#[derive(Default)]
struct AppState {
    pending_plans: Mutex<HashMap<String, SyncPlan>>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredVault {
    id: String,
    name: String,
    worker_url: String,
    api_key: String,
    local_path: String,
}

#[derive(Debug, Clone, Serialize)]
struct LocalVaultSummary {
    id: String,
    name: String,
    worker_url: String,
    local_path: String,
    active: bool,
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
    worker_url: String,
    api_key: String,
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
        .map(|vault| LocalVaultSummary {
            id: vault.id.clone(),
            name: vault.name.clone(),
            worker_url: vault.worker_url.clone(),
            local_path: vault.local_path.clone(),
            active: config.active_vault_id.as_deref() == Some(vault.id.as_str()),
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

    Ok(LocalVaultSummary {
        id: vault.id,
        name: vault.name,
        worker_url: vault.worker_url,
        local_path: vault.local_path,
        active: true,
    })
}

#[tauri::command]
async fn add_vault(request: AddVaultRequest) -> Result<LocalVaultSummary, String> {
    validate_request(&request)?;

    let client = ApiClient::new(VaultConfig {
        worker_url: request.worker_url.clone(),
        api_key: request.api_key.clone(),
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
        worker_url: request.worker_url.clone(),
        api_key: request.api_key.clone(),
        local_path: request.local_path.clone(),
    };

    validate_passphrase(&stored, &key).await?;
    save_key_to_keychain(&vault_id, &key).map_err(err_string)?;
    upsert_vault(stored.clone()).map_err(err_string)?;

    Ok(LocalVaultSummary {
        id: stored.id.clone(),
        name: stored.name.clone(),
        worker_url: stored.worker_url,
        local_path: stored.local_path,
        active: true,
    })
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
    let remote_manifest = ApiClient::new(vault_config)
        .get_manifest(&keys)
        .await
        .map_err(err_string)?;
    let local_manifest = build_working_manifest_for_path(&local_root, &keys).map_err(err_string)?;
    let diff = diff_local_and_remote(&local_manifest, &remote_manifest);

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
    let local_manifest =
        build_working_manifest_for_path(Path::new(&vault.local_path), &keys).map_err(err_string)?;
    let remote_manifest = ApiClient::new(to_vault_config(&vault))
        .get_manifest(&keys)
        .await
        .map_err(err_string)?;
    Ok(diff_local_and_remote(&local_manifest, &remote_manifest))
}

async fn sync_vault_inner(
    vault_id: Option<String>,
    state: &AppState,
) -> Result<SyncCommandResponse, String> {
    let vault = selected_vault(vault_id).map_err(err_string)?;
    let key = load_key_from_keychain(&vault.id).map_err(err_string)?;
    let plan = prepare_sync(&to_vault_config(&vault), &key)
        .await
        .map_err(err_string)?;

    if plan.conflicts.is_empty() {
        let result = complete_sync(&to_vault_config(&vault), &key, &plan, &[])
            .await
            .map_err(err_string)?;
        return Ok(SyncCommandResponse {
            completed_result: Some(result),
            pending_conflicts: Vec::new(),
        });
    }

    let pending_conflicts = plan.conflicts.clone();
    state
        .pending_plans
        .lock()
        .map_err(|_| "pending plan lock poisoned".to_string())?
        .insert(vault.id.clone(), plan);

    Ok(SyncCommandResponse {
        completed_result: None,
        pending_conflicts,
    })
}

#[tauri::command]
async fn sync_vault(
    vault_id: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<SyncCommandResponse, String> {
    sync_vault_inner(vault_id, &state).await
}

async fn resolve_conflict_inner(
    vault_id: String,
    resolutions: Vec<ConflictResolution>,
    state: &AppState,
) -> Result<SyncResult, String> {
    let vault = selected_vault(Some(vault_id.clone())).map_err(err_string)?;
    let plan = state
        .pending_plans
        .lock()
        .map_err(|_| "pending plan lock poisoned".to_string())?
        .remove(&vault_id)
        .ok_or_else(|| format!("no pending conflict set for {}", vault_id))?;
    let key = load_key_from_keychain(&vault.id).map_err(err_string)?;

    complete_sync(&to_vault_config(&vault), &key, &plan, &resolutions)
        .await
        .map_err(err_string)
}

#[tauri::command]
async fn resolve_conflict(
    vault_id: String,
    resolutions: Vec<ConflictResolution>,
    state: tauri::State<'_, AppState>,
) -> Result<SyncResult, String> {
    resolve_conflict_inner(vault_id, resolutions, &state).await
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
    if request.worker_url.trim().is_empty() {
        return Err("worker URL is required".into());
    }
    if request.api_key.trim().is_empty() {
        return Err("API key is required".into());
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
    let manifest = client.get_manifest(&keys).await.map_err(err_string)?;
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

fn to_vault_config(vault: &StoredVault) -> VaultConfig {
    VaultConfig {
        worker_url: vault.worker_url.clone(),
        api_key: vault.api_key.clone(),
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
    Ok(serde_json::from_str(&contents)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?)
}

fn save_app_config(config: &StoredAppConfig) -> Result<(), io::Error> {
    let path = app_config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        path,
        serde_json::to_vec_pretty(config)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
    )
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
    let key_hex = hex::encode(key);

    // Test/dev escape hatch: store the key as a file instead of the macOS
    // keychain so live integration tests can run non-interactively. Production
    // builds leave this unset and use the real keychain below.
    if let Ok(dir) = std::env::var("OBSINK_KEYRING_DIR") {
        return fs::write(PathBuf::from(dir).join(vault_id), key_hex);
    }

    let _ = Command::new("security")
        .args([
            "delete-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            vault_id,
        ])
        .output();

    let output = Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            vault_id,
            "-w",
            &key_hex,
        ])
        .output()?;

    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }

    Ok(())
}

fn load_key_from_keychain(vault_id: &str) -> Result<KeyBytes, io::Error> {
    // Test/dev escape hatch (see save_key_to_keychain).
    if let Ok(dir) = std::env::var("OBSINK_KEYRING_DIR") {
        let hex_value = fs::read_to_string(PathBuf::from(dir).join(vault_id))?
            .trim()
            .to_string();
        let bytes = hex::decode(hex_value)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if bytes.len() != 32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "stored key has invalid length",
            ));
        }
        let mut key = [0_u8; 32];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }

    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-w",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            vault_id,
        ])
        .output()?;

    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }

    let hex_value = String::from_utf8(output.stdout)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        .trim()
        .to_string();
    let bytes = hex::decode(hex_value)
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
    use obsink_core::{derive_keys, load_manifest_from_disk, sync_manifest_path, ApiClient, ConflictResolutionChoice, VaultConfig};
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{Duration, SystemTime},
    };

    /// `#[ignore]`d live integration test: drives the real desktop command
    /// functions (add_vault / set_active_vault / get_status / sync_vault_inner /
    /// get_conflict_preview_inner / resolve_conflict_inner) end-to-end against a
    /// deployed Worker. Run with:
    ///   OBSINK_TEST_WORKER_URL=... OBSINK_TEST_API_KEY=... \
    ///   cargo test -p obsink-desktop live_tests -- --ignored --nocapture
    #[ignore]
    #[tokio::test]
    async fn desktop_flows_live() {
        let worker_url = env_or_panic("OBSINK_TEST_WORKER_URL");
        let api_key = env_or_panic("OBSINK_TEST_API_KEY");
        let passphrase =
            std::env::var("OBSINK_TEST_PASSPHRASE").unwrap_or_else(|_| {
                "obsink-test-passphrase-2026".to_string()
            });

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

        let state = AppState::default();
        let file_rel = "notes/a.md";

        // ===== OBS-3: add (Create) + upload + cross-device download =====
        let summary = add_vault(AddVaultRequest {
            mode: AddVaultMode::Create,
            worker_url: worker_url.clone(),
            api_key: api_key.clone(),
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
        assert!(get_vaults().unwrap().iter().any(|v| v.id == vault_id && v.active));

        fs::write(dir_a.join(file_rel), "content-A").unwrap();
        let resp = sync_vault_inner(Some(vault_id.clone()), &state).await.unwrap();
        assert!(resp.pending_conflicts.is_empty());
        assert_eq!(
            resp.completed_result.unwrap().upload.len(),
            1,
            "OBS-3: a.md should upload"
        );

        // Connect device B (same passphrase -> same key) and pull.
        add_vault(AddVaultRequest {
            mode: AddVaultMode::Connect,
            worker_url: worker_url.clone(),
            api_key: api_key.clone(),
            local_path: dir_b.to_string_lossy().into_owned(),
            vault_name: String::new(),
            vault_id: vault_id.clone(),
            passphrase: passphrase.clone(),
        })
        .await
        .unwrap(); // validate_passphrase decrypts a.md -> proves the key works
        let resp_b = sync_vault_inner(Some(vault_id.clone()), &state).await.unwrap();
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
        sync_vault_inner(Some(vault_id.clone()), &state).await.unwrap();
        // Repoint the active local folder at A (which is now behind the server).
        connect_local(&worker_url, &api_key, &vault_id, &passphrase, &dir_a).await;
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
            // Rebaseline: A's a.md == "REMOTE", then sync so server == local.
            fs::write(dir_a.join(file_rel), "REMOTE").unwrap();
            let _ = sync_vault_inner(Some(vault_id.clone()), &state).await.unwrap();
            let ts = server_modified_for(&dir_a, file_rel);

            // Engineer a conflict: different content, same (pinned) mtime.
            let local_text = format!("LOCAL-{choice:?}");
            fs::write(dir_a.join(file_rel), &local_text).unwrap();
            set_mtime(&dir_a.join(file_rel), ts);

            let resp = sync_vault_inner(Some(vault_id.clone()), &state).await.unwrap();
            assert_eq!(
                resp.pending_conflicts.len(),
                1,
                "OBS-4 ({choice:?}): expected 1 conflict"
            );
            assert!(resp.completed_result.is_none());

            // Side-by-side preview decrypts the remote blob via the desktop path.
            let preview = get_conflict_preview_inner(
                vault_id.clone(),
                file_rel.to_string(),
                &state,
            )
            .await
            .unwrap();
            assert_eq!(preview.local_text, local_text, "OBS-4 ({choice:?}): local preview");
            assert_eq!(preview.remote_text, "REMOTE", "OBS-4 ({choice:?}): remote preview");

            let result = resolve_conflict_inner(
                vault_id.clone(),
                vec![ConflictResolution {
                    path: file_rel.to_string(),
                    choice: choice.clone(),
                }],
                &state,
            )
            .await
            .unwrap();
            assert!(
                result.conflicts.is_empty(),
                "OBS-4 ({choice:?}): no late 409 expected"
            );

            match choice {
                ConflictResolutionChoice::KeepLocal => {
                    let remote = remote_text(&worker_url, &api_key, &vault_id, file_rel).await;
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
                        remote_text(&worker_url, &api_key, &vault_id, file_rel).await,
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
            worker_url: worker_url.clone(),
            api_key: api_key.clone(),
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
        assert_eq!(get_vaults().unwrap().len(), 2, "OBS-6: two vaults configured");

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

    fn env_or_panic(key: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| panic!("set {key}"))
    }

    async fn connect_local(
        worker_url: &str,
        api_key: &str,
        vault_id: &str,
        passphrase: &str,
        local_path: &Path,
    ) {
        add_vault(AddVaultRequest {
            mode: AddVaultMode::Connect,
            worker_url: worker_url.to_string(),
            api_key: api_key.to_string(),
            local_path: local_path.to_string_lossy().into_owned(),
            vault_name: String::new(),
            vault_id: vault_id.to_string(),
            passphrase: passphrase.to_string(),
        })
        .await
        .unwrap();
    }

    fn server_modified_for(dir: &Path, rel: &str) -> u64 {
        let manifest = load_manifest_from_disk(&sync_manifest_path(dir)).unwrap();
        manifest.get(rel).unwrap().modified
    }

    fn set_mtime(path: &Path, secs: u64) {
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    async fn remote_text(worker_url: &str, api_key: &str, vault_id: &str, path: &str) -> String {
        let key = load_key_from_keychain(vault_id).unwrap();
        let keys = derive_keys(&key);
        let config = VaultConfig {
            worker_url: worker_url.to_string(),
            api_key: api_key.to_string(),
            vault_id: vault_id.to_string(),
            local_path: String::new(),
        };
        let blob = ApiClient::new(config).get_file(path, &keys).await.unwrap();
        let bytes = obsink_core::decrypt(&keys.content_enc, &blob).unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}
