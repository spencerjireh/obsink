use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use clap::{Parser, Subcommand};
use dirs::home_dir;
use obsink_core::{
    complete_sync, create_account_key, daemon_channel, decode_base64, derive_keys,
    diff_local_and_remote, encode_base64, fetch_remote_manifest,
    keychain::{
        delete_account_key, delete_secret, load_account_key, load_bearer, load_or_create_device_id,
        load_secret, load_secret_opt, save_account_key, save_secret, user_account,
    },
    load_local_state, new_key, new_vault_id, normalize_server_url, prepare_sync,
    rewrap_account_key, run_daemon, sync_manifest_path, unwrap_vault_key, wrap_vault_key,
    write_atomic, ApiClient, AuthClient, Conflict, ConflictResolution, ConflictResolutionChoice,
    CreateVaultRequest, DaemonEvent, DaemonOptions, Device, DevicePlatform, KeyBytes,
    ProgressEvent, ProgressSink, SetKeysOutcome, SyncActionKind, SyncFailure, SyncPhase, SyncPlan,
    VaultConfig,
};
use rpassword::prompt_password;
use serde::{Deserialize, Serialize};

const CONFIG_FILE: &str = ".obsink/config.toml";

/// The server used when no flag, environment variable or saved config names
/// one: the public server, or whatever `OBSINK_SERVER_URL` was at build time
/// (self-hosters bake their own, as the desktop app does).
const FALLBACK_SERVER_URL: &str = match option_env!("OBSINK_SERVER_URL") {
    Some(url) => url,
    None => "https://obsink-api.spencerjireh.com",
};

#[derive(Debug, Parser)]
#[command(name = "obsink")]
#[command(version)]
#[command(about = "Local-first Obsidian vault sync tooling")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// Which server to talk to. `--server-url` falls back to the URL in the saved
/// config, then to the built-in default. The credential is always the session
/// from `obsink login` (there is no operator bearer, spec §4.1).
#[derive(Debug, clap::Args)]
struct ServerArgs {
    #[arg(long, env = "OBSINK_SERVER_URL")]
    server_url: Option<String>,
}

/// The passphrase for scripts: `OBSINK_PASSPHRASE` skips the prompt.
const PASSPHRASE_ENV: &str = "OBSINK_PASSPHRASE";
/// Spec §6.1: the wrapped account key is the new exposure, so the passphrase
/// has a floor.
const MIN_PASSPHRASE_CHARS: usize = 12;

impl ServerArgs {
    fn url(&self) -> Result<String, Box<dyn std::error::Error>> {
        resolve_server_url(self.server_url.as_deref())
    }
}

/// Explicit flag/env first, then the saved config, then the built-in default
/// (`FALLBACK_SERVER_URL`), so `obsink login` works right after install.sh.
fn resolve_server_url(explicit: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    let saved = load_config().ok().map(|config| config.server_url);
    Ok(pick_server_url(explicit, saved))
}

