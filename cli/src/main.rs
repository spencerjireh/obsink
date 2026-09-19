use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
};

use clap::{Parser, Subcommand};
use dirs::home_dir;
use obsink_core::{
    complete_sync, derive_key, derive_keys, diff_local_and_remote, fetch_remote_manifest,
    keychain::{delete_secret, load_secret, save_secret},
    load_local_state, normalize_server_url, prepare_sync, sync_manifest_path, write_atomic,
    ApiClient, AuthClient, Conflict, ConflictResolution, ConflictResolutionChoice,
    CreateVaultRequest, KeyBytes, ProgressEvent, ProgressSink, SyncActionKind, SyncPhase, SyncPlan,
    VaultConfig,
};
use rpassword::prompt_password;
use serde::{Deserialize, Serialize};

const CONFIG_FILE: &str = ".obsink/config.toml";

#[derive(Debug, Parser)]
#[command(name = "obsink")]
#[command(version)]
#[command(about = "Local-first Obsidian vault sync tooling")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// Which server to talk to and how to authenticate. `--server-url` falls back
/// to the URL in the saved config; `--api-key` is the operator bearer (admin
/// and harness use) and is remembered in the keychain, so it is needed once.
#[derive(Debug, clap::Args)]
struct ServerArgs {
    #[arg(long, env = "OBSINK_SERVER_URL")]
    server_url: Option<String>,
    #[arg(long, env = "OBSINK_API_KEY", hide_env_values = true)]
    api_key: Option<String>,
}

impl ServerArgs {
    fn url(&self) -> Result<String, Box<dyn std::error::Error>> {
        resolve_server_url(self.server_url.as_deref())
    }
}

