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
    hosted_worker_url, normalize_worker_url, prepare_sync, sync_manifest_path, ApiClient,
    AuthClient, Conflict, ConflictResolution, ConflictResolutionChoice, CreateVaultRequest,
    KeyBytes, ProgressEvent, ProgressSink, SyncActionKind, SyncPhase, VaultConfig,
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

/// Which server to talk to and how to authenticate. `--worker-url` defaults
/// to ObSink Cloud (the hosted Worker); `--api-key` is the self-hosted
/// credential and is remembered in the keychain, so it is needed once.
#[derive(Debug, clap::Args)]
struct ServerArgs {
    #[arg(long, env = "OBSINK_WORKER_URL")]
    worker_url: Option<String>,
    #[arg(long, env = "OBSINK_API_KEY", hide_env_values = true)]
    api_key: Option<String>,
}

impl ServerArgs {
    fn url(&self) -> String {
        normalize_worker_url(self.worker_url.as_deref().unwrap_or(&hosted_worker_url()))
    }
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Sign in to ObSink Cloud (or a self-hosted Worker with accounts enabled)
    /// with an emailed one-time code.
    Login {
        #[arg(long)]
        email: Option<String>,
        /// Skip the prompt (scripts): the 6-digit code from the email.
        #[arg(long)]
        code: Option<String>,
        #[arg(long, env = "OBSINK_WORKER_URL")]
        worker_url: Option<String>,
        #[arg(long)]
        device_name: Option<String>,
    },
    /// Sign out this device (revokes the session and forgets the credential).
    Logout {
        #[arg(long, env = "OBSINK_WORKER_URL")]
        worker_url: Option<String>,
    },
    /// Show the signed-in account and its devices.
    Whoami {
        #[arg(long, env = "OBSINK_WORKER_URL")]
        worker_url: Option<String>,
    },
    /// List the vaults the current credential can see.
    Vaults {
        #[command(flatten)]
        server: ServerArgs,
    },
    /// Create a new vault and sync this directory into it.
    Init {
        #[command(flatten)]
        server: ServerArgs,
        #[arg(long)]
        vault_name: String,
        #[arg(short, long, default_value = ".")]
        directory: PathBuf,
        #[arg(long)]
        passphrase: Option<String>,
    },
    /// Attach this directory to an existing vault.
    Connect {
        #[command(flatten)]
        server: ServerArgs,
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

/// On-disk config. The bearer (session token or self-hosted API key) is NOT
/// here — it lives in the keychain under `bearer:<worker_url>`. `api_key` is
/// only read for one-time migration of pre-accounts configs.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CliConfig {
    worker_url: String,
    vault_id: String,
    local_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
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
        Commands::Login {
            email,
            code,
            worker_url,
            device_name,
        } => {
            let url = normalize_worker_url(worker_url.as_deref().unwrap_or(&hosted_worker_url()));
            let auth = AuthClient::new(&url);
            let caps = auth.capabilities().await?;
            if !caps.auth.email {
                return Err(format!(
                    "{url} does not offer email sign-in; use --api-key with a self-hosted Worker"
                )
                .into());
            }
            let email = match email {
                Some(email) => email,
                None => prompt_line("Email: ")?,
            };
            let start = auth.email_start(&email).await?;
            let code = match (code, start.code) {
                (Some(code), _) => code,
                (None, Some(dev_code)) => {
                    eprintln!("(dev server returned the code inline)");
                    dev_code
                }
                (None, None) => {
                    println!("Sent a 6-digit code to {email}.");
                    prompt_line("Code: ")?
                }
            };
            let device = device_name.unwrap_or_else(default_device_name);
            let session = auth.email_verify(&email, code.trim(), &device).await?;
            save_secret(&bearer_account(&url), &session.token)?;
            println!(
                "signed in as {} on {url}",
                session.user.email.unwrap_or(session.user.id)
            );
        }
        Commands::Logout { worker_url } => {
            let url = normalize_worker_url(worker_url.as_deref().unwrap_or(&hosted_worker_url()));
            let account = bearer_account(&url);
            match load_secret(&account) {
                Ok(token) if token.starts_with("os_") => {
                    if let Err(error) = AuthClient::new(&url).logout(&token).await {
                        eprintln!("warning: could not revoke the session server-side: {error}");
                    }
                }
                Ok(_) => {}
                Err(_) => {
                    println!("not signed in to {url}");
                    return Ok(());
                }
            }
            delete_secret(&account);
            println!("signed out of {url}");
        }
        Commands::Whoami { worker_url } => {
            let url = normalize_worker_url(worker_url.as_deref().unwrap_or(&hosted_worker_url()));
            let token = load_secret(&bearer_account(&url))
                .map_err(|_| format!("not signed in to {url}; run `obsink login`"))?;
            let me = AuthClient::new(&url).me(&token).await?;
            println!("server: {url}");
            match me.user {
                Some(user) => {
                    println!("account: {} ({})", user.email.unwrap_or_default(), user.id);
                    for session in me.sessions {
                        println!(
                            "  device: {}{}",
                            session.device_name,
                            if session.current { " (this device)" } else { "" }
                        );
                    }
                }
                None => println!("credential: self-hosted API key ({})", me.kind),
            }
        }
        Commands::Vaults { server } => {
            let (url, bearer) = resolve_server(&server)?;
            let client = ApiClient::new(VaultConfig {
                worker_url: url,
                api_key: bearer,
                vault_id: String::new(),
                local_path: String::new(),
            });
            let vaults = client.list_vaults().await?;

            for vault in vaults {
                println!("{} {}", vault.id, vault.name);
            }
        }
        Commands::Init {
            server,
            vault_name,
            directory,
            passphrase,
        } => {
            let (url, bearer) = resolve_server(&server)?;
            let client = ApiClient::new(VaultConfig {
                worker_url: url.clone(),
                api_key: bearer,
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
            save_secret(&vault_id, &hex::encode(key))?;

            let config = CliConfig {
                worker_url: url,
                vault_id,
                local_path: directory.display().to_string(),
                api_key: None,
            };
            save_config(&config)?;
            run_sync_for_config(&config, &key).await?;

            println!("connected vault {}", config.vault_id);
            println!("config: {}", config_path()?.display());
        }
        Commands::Connect {
            server,
            vault_id,
            directory,
            passphrase,
        } => {
            let (url, _bearer) = resolve_server(&server)?;
            let key = derive_key_from_passphrase(passphrase, &vault_id)?;

            let config = CliConfig {
                worker_url: url,
                vault_id,
                local_path: directory.display().to_string(),
                api_key: None,
            };

            validate_passphrase(&config, &key).await?;
            save_secret(&config.vault_id, &hex::encode(key))?;
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

            let remote = ApiClient::new(to_vault_config(&stored)?)
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

/// Work out the Worker URL and bearer for a server-facing command. A supplied
/// `--api-key` is remembered in the keychain for the URL; otherwise the stored
/// credential (session token from `login`, or an earlier `--api-key`) is used.
fn resolve_server(server: &ServerArgs) -> Result<(String, String), Box<dyn std::error::Error>> {
    let url = server.url();
    let account = bearer_account(&url);
    if let Some(api_key) = server.api_key.as_deref().filter(|key| !key.is_empty()) {
        save_secret(&account, api_key)?;
        return Ok((url, api_key.to_string()));
    }
    match load_secret(&account) {
        Ok(bearer) => Ok((url, bearer)),
        Err(_) => Err(format!(
            "no credential for {url}: run `obsink login` (ObSink Cloud) or pass --api-key (self-hosted)"
        )
        .into()),
    }
}

fn bearer_account(worker_url: &str) -> String {
    format!("bearer:{}", normalize_worker_url(worker_url))
}

fn default_device_name() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .map(|name| format!("{name} (CLI)"))
        .unwrap_or_else(|| "CLI".to_string())
}

fn prompt_line(prompt: &str) -> Result<String, Box<dyn std::error::Error>> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let value = input.trim().to_string();
    if value.is_empty() {
        return Err("nothing entered".into());
    }
    Ok(value)
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
    let vault_config = to_vault_config(config)?;

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
    let client = ApiClient::new(to_vault_config(config)?);
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

fn to_vault_config(config: &CliConfig) -> Result<VaultConfig, Box<dyn std::error::Error>> {
    let bearer = load_secret(&bearer_account(&config.worker_url)).map_err(|_| {
        format!(
            "no credential for {}: run `obsink login` or `obsink connect --api-key ...`",
            config.worker_url
        )
    })?;
    Ok(VaultConfig {
        worker_url: config.worker_url.clone(),
        api_key: bearer,
        vault_id: config.vault_id.clone(),
        local_path: config.local_path.clone(),
    })
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

/// Load the config, migrating a pre-accounts file (plaintext `api_key`) by
/// moving the key into the keychain and rewriting the file without it.
fn load_config() -> Result<CliConfig, Box<dyn std::error::Error>> {
    let path = config_path()?;
    let contents = fs::read_to_string(path)?;
    let mut config: CliConfig = toml::from_str(&contents)?;
    config.worker_url = normalize_worker_url(&config.worker_url);
    if let Some(api_key) = config.api_key.take() {
        save_secret(&bearer_account(&config.worker_url), &api_key)?;
        save_config(&config)?;
        eprintln!("moved the API key from config.toml into the keychain");
    }
    Ok(config)
}

// --- Keychain -----------------------------------------------------------------
//
// Two kinds of secret share the `obsink` service: the derived vault key
// (account = vault ID, hex) and the server bearer (account = `bearer:<url>`).
// `OBSINK_KEYRING_DIR` swaps the macOS keychain for a directory of files
// (Linux, CI, harnesses).

fn keyring_dir() -> Option<PathBuf> {
    std::env::var_os("OBSINK_KEYRING_DIR").map(PathBuf::from)
}

fn keyring_file(dir: &std::path::Path, account: &str) -> PathBuf {
    // Accounts contain `:` and `/` (bearer URLs); keep filenames flat.
    dir.join(account.replace(['/', ':'], "_"))
}

fn save_secret(account: &str, value: &str) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(dir) = keyring_dir() {
        fs::create_dir_all(&dir)?;
        fs::write(keyring_file(&dir, account), value)?;
        return Ok(());
    }

    let _ = Command::new("security")
        .args([
            "delete-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            account,
        ])
        .output();

    let output = Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            account,
            "-w",
            value,
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

fn load_secret(account: &str) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(dir) = keyring_dir() {
        return Ok(fs::read_to_string(keyring_file(&dir, account))?
            .trim()
            .to_string());
    }

    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-w",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            account,
        ])
        .output()?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into());
    }

    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn delete_secret(account: &str) {
    if let Some(dir) = keyring_dir() {
        let _ = fs::remove_file(keyring_file(&dir, account));
        return;
    }
    let _ = Command::new("security")
        .args([
            "delete-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            account,
        ])
        .output();
}

fn load_key_from_keychain(vault_id: &str) -> Result<KeyBytes, Box<dyn std::error::Error>> {
    let bytes = hex::decode(load_secret(vault_id)?)?;

    if bytes.len() != 32 {
        return Err("stored key has invalid length".into());
    }

    let mut key = [0_u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}