fn pick_server_url(explicit: Option<&str>, saved: Option<String>) -> String {
    if let Some(url) = explicit.map(str::trim).filter(|url| !url.is_empty()) {
        return normalize_server_url(url);
    }
    if let Some(url) = saved {
        return url;
    }
    normalize_server_url(FALLBACK_SERVER_URL)
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Sign in to a server with an emailed one-time code, then set or enter
    /// the account passphrase (`OBSINK_PASSPHRASE` skips the prompt).
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
    /// Enter the account passphrase on a machine that holds a session but not
    /// the account key (`OBSINK_PASSPHRASE` skips the prompt).
    Unlock {
        #[arg(long, env = "OBSINK_SERVER_URL")]
        server_url: Option<String>,
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
    /// The account's devices; rename or sign one out from here.
    Devices {
        #[arg(long, env = "OBSINK_SERVER_URL")]
        server_url: Option<String>,
        /// Rename a device: `--rename <id> <new name>`.
        #[arg(long, num_args = 2, value_names = ["ID", "NAME"])]
        rename: Option<Vec<String>>,
        /// Sign a device out for good (its folders stay where they are).
        #[arg(long, value_name = "ID")]
        revoke: Option<String>,
    },
    /// Change the account passphrase (the same key, rewrapped).
    Passphrase {
        #[arg(long, env = "OBSINK_SERVER_URL")]
        server_url: Option<String>,
    },
    /// Every vault of the account, with its state on this machine.
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
    },
    /// Put an existing vault of the account into this directory.
    #[command(alias = "connect")]
    Download {
        #[command(flatten)]
        server: ServerArgs,
        #[arg(long)]
        vault_id: String,
        #[arg(short, long, default_value = ".")]
        directory: PathBuf,
    },
    /// Rename the configured vault for every device.
    Rename {
        #[arg(long)]
        name: String,
    },
    /// The archived versions of a file in the configured vault.
    History {
        path: String,
    },
    /// Files deleted from the configured vault in the last 30 days.
    Trash,
    /// Put an archived version (`--version <name>` from `history`) or the
    /// newest trashed copy of a file back into the folder; the next sync
    /// uploads it.
    Restore {
        path: String,
        #[arg(long, value_name = "NAME")]
        version: Option<String>,
    },
    Status {
        #[arg(short, long)]
        directory: Option<PathBuf>,
    },
    Sync,
    /// Keep the vault in sync: watch the folder, poll the server, run a
    /// cycle whenever either side changes. Conflicts are reported and left
    /// for `obsink sync`. Ctrl-C stops.
    Watch,
}

