//! Secret storage for the CLI and desktop app (feature `keychain`), spec §6.3.
//!
//! Four kinds of entry share the `obsink` service: the vault key (account =
//! vault ID, hex), the server bearer (`bearer:<url>`), the account key
//! (`account:<user id>`, hex plus the server's `key_id`) and the device id
//! (`device:<url>`, shared by the CLI and the desktop app so one Mac is one
//! device). On macOS they live in the login keychain as generic-password
//! items, written through the Security framework so the secret never appears
//! on a process argv. `OBSINK_KEYRING_DIR` swaps the keychain for a directory
//! of 0600 files (CI, harnesses).

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::{
    crypto::{new_key, KeyBytes},
    server_url::{legacy_server_urls, normalize_server_url},
};

pub const KEYCHAIN_SERVICE: &str = "obsink";

/// Harnesses fix the device id per side instead of reading the keychain.
pub const DEVICE_ID_ENV: &str = "OBSINK_DEVICE_ID";

fn keyring_dir() -> Option<PathBuf> {
    std::env::var_os("OBSINK_KEYRING_DIR").map(PathBuf::from)
}

fn keyring_file(dir: &Path, account: &str) -> PathBuf {
    // Accounts contain `:` and `/` (bearer URLs); keep filenames flat.
    dir.join(account.replace(['/', ':'], "_"))
}

fn write_private(path: &Path, value: &str) -> io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // An existing file keeps its old mode through `open`; make it private too.
    file.set_permissions(<fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600))?;
    file.write_all(value.as_bytes())
}

pub fn save_secret(account: &str, value: &str) -> io::Result<()> {
    if let Some(dir) = keyring_dir() {
        fs::create_dir_all(&dir)?;
        fs::set_permissions(
            &dir,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )?;
        return write_private(&keyring_file(&dir, account), value);
    }
    native::save(account, value)
}

pub fn load_secret(account: &str) -> io::Result<String> {
    if let Some(dir) = keyring_dir() {
        return Ok(fs::read_to_string(keyring_file(&dir, account))?
            .trim()
            .to_string());
    }
    native::load(account)
}

/// `Ok(None)` when there is no such entry; other errors propagate.
pub fn load_secret_opt(account: &str) -> io::Result<Option<String>> {
    match load_secret(account) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn delete_secret(account: &str) {
    if let Some(dir) = keyring_dir() {
        let _ = fs::remove_file(keyring_file(&dir, account));
        return;
    }
    native::delete(account);
}

/// Keychain account for a server's bearer: `bearer:<canonical url>`.
pub fn bearer_account(server_url: &str) -> String {
    format!("bearer:{}", normalize_server_url(server_url))
}

/// Keychain account for the signed-in user id of a server: `user:<url>`.
/// The account key is filed under the user id, so a client needs this to
/// find it without a round trip.
pub fn user_account(server_url: &str) -> String {
    format!("user:{}", normalize_server_url(server_url))
}

/// Keychain account for the account key: `account:<user id>`.
pub fn account_key_account(user_id: &str) -> String {
    format!("account:{user_id}")
}

/// Keychain account for this machine's device id: `device:<canonical url>`.
pub fn device_id_account(server_url: &str) -> String {
    format!("device:{}", normalize_server_url(server_url))
}

/// Store the unlocked account key with the server's `key_id`, so a later
/// `GET /auth/keys` can tell whether this entry is the account's key.
pub fn save_account_key(user_id: &str, key: &KeyBytes, key_id: &str) -> io::Result<()> {
    save_secret(
        &account_key_account(user_id),
        &format!("{}:{key_id}", hex::encode(key)),
    )
}

/// The stored account key and its `key_id`, or `NotFound`.
pub fn load_account_key(user_id: &str) -> io::Result<(KeyBytes, String)> {
    let value = load_secret(&account_key_account(user_id))?;
    let (hex_key, key_id) = value.split_once(':').ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "account key entry is malformed")
    })?;
    let bytes =
        hex::decode(hex_key).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let key: KeyBytes = bytes
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "account key is not 32 bytes"))?;
    Ok((key, key_id.to_string()))
}

pub fn delete_account_key(user_id: &str) {
    delete_secret(&account_key_account(user_id));
}

