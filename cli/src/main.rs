use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    process::Command,
};

use clap::{Parser, Subcommand};
use dirs::home_dir;
use obsink_core::{
    build_manifest_from_dir, complete_sync, derive_key, derive_keys, diff_local_and_remote,
    prepare_sync, sync_manifest_path, ApiClient, Conflict, ConflictResolution,
    ConflictResolutionChoice, CreateVaultRequest, KeyBytes, ProgressEvent, ProgressSink,
    SyncActionKind, SyncPhase, VaultConfig,
};
use rpassword::prompt_password;
use serde::{Deserialize, Serialize};

const CONFIG_FILE: &str = ".obsink/config.toml";
const KEYCHAIN_SERVICE: &str = "obsink";

#[derive(Debug, Parser)]
#[command(name = "obsink")]
#[command(about = "Local-first Obsidian vault sync tooling")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Vaults {
        #[arg(long)]
        worker_url: String,
        #[arg(long)]
        api_key: String,
    },
    Init {
        #[arg(long)]
        worker_url: String,
        #[arg(long)]
        api_key: String,
        #[arg(long)]
        vault_name: String,
        #[arg(short, long, default_value = ".")]
        directory: PathBuf,
        #[arg(long)]
        passphrase: Option<String>,
    },
    Connect {
        #[arg(long)]
        worker_url: String,
        #[arg(long)]
        api_key: String,
        #[arg(long)]
        vault_id: String,
        #[arg(short, long, default_value = ".")]
        directory: PathBuf,
        #[arg(long)]
        passphrase: Option<String>,
    },
    Status {
        #[arg(short, long)]
        directory: Option<PathBuf>,
    },
    Sync,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CliConfig {
    worker_url: String,
    api_key: String,
    vault_id: String,
    local_path: String,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

/// Logs go to stderr (stdout stays clean for scripted parsing). Verbosity is
/// controlled by `RUST_LOG` (e.g. `RUST_LOG=obsink_core=debug`); default warn.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

#[tokio::main]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();
    let cli = Cli::parse();

    match cli.command {
        Commands::Vaults {
            worker_url,
            api_key,
        } => {
            let client = ApiClient::new(VaultConfig {
                worker_url,
                api_key,
                vault_id: String::new(),
                local_path: String::new(),
            });
            let vaults = client.list_vaults().await?;

            for vault in vaults {
                println!("{} {}", vault.id, vault.name);
            }
        }
        Commands::Init {
            worker_url,
            api_key,
            vault_name,
            directory,
            passphrase,
        } => {
            let client = ApiClient::new(VaultConfig {
                worker_url: worker_url.clone(),
                api_key: api_key.clone(),
                vault_id: String::new(),
                local_path: directory.display().to_string(),
            });
            let response = client
                .create_vault(&CreateVaultRequest {
                    name: vault_name,
                    max_file_size: 50 * 1024 * 1024,
                })
                .await?;

            let vault_id = response.vault.id;
            let key = derive_key_from_passphrase(passphrase, &vault_id)?;
            save_key_to_keychain(&vault_id, &key)?;

            let config = CliConfig {
                worker_url,
                api_key,
                vault_id,
                local_path: directory.display().to_string(),
            };
            save_config(&config)?;
            run_sync_for_config(&config, &key).await?;

            println!("connected vault {}", config.vault_id);
            println!("config: {}", config_path()?.display());
        }
        Commands::Connect {
            worker_url,
            api_key,
            vault_id,
            directory,
            passphrase,
        } => {
            let key = derive_key_from_passphrase(passphrase, &vault_id)?;

            let config = CliConfig {
                worker_url,
                api_key,
                vault_id,
                local_path: directory.display().to_string(),
            };

            validate_passphrase(&config, &key).await?;
            save_key_to_keychain(&config.vault_id, &key)?;
            save_config(&config)?;
            run_sync_for_config(&config, &key).await?;

            println!("config: {}", config_path()?.display());
        }
        Commands::Status { directory } => {
            let stored = load_config()?;
            let keys = derive_keys(&load_key_from_keychain(&stored.vault_id)?);
            let directory = directory.unwrap_or_else(|| PathBuf::from(&stored.local_path));
            let manifest = build_manifest_from_dir(&directory, &keys)?;
            let total_size: u64 = manifest.values().map(|entry| entry.size).sum();

            println!("directory: {}", directory.display());
            println!("files: {}", manifest.len());
            println!("bytes: {total_size}");

            let remote = ApiClient::new(to_vault_config(&stored))
                .get_manifest(&keys)
                .await?;
            let diff = diff_local_and_remote(&manifest, &remote);
            println!("upload: {}", diff.upload.len());
            println!("download: {}", diff.download.len());
            println!("conflicts: {}", diff.conflicts.len());
        }
        Commands::Sync => {
            let config = load_config()?;
            let key = load_key_from_keychain(&config.vault_id)?;
            run_sync_for_config(&config, &key).await?;
        }
    }

    Ok(())
}

/// Prints sync progress to stderr so it doesn't interleave with the stdout
/// result summary (which scripts may parse).
struct CliProgress;