/// On-disk config. The bearer (session token) is NOT here — it lives in the
/// keychain under `bearer:<server_url>`, next to the device id and the
/// account key. The `server_url` alias reads configs written before the
/// server pivot.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CliConfig {
    #[serde(alias = "server_url")]
    server_url: String,
    vault_id: String,
    local_path: String,
    /// Extra ignore patterns for this vault (`ignore = ["drafts/", "*.tmp"]`),
    /// on top of the built-in defaults.
    #[serde(default)]
    ignore: Vec<String>,
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
        } => run_login(email, code, server_url, device_name, invite_code).await?,
        Commands::Logout { server_url } => {
            let url = resolve_server_url(server_url.as_deref())?;
            let account = bearer_account(&url);
            match load_bearer(&url) {
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
        Commands::Whoami { server_url } => run_whoami(server_url).await?,
        Commands::Devices {
            server_url,
            rename,
            revoke,
        } => run_devices(server_url, rename, revoke).await?,
        Commands::Passphrase { server_url } => run_passphrase(server_url).await?,
        Commands::Unlock { server_url } => {
            let url = resolve_server_url(server_url.as_deref())?;
            let (token, user_id) = signed_in(&url).await?;
            unlock_account(&AuthClient::new(&url), &token, &user_id).await?;
        }
        Commands::Invite { server_url, list } => {
            let url = resolve_server_url(server_url.as_deref())?;
            let token = load_bearer(&url)
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
        Commands::Vaults { server } => run_vaults(server).await?,
        Commands::Init {
            server,
            vault_name,
            directory,
        } => run_init(server, vault_name, directory).await?,
        Commands::Download {
            server,
            vault_id,
            directory,
        } => run_download(server, vault_id, directory).await?,
        Commands::Rename { name } => {
            let config = load_config()?;
            ApiClient::new(to_vault_config(&config)?)
                .rename_vault(name.trim())
                .await?;
            println!("renamed vault {} to {}", config.vault_id, name.trim());
        }
        Commands::History { path } => run_history(path).await?,
        Commands::Trash => run_trash().await?,
        Commands::Restore { path, version } => run_restore(path, version).await?,
        Commands::Status { directory } => run_status(directory).await?,
        Commands::Sync => {
            let config = load_config()?;
            let key = load_key_from_keychain(&config.vault_id)?;
            run_sync_for_config(&config, &key).await?;
        }
        Commands::Watch => {
            let config = load_config()?;
            let key = load_key_from_keychain(&config.vault_id)?;
            run_watch(&config, &key).await?;
        }
    }

    Ok(())
}

/// `obsink login`: an emailed code (or a dev code the server returns
/// inline) becomes the session bearer for the server.
async fn run_login(
    email: Option<String>,
    code: Option<String>,
    server_url: Option<String>,
    device_name: Option<String>,
    invite_code: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = resolve_server_url(server_url.as_deref())?;
    let auth = AuthClient::new(&url);
    let caps = auth.capabilities().await?;
    caps.check_protocol()?;
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
    let device = Device {
        id: load_or_create_device_id(&url)?,
        name: device_name.unwrap_or_else(default_device_name),
        platform: DevicePlatform::Cli,
    };
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
    save_secret(&user_account(&url), &session.user.id)?;
    // The URL is the normalized one (an alias of an old host reads as the
    // current host here), so the user sees which server holds the session.
    println!(
        "Signed in as {} on {url}.",
        session
            .user
            .email
            .clone()
            .unwrap_or(session.user.id.clone())
    );
    unlock_account(&auth, &session.token, &session.user.id).await?;
    Ok(())
}

/// Spec §12.1: set the passphrase on a new account (create-only; a lost race
/// unlocks the winner's key instead) or enter it on an existing one, and keep
/// the account key in the keychain.
async fn unlock_account(
    auth: &AuthClient,
    token: &str,
    user_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let outcome = match auth.get_keys(token).await? {
        None => {
            let passphrase = read_new_passphrase()?;
            let (key, material) = create_account_key(&passphrase, user_id)?;
            match auth.set_keys(token, &material).await? {
                SetKeysOutcome::Created { key_id } => {
                    save_account_key(user_id, &key, &key_id)?;
                    println!("Passphrase set. There is no recovery if it is lost.");
                    return Ok(());
                }
                SetKeysOutcome::Exists(blob) => {
                    println!("A passphrase was already set on another device. Enter it.");
                    Some(blob)
                }
            }
        }
        Some(blob) => Some(blob),
    };
    let blob = outcome.expect("an existing blob");
    if let Ok((_, key_id)) = load_account_key(user_id) {
        if key_id == blob.key_id {
            println!("Unlocked (key already on this machine).");
            return Ok(());
        }
        // A key from a lost first-set race: not the account's.
        delete_account_key(user_id);
    }
    let passphrase = read_passphrase("Passphrase: ")?;
    let key = blob
        .unlock(&passphrase, user_id)
        .map_err(|_| "Passphrase does not match this account.")?;
    save_account_key(user_id, &key, &blob.key_id)?;
    println!("Unlocked.");
    Ok(())
}

/// The passphrase from `OBSINK_PASSPHRASE` or the prompt.
fn read_passphrase(prompt: &str) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(value) = std::env::var(PASSPHRASE_ENV)
        .ok()
        .filter(|value| !value.is_empty())
    {
        return Ok(value);
    }
    Ok(prompt_password(prompt)?)
}

/// A new passphrase: at least 12 characters, entered twice unless it comes
/// from the environment.
fn read_new_passphrase() -> Result<String, Box<dyn std::error::Error>> {
    let from_env = std::env::var(PASSPHRASE_ENV)
        .ok()
        .filter(|value| !value.is_empty());
    let passphrase = match from_env {
        Some(value) => value,
        None => {
            println!("Set the account passphrase. It unlocks every vault on every device.");
            let first = prompt_password("Passphrase: ")?;
            let second = prompt_password("Again: ")?;
            if first != second {
                return Err("the passphrases do not match".into());
            }
            first
        }
    };
    if passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(
            format!("the passphrase needs at least {MIN_PASSPHRASE_CHARS} characters").into(),
        );
    }
    Ok(passphrase)
}