/// This machine's device id for a server: `OBSINK_DEVICE_ID` when set, else
/// the keychain entry, else a fresh id that is saved. Any keychain error other
/// than "no entry" is propagated rather than papered over with a new id, so a
/// denied keychain prompt never turns one machine into two devices.
pub fn load_or_create_device_id(server_url: &str) -> io::Result<String> {
    if let Some(fixed) = std::env::var(DEVICE_ID_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(fixed);
    }
    let account = device_id_account(server_url);
    match load_secret(&account) {
        Ok(id) if !id.is_empty() => return Ok(id),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let id = new_device_id();
    save_secret(&account, &id)?;
    Ok(id)
}

/// `dev_<32 hex>`: within the server's `[A-Za-z0-9_-]{1,64}`.
pub fn new_device_id() -> String {
    format!("dev_{}", hex::encode(&new_key()[..16]))
}

/// The bearer for a server. A bearer an older build stored under a legacy
/// host (`LEGACY_SERVER_ALIASES`) is moved to the canonical entry the first
/// time it is read, so the host move needs no sign-in.
pub fn load_bearer(server_url: &str) -> io::Result<String> {
    let canonical = normalize_server_url(server_url);
    let account = format!("bearer:{canonical}");
    let missing = match load_secret(&account) {
        Ok(token) => return Ok(token),
        Err(error) => error,
    };
    for legacy in legacy_server_urls(&canonical) {
        let legacy_account = format!("bearer:{legacy}");
        if let Ok(token) = load_secret(&legacy_account) {
            save_secret(&account, &token)?;
            delete_secret(&legacy_account);
            return Ok(token);
        }
    }
    Err(missing)
}

#[cfg(target_os = "macos")]
mod native {
    use std::io;

    use security_framework::passwords;

    use super::KEYCHAIN_SERVICE;

    fn to_io(error: security_framework::base::Error) -> io::Error {
        io::Error::other(error.to_string())
    }

    pub fn save(account: &str, value: &str) -> io::Result<()> {
        // Replaces an existing item with the same service/account.
        passwords::set_generic_password(KEYCHAIN_SERVICE, account, value.as_bytes()).map_err(to_io)
    }

    /// `errSecItemNotFound`; every other code (a denied prompt, a locked
    /// keychain) is a real error.
    const ITEM_NOT_FOUND: i32 = -25300;

    pub fn load(account: &str) -> io::Result<String> {
        let bytes =
            passwords::get_generic_password(KEYCHAIN_SERVICE, account).map_err(|error| {
                if error.code() == ITEM_NOT_FOUND {
                    io::Error::new(io::ErrorKind::NotFound, error.to_string())
                } else {
                    to_io(error)
                }
            })?;
        String::from_utf8(bytes)
            .map(|value| value.trim().to_string())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub fn delete(account: &str) {
        let _ = passwords::delete_generic_password(KEYCHAIN_SERVICE, account);
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    use std::io;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "no native keychain on this platform; set OBSINK_KEYRING_DIR",
        )
    }

    pub fn save(_account: &str, _value: &str) -> io::Result<()> {
        Err(unsupported())
    }

    pub fn load(_account: &str) -> io::Result<String> {
        Err(unsupported())
    }

    pub fn delete(_account: &str) {}
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::{
        account_key_account, bearer_account, delete_account_key, delete_secret, device_id_account,
        keyring_file, load_account_key, load_bearer, load_or_create_device_id, load_secret,
        save_account_key, save_secret, write_private, DEVICE_ID_ENV,
    };

    #[test]
    fn file_fallback_is_private() {
        let dir = tempdir().unwrap();
        let path = keyring_file(dir.path(), "bearer:https://example.com");

        write_private(&path, "secret").unwrap();
        write_private(&path, "secret2").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "secret2");
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            "bearer_https___example.com"
        );
    }

    // The file keyring under a private directory, so the test never touches
    // the login keychain. `OBSINK_KEYRING_DIR` is process-wide; this is the
    // only core test that sets it.
    #[test]
    fn a_legacy_bearer_moves_to_the_canonical_entry() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("OBSINK_KEYRING_DIR", dir.path());

        save_secret("bearer:https://obsink.spencerjireh.com", "os_old").unwrap();
        assert_eq!(
            bearer_account("https://obsink.spencerjireh.com/"),
            "bearer:https://obsink-api.spencerjireh.com"
        );
        assert_eq!(
            load_bearer("https://obsink-api.spencerjireh.com").unwrap(),
            "os_old"
        );
        // Moved: the canonical entry exists, the legacy one is gone.
        assert_eq!(
            load_secret("bearer:https://obsink-api.spencerjireh.com").unwrap(),
            "os_old"
        );
        assert!(load_secret("bearer:https://obsink.spencerjireh.com").is_err());
        assert!(load_bearer("https://elsewhere.test").is_err());

        // The account key round-trips with its key id, filed under the user.
        let key = [7u8; 32];
        save_account_key("usr_1", &key, "key_1").unwrap();
        assert_eq!(account_key_account("usr_1"), "account:usr_1");
        assert_eq!(
            load_account_key("usr_1").unwrap(),
            (key, "key_1".to_string())
        );
        assert_eq!(
            load_account_key("usr_2").unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        delete_account_key("usr_1");
        assert!(load_account_key("usr_1").is_err());

        // The device id is minted once and then reused; the env var overrides it.
        let first = load_or_create_device_id("https://elsewhere.test/").unwrap();
        assert!(first.starts_with("dev_") && first.len() == 36);
        assert_eq!(
            load_or_create_device_id("https://elsewhere.test").unwrap(),
            first
        );
        assert_eq!(
            load_secret(&device_id_account("https://elsewhere.test")).unwrap(),
            first
        );
        std::env::set_var(DEVICE_ID_ENV, "harness-a");
        assert_eq!(
            load_or_create_device_id("https://elsewhere.test").unwrap(),
            "harness-a"
        );
        std::env::remove_var(DEVICE_ID_ENV);
        delete_secret(&device_id_account("https://elsewhere.test"));

        delete_secret("bearer:https://obsink-api.spencerjireh.com");
        std::env::remove_var("OBSINK_KEYRING_DIR");
    }
}
