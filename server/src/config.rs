//! Process configuration from environment variables, plus the server key.
//!
//! Every value has a default except `DATABASE_URL`. Tests build a [`Config`]
//! directly instead of going through the environment.

use std::{fs, io, path::PathBuf};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rand::RngCore;

pub const DEFAULT_MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
pub const DEFAULT_MAX_VAULT_BYTES: u64 = 1024 * 1024 * 1024;
pub const DEFAULT_MAX_VAULTS_PER_USER: u32 = 10;
pub const DEFAULT_MAX_BATCH_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmtpTls {
    StartTls,
    Tls,
    None,
}

#[derive(Debug, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: String,
    pub tls: SmtpTls,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: String,
    pub database_url: String,
    pub data_dir: PathBuf,
    /// Raw `OBSINK_SERVER_KEY` (base64). `None` = read or create `<data>/server.key`.
    pub server_key: Option<String>,
    /// Accepted `aud` values for Apple identity tokens. Empty disables Apple.
    pub apple_client_ids: Vec<String>,
    pub apple_jwks_url: String,
    pub smtp: Option<SmtpConfig>,
    /// Return the one-time code in the `/auth/email/start` response (dev only).
    pub dev_return_code: bool,
    pub max_vaults_per_user: u32,
    pub max_vault_bytes: u64,
    pub max_file_bytes: u64,
    pub max_batch_bytes: u64,
    pub retention_interval_secs: u64,
    pub migrate_on_start: bool,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let database_url = env("DATABASE_URL").ok_or("DATABASE_URL is required")?;
        let smtp = env("SMTP_HOST").map(|host| {
            Ok::<_, String>(SmtpConfig {
                host,
                port: parse_or("SMTP_PORT", 587)?,
                username: env("SMTP_USERNAME"),
                password: env("SMTP_PASSWORD"),
                from: env("SMTP_FROM").unwrap_or_else(|| "ObSink <no-reply@localhost>".to_string()),
                tls: match env("SMTP_TLS").as_deref().unwrap_or("starttls") {
                    "starttls" => SmtpTls::StartTls,
                    "tls" => SmtpTls::Tls,
                    "none" => SmtpTls::None,
                    other => {
                        return Err(format!(
                            "SMTP_TLS must be starttls, tls, or none (got {other})"
                        ))
                    }
                },
            })
        });
        let smtp = match smtp {
            Some(result) => Some(result?),
            None => None,
        };
        Ok(Self {
            listen: env("OBSINK_LISTEN").unwrap_or_else(|| "0.0.0.0:8080".to_string()),
            database_url,
            data_dir: PathBuf::from(env("OBSINK_DATA_DIR").unwrap_or_else(|| "/data".to_string())),
            server_key: env("OBSINK_SERVER_KEY"),
            apple_client_ids: std::env::var("APPLE_CLIENT_IDS")
                .map(|value| split_csv(&value))
                .unwrap_or_else(|_| vec!["com.obsink.ios".to_string()]),
            apple_jwks_url: env("APPLE_JWKS_URL")
                .unwrap_or_else(|| "https://appleid.apple.com/auth/keys".to_string()),
            smtp,
            dev_return_code: env("AUTH_DEV_RETURN_CODE").as_deref() == Some("1"),
            max_vaults_per_user: parse_or("MAX_VAULTS_PER_USER", DEFAULT_MAX_VAULTS_PER_USER)?,
            max_vault_bytes: parse_or("MAX_VAULT_BYTES", DEFAULT_MAX_VAULT_BYTES)?,
            max_file_bytes: parse_or("MAX_FILE_BYTES", DEFAULT_MAX_FILE_BYTES)?,
            max_batch_bytes: parse_or("MAX_BATCH_BYTES", DEFAULT_MAX_BATCH_BYTES)?,
            retention_interval_secs: parse_or("RETENTION_INTERVAL_SECS", 86_400)?,
            migrate_on_start: env("OBSINK_MIGRATE_ON_START").as_deref() != Some("0"),
        })
    }

    /// Email sign-in is offered when mail can go out, or when the dev flag
    /// returns the code inline.
    pub fn email_enabled(&self) -> bool {
        self.smtp.is_some() || self.dev_return_code
    }

    pub fn apple_enabled(&self) -> bool {
        !self.apple_client_ids.is_empty()
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn parse_or<T: std::str::FromStr>(key: &str, default: T) -> Result<T, String> {
    match env(key) {
        Some(value) => value
            .parse()
            .map_err(|_| format!("{key} is not a valid number: {value}")),
        None => Ok(default),
    }
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// The 32-byte master key for envelope encryption: `OBSINK_SERVER_KEY` if
/// set, else `<data>/server.key`, else a fresh key written there (mode 0600)
/// with a warning to back it up.
pub fn load_or_create_server_key(config: &Config) -> Result<[u8; 32], String> {
    if let Some(encoded) = &config.server_key {
        return decode_key(encoded.trim()).map_err(|error| format!("OBSINK_SERVER_KEY: {error}"));
    }
    let path = config.data_dir.join("server.key");
    match fs::read_to_string(&path) {
        Ok(contents) => {
            decode_key(contents.trim()).map_err(|error| format!("{}: {error}", path.display()))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let key = generate_key();
            fs::create_dir_all(&config.data_dir)
                .map_err(|error| format!("{}: {error}", config.data_dir.display()))?;
            write_private(&path, &BASE64.encode(key))
                .map_err(|error| format!("{}: {error}", path.display()))?;
            tracing::warn!(
                path = %path.display(),
                "OBSINK_SERVER_KEY is unset; generated a server key on disk — back it up, metadata is unreadable without it"
            );
            Ok(key)
        }
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

pub fn generate_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    key
}

pub fn encode_key(key: &[u8; 32]) -> String {
    BASE64.encode(key)
}

fn decode_key(encoded: &str) -> Result<[u8; 32], String> {
    let bytes = BASE64
        .decode(encoded)
        .map_err(|_| "expected 32 bytes encoded as base64".to_string())?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| format!("expected 32 bytes, got {}", bytes.len()))
}

fn write_private(path: &std::path::Path, contents: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(contents.as_bytes())?;
        file.write_all(b"\n")
    }
    #[cfg(not(unix))]
    {
        fs::write(path, format!("{contents}\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(dir: &std::path::Path) -> Config {
        Config {
            listen: String::new(),
            database_url: String::new(),
            data_dir: dir.to_path_buf(),
            server_key: None,
            apple_client_ids: Vec::new(),
            apple_jwks_url: String::new(),
            smtp: None,
            dev_return_code: false,
            max_vaults_per_user: 1,
            max_vault_bytes: 1,
            max_file_bytes: 1,
            max_batch_bytes: 1,
            retention_interval_secs: 1,
            migrate_on_start: false,
        }
    }

    #[test]
    fn server_key_is_generated_and_persisted_when_unset() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(&dir.path().join("data"));
        let first = load_or_create_server_key(&cfg).unwrap();
        let second = load_or_create_server_key(&cfg).unwrap();
        assert_eq!(first, second);
        assert!(dir.path().join("data/server.key").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join("data/server.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn env_key_wins_over_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        let key = generate_key();
        cfg.server_key = Some(encode_key(&key));
        assert_eq!(load_or_create_server_key(&cfg).unwrap(), key);
        assert!(!dir.path().join("server.key").exists());
    }

    #[test]
    fn rejects_short_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.server_key = Some(BASE64.encode([1u8; 16]));
        assert!(load_or_create_server_key(&cfg)
            .unwrap_err()
            .contains("32 bytes"));
    }
}
