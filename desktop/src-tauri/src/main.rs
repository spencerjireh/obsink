use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::UNIX_EPOCH,
};

use dirs::home_dir;
use obsink_core::{
    build_working_manifest_for_path, complete_sync, derive_key, diff_local_and_remote,
    prepare_sync, sync_manifest_path, ApiClient, Conflict, ConflictResolution,
    CreateVaultRequest, KeyBytes, SyncPlan, SyncResult, VaultConfig,
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
    let vault_config = to_vault_config(vault);
    let remote_manifest = ApiClient::new(vault_config)
        .get_manifest()
        .await
        .map_err(err_string)?;
    let local_manifest = build_working_manifest_for_path(&local_root).map_err(err_string)?;
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
    let local_manifest =
        build_working_manifest_for_path(Path::new(&vault.local_path)).map_err(err_string)?;
    let remote_manifest = ApiClient::new(to_vault_config(&vault))
        .get_manifest()
        .await
        .map_err(err_string)?;
    Ok(diff_local_and_remote(&local_manifest, &remote_manifest))
}

#[tauri::command]
async fn sync_vault(
    vault_id: Option<String>,
    state: tauri::State<'_, AppState>,
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
async fn resolve_conflict(
    vault_id: String,
    resolutions: Vec<ConflictResolution>,
    state: tauri::State<'_, AppState>,
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
    let client = ApiClient::new(to_vault_config(vault));
    let manifest = client.get_manifest().await.map_err(err_string)?;
    if let Some((path, _)) = manifest.iter().find(|(_, entry)| !entry.deleted) {
        let blob = client.get_file(path).await.map_err(err_string)?;
        obsink_core::decrypt(key, &blob).map_err(err_string)?;
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

fn main() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            add_vault,
            get_manifest_diff,
            get_status,
            get_vaults,
            resolve_conflict,
            sync_vault,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