/// `obsink passphrase`: rewrap the account key under a new passphrase.
async fn run_passphrase(server_url: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let url = resolve_server_url(server_url.as_deref())?;
    let (token, user_id) = signed_in(&url).await?;
    let (key, _) = load_account_key(&user_id)
        .map_err(|_| "this machine holds no account key; run `obsink login`")?;
    let current = read_passphrase("Current passphrase: ")?;
    let auth = AuthClient::new(&url);
    let blob = auth
        .get_keys(&token)
        .await?
        .ok_or("the account has no passphrase yet; run `obsink login`")?;
    if blob
        .unlock(&current, &user_id)
        .map_err(|_| "Passphrase does not match this account.")?
        != key
    {
        return Err("Passphrase does not match this account.".into());
    }
    std::env::remove_var(PASSPHRASE_ENV);
    let next = read_new_passphrase()?;
    let material = rewrap_account_key(&key, &next, &user_id)?;
    auth.rewrap_keys(&token, &material).await?;
    println!("Passphrase changed.");
    Ok(())
}

/// The bearer and user id of the signed-in account for a server. A session
/// seeded without a sign-in (the harnesses' `OBSINK_BEARER`) has no user id
/// recorded yet; it is asked from the server once and kept.
async fn signed_in(url: &str) -> Result<(String, String), Box<dyn std::error::Error>> {
    let token =
        load_bearer(url).map_err(|_| format!("not signed in to {url}; run `obsink login`"))?;
    if let Some(user_id) = load_secret_opt(&user_account(url))? {
        return Ok((token, user_id));
    }
    let me = AuthClient::new(url).me(&token).await?;
    let user = me
        .user
        .ok_or_else(|| format!("no account behind the session for {url}; run `obsink login`"))?;
    save_secret(&user_account(url), &user.id)?;
    Ok((token, user.id))
}

/// The unlocked account key for a server, or a pointer at `obsink login`.
async fn account_key_for(
    url: &str,
) -> Result<(String, String, KeyBytes), Box<dyn std::error::Error>> {
    let (token, user_id) = signed_in(url).await?;
    let (key, _) = load_account_key(&user_id)
        .map_err(|_| "this machine holds no account key; run `obsink login`")?;
    Ok((token, user_id, key))
}