impl ProgressSink for CliProgress {
    fn report(&self, event: ProgressEvent) {
        match event {
            ProgressEvent::Phase(phase) => match phase {
                SyncPhase::Downloading => eprintln!("downloading…"),
                SyncPhase::ResolvingConflicts => eprintln!("resolving conflicts…"),
                SyncPhase::Uploading => eprintln!("uploading…"),
            },
            ProgressEvent::FileStarted {
                path, kind, index, total,
            } => {
                let verb = match kind {
                    SyncActionKind::Download => "downloading",
                    SyncActionKind::Upload => "uploading",
                    _ => "syncing",
                };
                eprintln!("[{}/{}] {verb}: {path}", index + 1, total);
            }
            ProgressEvent::FileCompleted { path, bytes } => {
                eprintln!("  done ({bytes} B): {path}");
            }
            ProgressEvent::FileFailed { path, error } => {
                eprintln!("  FAILED: {path}: {error}");
            }
            ProgressEvent::Done {
                uploaded,
                downloaded,
                failed,
            } => {
                eprintln!(
                    "sync summary: {uploaded} uploaded, {downloaded} downloaded, {failed} failed"
                );
            }
        }
    }
}

async fn run_sync_for_config(
    config: &CliConfig,
    key: &KeyBytes,
) -> Result<(), Box<dyn std::error::Error>> {
    let vault_config = to_vault_config(config);

    loop {
        let plan = prepare_sync(&vault_config, key, &CliProgress).await?;
        let resolutions = prompt_conflict_resolutions(&plan.conflicts)?;
        let result = complete_sync(&vault_config, key, &plan, &resolutions, &CliProgress).await?;

        println!("downloaded: {}", result.download.len());
        println!("uploaded: {}", result.upload.len());

        if !result.failures.is_empty() {
            let fatal = result.failures.iter().any(|failure| failure.fatal);
            eprintln!(
                "{} file(s) failed this sync:",
                result.failures.len()
            );
            for failure in &result.failures {
                let tag = if failure.fatal { "FATAL" } else { "skipped" };
                eprintln!("  [{tag}] {}: {}", failure.path, failure.error);
            }
            if fatal {
                eprintln!("a fatal error stopped the sync early; re-run `obsink sync` to resume");
            }
        }

        if result.conflicts.is_empty() {
            if result.failures.is_empty() {
                println!("sync complete");
            } else {
                println!("sync complete with failures (see above)");
            }
            println!(
                "manifest: {}",
                sync_manifest_path(&PathBuf::from(&config.local_path)).display()
            );
            return Ok(());
        }

        println!("late conflicts detected: {}", result.conflicts.len());
    }
}

fn prompt_conflict_resolutions(
    conflicts: &[Conflict],
) -> Result<Vec<ConflictResolution>, Box<dyn std::error::Error>> {
    let mut resolutions = Vec::new();

    for conflict in conflicts {
        println!("conflict: {}", conflict.path);
        println!("  1. keep local");
        println!("  2. keep remote");
        println!("  3. keep both");

        loop {
            print!("choose [1-3]: ");
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;

            let choice = match input.trim() {
                "1" => Some(ConflictResolutionChoice::KeepLocal),
                "2" => Some(ConflictResolutionChoice::KeepRemote),
                "3" => Some(ConflictResolutionChoice::KeepBoth),
                _ => None,
            };

            if let Some(choice) = choice {
                resolutions.push(ConflictResolution {
                    path: conflict.path.clone(),
                    choice,
                });
                break;
            }
        }
    }

    Ok(resolutions)
}

async fn validate_passphrase(
    config: &CliConfig,
    key: &KeyBytes,
) -> Result<(), Box<dyn std::error::Error>> {
    let keys = derive_keys(key);
    let client = ApiClient::new(to_vault_config(config));
    let manifest = client.get_manifest(&keys).await?;

    if let Some((path, entry)) = manifest.iter().find(|(_, entry)| !entry.deleted) {
        let blob = client.get_file(path, &keys).await?;
        obsink_core::decrypt(&keys.content_enc, &blob)?;
        println!("validated passphrase against {path}");
        println!("remote size: {} bytes", entry.size);
    }

    Ok(())
}

fn derive_key_from_passphrase(
    passphrase: Option<String>,
    vault_id: &str,
) -> Result<KeyBytes, Box<dyn std::error::Error>> {
    let passphrase = match passphrase {
        Some(passphrase) => passphrase,
        None => prompt_password("Passphrase: ")?,
    };

    Ok(derive_key(&passphrase, vault_id.as_bytes())?)
}

fn to_vault_config(config: &CliConfig) -> VaultConfig {
    VaultConfig {
        worker_url: config.worker_url.clone(),
        api_key: config.api_key.clone(),
        vault_id: config.vault_id.clone(),
        local_path: config.local_path.clone(),
    }
}

fn config_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    // OBSINK_HOME overrides the config location without touching HOME, so tests
    // can isolate per-device config while leaving macOS keychain resolution intact.
    let base = match std::env::var_os("OBSINK_HOME") {
        Some(value) => PathBuf::from(value),
        None => home_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory not found"))?,
    };
    Ok(base.join(CONFIG_FILE))
}

fn save_config(config: &CliConfig) -> Result<(), Box<dyn std::error::Error>> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, toml::to_string_pretty(config)?)?;
    Ok(())
}

fn load_config() -> Result<CliConfig, Box<dyn std::error::Error>> {
    let path = config_path()?;
    let contents = fs::read_to_string(path)?;
    Ok(toml::from_str(&contents)?)
}

fn save_key_to_keychain(vault_id: &str, key: &KeyBytes) -> Result<(), Box<dyn std::error::Error>> {
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
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into());
    }

    Ok(())
}

fn load_key_from_keychain(vault_id: &str) -> Result<KeyBytes, Box<dyn std::error::Error>> {
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
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into());
    }

    let hex_value = String::from_utf8(output.stdout)?.trim().to_string();
    let bytes = hex::decode(hex_value)?;

    if bytes.len() != 32 {
        return Err("stored key has invalid length".into());
    }

    let mut key = [0_u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}