/// Explicit flag/env first, then the saved config, otherwise an error: there
/// is no built-in default server.
fn resolve_server_url(explicit: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(url) = explicit.map(str::trim).filter(|url| !url.is_empty()) {
        return Ok(normalize_server_url(url));
    }
    if let Ok(config) = load_config() {
        return Ok(config.server_url);
    }
    Err("no server: pass --server-url or set OBSINK_SERVER_URL".into())
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Sign in to a server with an emailed one-time code.
    Login {
        #[arg(long)]
        email: Option<String>,
        /// Skip the prompt (scripts): the 6-digit code from the email.
        #[arg(long)]
        code: Option<String>,
        #[arg(long, env = "OBSINK_SERVER_URL")]
        server_url: Option<String>,
        #[arg(long)]
        device_name: Option<String>,
        /// Needed to create a new account once the server has any user.
        #[arg(long)]
        invite_code: Option<String>,
    },
    /// Mint an invite code so someone else can create an account.
    Invite {
        #[arg(long, env = "OBSINK_SERVER_URL")]
        server_url: Option<String>,
        /// List the invites you have minted instead of creating one.
        #[arg(long)]
        list: bool,
    },
    /// Sign out this device (revokes the session and forgets the credential).
    Logout {
        #[arg(long, env = "OBSINK_SERVER_URL")]
        server_url: Option<String>,
    },
    /// Show the signed-in account and its devices.
    Whoami {
        #[arg(long, env = "OBSINK_SERVER_URL")]
        server_url: Option<String>,
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

/// On-disk config. The bearer (session token or operator API key) is NOT
/// here — it lives in the keychain under `bearer:<server_url>`. The
/// `server_url` alias reads configs written before the server pivot.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CliConfig {
    #[serde(alias = "server_url")]
    server_url: String,
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
        Commands::Login {
            email,
            code,
            server_url,
            device_name,
            invite_code,
        } => {
            let url = resolve_server_url(server_url.as_deref())?;
            let auth = AuthClient::new(&url);
            let caps = auth.capabilities().await?;
            if !caps.auth.email {
                return Err(format!(
                    "{url} does not offer email sign-in (the operator has not configured SMTP)"
                )
                .into());
            }
            let email = match email {
                Some(email) => email,
                None => prompt_line("Email: ")?,
            };
            // A supplied --code belongs to a code already sent; requesting
            // another would replace it server-side.
            let code = match code {
                Some(code) => code,
                None => {
                    let start = auth.email_start(&email).await?;
                    match start.code {
                        Some(dev_code) => {
                            eprintln!("(dev server returned the code inline)");
                            dev_code
                        }
                        None => {
                            println!("Sent a 6-digit code to {email}.");
                            prompt_line("Code: ")?
                        }
                    }
                }
            };
            let device = device_name.unwrap_or_else(default_device_name);
            let session = match auth
                .email_verify(&email, code.trim(), &device, invite_code.as_deref())
                .await
            {
                Ok(session) => session,
                Err(obsink_core::AuthError::Server { status, message })
                    if status.as_u16() == 403 && invite_code.is_none() =>
                {
                    return Err(format!("{message} (pass --invite-code)").into());
                }
                Err(error) => return Err(error.into()),
            };
            save_secret(&bearer_account(&url), &session.token)?;
            println!(
                "signed in as {} on {url}",
                session.user.email.unwrap_or(session.user.id)
            );
        }
        Commands::Logout { server_url } => {
            let url = resolve_server_url(server_url.as_deref())?;
            let account = bearer_account(&url);
            match load_secret(&account) {
                Ok(token) => {
                    if let Err(error) = AuthClient::new(&url).logout(&token).await {
                        eprintln!("warning: could not revoke the session server-side: {error}");
                    }
                }
                Err(_) => {
                    println!("not signed in to {url}");
                    return Ok(());
                }
            }
            delete_secret(&account);
            println!("signed out of {url}");
        }
        Commands::Whoami { server_url } => {
            let url = resolve_server_url(server_url.as_deref())?;
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
                            if session.current {
                                " (this device)"
                            } else {
                                ""
                            }
                        );
                    }
                }
                None => println!("credential: operator API key ({})", me.kind),
            }
            if let Some(usage) = me.usage {
                let limit = match (usage.max_vault_bytes, usage.max_vaults) {
                    (Some(bytes), Some(vaults)) => {
                        format!("; limit {bytes} bytes per vault, {vaults} vaults")
                    }
                    _ => String::new(),
                };
                println!(
                    "usage: {} bytes across {} vault(s){limit}",
                    usage.total_bytes,
                    usage.vaults.len()
                );
                for vault in usage.vaults {
                    println!("  vault {}: {} bytes", vault.id, vault.bytes);
                }
            }
        }
        Commands::Invite { server_url, list } => {
            let url = resolve_server_url(server_url.as_deref())?;
            let token = load_secret(&bearer_account(&url))
                .map_err(|_| format!("not signed in to {url}; run `obsink login`"))?;
            let auth = AuthClient::new(&url);
            if list {
                for invite in auth.list_invites(&token).await? {
                    println!(
                        "{} {} (expires {})",
                        invite.code, invite.status, invite.expires
                    );
                }
            } else {
                let invite = auth.create_invite(&token).await?;
                println!("invite code: {} (expires {})", invite.code, invite.expires);
            }
        }
        Commands::Vaults { server } => {
            let (url, bearer) = resolve_server(&server)?;
            let client = ApiClient::new(VaultConfig {
                server_url: url,
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
            let directory = resolve_vault_dir(&directory)?;
            let client = ApiClient::new(VaultConfig {
                server_url: url.clone(),
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
                server_url: url,
                vault_id,
                local_path: directory.display().to_string(),
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
            let directory = resolve_vault_dir(&directory)?;
            let key = derive_key_from_passphrase(passphrase, &vault_id)?;

            let config = CliConfig {
                server_url: url,
                vault_id,
                local_path: directory.display().to_string(),
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
            let directory = match directory {
                Some(directory) => resolve_vault_dir(&directory)?,
                None => PathBuf::from(&stored.local_path),
            };
            let local = load_local_state(&directory, &keys)?;
            let live = local.working.values().filter(|entry| !entry.deleted);
            let total_size: u64 = live.clone().map(|entry| entry.size).sum();

            println!("directory: {}", directory.display());
            println!("files: {}", live.count());
            println!("bytes: {total_size}");

            let remote = fetch_remote_manifest(
                &ApiClient::new(to_vault_config(&stored)?),
                &directory,
                &keys,
            )
            .await?;
            let diff = diff_local_and_remote(&local.base, &local.working, &remote);
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

/// Work out the server URL and bearer for a server-facing command. A supplied
/// `--api-key` is remembered in the keychain for the URL; otherwise the stored
/// credential (session token from `login`, or an earlier `--api-key`) is used.
fn resolve_server(server: &ServerArgs) -> Result<(String, String), Box<dyn std::error::Error>> {
    let url = server.url()?;
    let account = bearer_account(&url);
    if let Some(api_key) = server.api_key.as_deref().filter(|key| !key.is_empty()) {
        save_secret(&account, api_key)?;
        return Ok((url, api_key.to_string()));
    }
    match load_secret(&account) {
        Ok(bearer) => Ok((url, bearer)),
        Err(_) => Err(format!(
            "no credential for {url}: run `obsink login --server-url {url}` (or pass --api-key for the operator bearer)"
        )
        .into()),
    }
}

fn bearer_account(server_url: &str) -> String {
    format!("bearer:{}", normalize_server_url(server_url))
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
                path,
                kind,
                index,
                total,
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

    let mut plan = prepare_sync(&vault_config, key, &CliProgress).await?;
    loop {
        let resolutions = prompt_conflict_resolutions(&plan.conflicts)?;
        let result = complete_sync(&vault_config, key, &plan, &resolutions, &CliProgress).await?;

        println!("downloaded: {}", result.download.len());
        println!("uploaded: {}", result.upload.len());

        if !result.failures.is_empty() {
            let fatal = result.failures.iter().any(|failure| failure.fatal);
            eprintln!("{} file(s) failed this sync:", result.failures.len());
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

        // Another device wrote between our manifest fetch and the upload; the
        // 409s come back as a conflict-only plan to resolve in the next round.
        println!("late conflicts detected: {}", result.conflicts.len());
        plan = SyncPlan::from_late_conflicts(&result).expect("conflicts are non-empty");
    }
}

/// The vault directory as stored in the config: created if missing and made
/// absolute, so `obsink sync` from any working directory finds the same vault.
fn resolve_vault_dir(directory: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(directory)?;
    fs::canonicalize(directory)
}

fn prompt_conflict_resolutions(
    conflicts: &[Conflict],
) -> Result<Vec<ConflictResolution>, Box<dyn std::error::Error>> {
    let mut resolutions = Vec::new();

    for conflict in conflicts {
        // "Keep both" needs two live versions; with a deletion on one side
        // the choice is only which side wins.
        let both_live = !conflict.local.deleted && !conflict.remote.deleted;
        println!("conflict: {}", conflict.path);
        println!(
            "  1. keep local{}",
            if conflict.local.deleted {
                " (deleted here)"
            } else {
                ""
            }
        );
        println!(
            "  2. keep remote{}",
            if conflict.remote.deleted {
                " (deleted on the server)"
            } else {
                ""
            }
        );
        if both_live {
            println!("  3. keep both");
        }

        loop {
            print!("choose [1-{}]: ", if both_live { 3 } else { 2 });
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;

            let choice = match input.trim() {
                "1" => Some(ConflictResolutionChoice::KeepLocal),
                "2" => Some(ConflictResolutionChoice::KeepRemote),
                "3" if both_live => Some(ConflictResolutionChoice::KeepBoth),
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
    let manifest = fetch_remote_manifest(&client, Path::new(&config.local_path), &keys).await?;

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
    let bearer = load_secret(&bearer_account(&config.server_url)).map_err(|_| {
        format!(
            "no credential for {}: run `obsink login` or `obsink connect --api-key ...`",
            config.server_url
        )
    })?;
    Ok(VaultConfig {
        server_url: config.server_url.clone(),
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
    write_atomic(&path, toml::to_string_pretty(config)?.as_bytes())?;
    Ok(())
}

fn load_config() -> Result<CliConfig, Box<dyn std::error::Error>> {
    let path = config_path()?;
    let contents = fs::read_to_string(path)?;
    let mut config: CliConfig = toml::from_str(&contents)?;
    config.server_url = normalize_server_url(&config.server_url);
    Ok(config)
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::resolve_vault_dir;

    #[test]
    fn resolve_vault_dir_returns_an_absolute_path() {
        let resolved = resolve_vault_dir(Path::new(".")).unwrap();

        assert!(resolved.is_absolute());
        assert_eq!(
            resolved,
            std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap()
        );
    }

    #[test]
    fn resolve_vault_dir_creates_missing_directories() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("new/vault");

        let resolved = resolve_vault_dir(&target).unwrap();

        assert!(target.is_dir());
        assert_eq!(resolved, std::fs::canonicalize(&target).unwrap());
    }
}