/// `obsink whoami`: the account, its devices and its usage.
async fn run_whoami(server_url: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let url = resolve_server_url(server_url.as_deref())?;
    let token =
        load_bearer(&url).map_err(|_| format!("not signed in to {url}; run `obsink login`"))?;
    let me = AuthClient::new(&url).me(&token).await?;
    println!("server: {url}");
    if let Some(user) = me.user {
        println!("account: {} ({})", user.email.unwrap_or_default(), user.id);
    }
    for device in me.devices {
        println!(
            "  device: {} [{}] {}{}",
            device.name,
            device.platform,
            device.id,
            if device.current { " (this device)" } else { "" }
        );
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
    Ok(())
}

/// `obsink vaults`: every vault of the account and whether this directory's
/// config holds it (the CLI is configured per directory).
async fn run_vaults(server: ServerArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (url, bearer) = resolve_server(&server)?;
    let device_id = load_or_create_device_id(&url).ok();
    let client = ApiClient::new(VaultConfig {
        server_url: url,
        bearer,
        vault_id: String::new(),
        local_path: String::new(),
        device_id: device_id.clone(),
        ignore: Vec::new(),
    });
    let configured = load_config().ok().map(|config| config.vault_id);
    for vault in client.list_vaults().await? {
        let here = if configured.as_deref() == Some(vault.id.as_str()) {
            "configured here"
        } else if device_id
            .as_deref()
            .is_some_and(|id| vault.devices.iter().any(|device| device.id == id))
        {
            "on this device"
        } else {
            "not on this device"
        };
        println!(
            "{} {} ({here}; revision {}, {} bytes, {} device(s))",
            vault.id,
            vault.name,
            vault.revision,
            vault.bytes,
            vault.devices.len()
        );
    }
    Ok(())
}

/// `obsink init`: a fresh vault key wrapped under the account key, the vault
/// on the server, this device attached, the first sync.
async fn run_init(
    server: ServerArgs,
    vault_name: String,
    directory: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = server.url()?;
    let (bearer, _, account_key) = account_key_for(&url).await?;
    let directory = resolve_vault_dir(&directory)?;
    let device_id = load_or_create_device_id(&url)?;
    let vault_key = new_key();
    // The wrap's AAD is the vault id, so the id is minted here (spec §4.3).
    let vault_id = new_vault_id();
    let client = ApiClient::new(VaultConfig {
        server_url: url.clone(),
        bearer: bearer.clone(),
        vault_id: vault_id.clone(),
        local_path: directory.display().to_string(),
        device_id: Some(device_id.clone()),
        ignore: Vec::new(),
    });
    let response = client
        .create_vault(&CreateVaultRequest {
            id: Some(vault_id.clone()),
            name: vault_name,
            max_file_size: 50 * 1024 * 1024,
            wrapped_key: Some(encode_base64(&wrap_vault_key(
                &account_key,
                &vault_key,
                &vault_id,
            )?)),
        })
        .await?;
    let vault_id = response.vault.id;
    save_secret(&vault_id, &hex::encode(vault_key))?;
    let config = CliConfig {
        server_url: url,
        vault_id,
        local_path: directory.display().to_string(),
        ignore: Vec::new(),
    };
    save_config(&config)?;
    ApiClient::new(to_vault_config(&config)?)
        .attach_device(None)
        .await?;
    run_sync_for_config(&config, &vault_key).await?;

    println!("created vault {}", config.vault_id);
    println!("config: {}", config_path()?.display());
    Ok(())
}

/// `obsink download`: an existing vault of the account into this directory.
async fn run_download(
    server: ServerArgs,
    vault_id: String,
    directory: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = server.url()?;
    let (bearer, _, account_key) = account_key_for(&url).await?;
    let directory = resolve_vault_dir(&directory)?;
    let device_id = load_or_create_device_id(&url)?;
    let client = ApiClient::new(VaultConfig {
        server_url: url.clone(),
        bearer,
        vault_id: vault_id.clone(),
        local_path: directory.display().to_string(),
        device_id: Some(device_id),
        ignore: Vec::new(),
    });
    let vault = client
        .list_vaults()
        .await?
        .into_iter()
        .find(|vault| vault.id == vault_id)
        .ok_or_else(|| format!("vault {vault_id} is not one of this account's"))?;
    let wrapped = vault
        .wrapped_key
        .ok_or("this vault has no key for the account (it was created before the passphrase)")?;
    let vault_key = unwrap_vault_key(&account_key, &decode_base64(&wrapped)?, &vault_id)
        .map_err(|_| "the vault key does not unwrap with this account key; sign in again")?;
    save_secret(&vault_id, &hex::encode(vault_key))?;

    let config = CliConfig {
        server_url: url,
        vault_id,
        local_path: directory.display().to_string(),
        ignore: Vec::new(),
    };
    save_config(&config)?;
    client.attach_device(None).await?;
    run_sync_for_config(&config, &vault_key).await?;

    println!("downloaded vault {} ({})", config.vault_id, vault.name);
    println!("config: {}", config_path()?.display());
    Ok(())
}

/// `obsink devices`: list, rename, or revoke.
async fn run_devices(
    server_url: Option<String>,
    rename: Option<Vec<String>>,
    revoke: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = resolve_server_url(server_url.as_deref())?;
    let (token, _) = signed_in(&url).await?;
    let auth = AuthClient::new(&url);
    if let Some(args) = rename {
        auth.rename_device(&token, &args[0], &args[1]).await?;
        println!("renamed device {} to {}", args[0], args[1]);
    }
    if let Some(id) = revoke {
        auth.revoke_device(&token, &id).await?;
        println!("signed out device {id}; its folders stay where they are");
    }
    for device in auth.me(&token).await?.devices {
        println!(
            "{} [{}] {}{} last seen {} vaults {}",
            device.id,
            device.platform,
            device.name,
            if device.current { " (this device)" } else { "" },
            device.last_seen,
            device.vault_ids.len()
        );
    }
    Ok(())
}

/// `obsink history <path>`: the archived versions of one file.
async fn run_history(path: String) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config()?;
    let keys = derive_keys(&load_key_from_keychain(&config.vault_id)?);
    let client = ApiClient::new(to_vault_config(&config)?);
    let versions = client.list_versions(&path, &keys).await?;
    if versions.is_empty() {
        println!("no earlier versions kept for {path}");
    }
    for version in versions {
        println!("{} ts {} size {}", version.name, version.ts, version.size);
    }
    Ok(())
}

/// `obsink trash`: the vault's recently deleted files.
async fn run_trash() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config()?;
    let keys = derive_keys(&load_key_from_keychain(&config.vault_id)?);
    let client = ApiClient::new(to_vault_config(&config)?);
    let entries = client.list_trash(&keys).await?;
    if entries.is_empty() {
        println!("nothing deleted in the last 30 days");
    }
    for entry in entries {
        println!(
            "{} deleted {} size {}",
            entry.path, entry.deleted_at, entry.size
        );
    }
    Ok(())
}

/// `obsink restore <path> [--version <name>]`: write the version (or the
/// newest trashed copy) into the folder. The next sync uploads it through the
/// conflict-gated PUT (spec §8.2, §9.3).
async fn run_restore(
    path: String,
    version: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config()?;
    let keys = derive_keys(&load_key_from_keychain(&config.vault_id)?);
    let client = ApiClient::new(to_vault_config(&config)?);
    let bytes = match &version {
        Some(name) => client.get_version(&path, name, &keys).await?,
        None => client.get_trash(&path, &keys).await?,
    };
    let target = Path::new(&config.local_path).join(&path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    write_atomic(&target, &bytes)?;
    println!(
        "restored {path} ({} bytes) from {}; run `obsink sync` to upload it",
        bytes.len(),
        version.as_deref().unwrap_or("the trash")
    );
    Ok(())
}

/// `obsink status`: what the next sync would do, without transferring.
async fn run_status(directory: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let stored = load_config()?;
    let keys = derive_keys(&load_key_from_keychain(&stored.vault_id)?);
    let directory = match directory {
        Some(directory) => resolve_vault_dir(&directory)?,
        None => PathBuf::from(&stored.local_path),
    };
    let vault_config = to_vault_config(&stored)?;
    let ignore = vault_config.ignore_rules();
    let local = load_local_state(&directory, &keys, &ignore)?;
    let live = local.working.values().filter(|entry| !entry.deleted);
    let total_size: u64 = live.clone().map(|entry| entry.size).sum();

    println!("directory: {}", directory.display());
    println!("files: {}", live.count());
    println!("bytes: {total_size}");

    let remote = fetch_remote_manifest(&ApiClient::new(vault_config), &directory, &keys).await?;
    let diff = diff_local_and_remote(&local.base, &local.working, &remote, &ignore);
    println!("upload: {}", diff.upload.len());
    println!("download: {}", diff.download.len());
    println!("conflicts: {}", diff.conflicts.len());
    Ok(())
}

/// One line per failed transfer, tagged `FATAL` or `skipped`.
fn print_failures(failures: &[SyncFailure], mut out: impl FnMut(String)) {
    for failure in failures {
        let tag = if failure.fatal { "FATAL" } else { "skipped" };
        out(format!("  [{tag}] {}: {}", failure.path, failure.error));
    }
}

/// `obsink watch`: the daemon with the stderr progress sink, one line per
/// event on stdout. Stops on Ctrl-C after the cycle in flight finishes.
async fn run_watch(config: &CliConfig, key: &KeyBytes) -> Result<(), Box<dyn std::error::Error>> {
    let vault_config = to_vault_config(config)?;
    let (handle, commands) = daemon_channel();
    let (events_tx, mut events) = tokio::sync::mpsc::channel(32);
    let daemon = tokio::spawn(run_daemon(
        vault_config,
        *key,
        DaemonOptions::default(),
        commands,
        events_tx,
        Arc::new(CliProgress),
    ));
    let stop_handle = handle.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("stopping after the current cycle");
            stop_handle.stop();
        }
    });
    while let Some(event) = events.recv().await {
        match event {
            DaemonEvent::Started => println!("watching {}", config.local_path),
            DaemonEvent::SyncStarted => println!("sync started"),
            DaemonEvent::SyncFinished(result) => {
                println!(
                    "sync finished: {} uploaded, {} downloaded, {} failed",
                    result.upload.len(),
                    result.download.len(),
                    result.failures.len()
                );
                print_failures(&result.failures, |line| println!("{line}"));
                if let Some(error) = &result.checkpoint_error {
                    println!("  checkpoint failed: {error}");
                }
            }
            DaemonEvent::ConflictsPending { plan } => {
                println!(
                    "{} conflict(s) waiting; run `obsink sync` to resolve:",
                    plan.conflicts.len()
                );
                for conflict in &plan.conflicts {
                    println!("  {}", conflict.path);
                }
            }
            DaemonEvent::Failed {
                message,
                fatal,
                retry_in,
            } => match retry_in {
                Some(wait) => println!("error: {message} (retrying in {}s)", wait.as_secs()),
                None => println!("{}: {message}", if fatal { "error" } else { "skipped" }),
            },
            DaemonEvent::Stopped => {
                println!("stopped");
                break;
            }
        }
    }
    daemon.await??;
    Ok(())
}

/// Work out the server URL and bearer for a server-facing command: the
/// session token `login` stored for the URL.
fn resolve_server(server: &ServerArgs) -> Result<(String, String), Box<dyn std::error::Error>> {
    let url = server.url()?;
    match load_bearer(&url) {
        Ok(bearer) => Ok((url, bearer)),
        Err(_) => {
            Err(format!("no credential for {url}: run `obsink login --server-url {url}`").into())
        }
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
            eprintln!("{} file(s) failed this sync:", result.failures.len());
            print_failures(&result.failures, |line| eprintln!("{line}"));
            if result.failures.iter().any(|failure| failure.fatal) {
                eprintln!("a fatal error stopped the sync early; re-run `obsink sync` to resume");
            }
        }
        if let Some(error) = &result.checkpoint_error {
            eprintln!("checkpoint failed: {error}; run `obsink sync` again");
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

fn to_vault_config(config: &CliConfig) -> Result<VaultConfig, Box<dyn std::error::Error>> {
    let bearer = load_secret(&bearer_account(&config.server_url)).map_err(|_| {
        format!(
            "no credential for {}: run `obsink login`",
            config.server_url
        )
    })?;
    Ok(VaultConfig {
        server_url: config.server_url.clone(),
        bearer,
        vault_id: config.vault_id.clone(),
        local_path: config.local_path.clone(),
        device_id: load_or_create_device_id(&config.server_url).ok(),
        ignore: config.ignore.clone(),
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

    use super::{pick_server_url, resolve_vault_dir, FALLBACK_SERVER_URL};

    #[test]
    fn the_server_url_is_the_flag_then_the_config_then_the_default() {
        assert_eq!(
            pick_server_url(
                Some(" HTTPS://Own.example/ "),
                Some("https://saved.example".into())
            ),
            "https://own.example"
        );
        assert_eq!(
            pick_server_url(Some("  "), Some("https://saved.example".into())),
            "https://saved.example"
        );
        assert_eq!(
            pick_server_url(None, None),
            FALLBACK_SERVER_URL.trim_end_matches('/')
        );
        assert!(FALLBACK_SERVER_URL.starts_with("http"));
    }

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
