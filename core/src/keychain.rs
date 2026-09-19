//! Secret storage for the CLI and desktop app (feature `keychain`).
//!
//! Two kinds of secret share the `obsink` service: the derived vault key
//! (account = vault ID, hex) and the server bearer (account = `bearer:<url>`).
//! On macOS they live in the login keychain as generic-password items, written
//! through the Security framework so the secret never appears on a process
//! argv. `OBSINK_KEYRING_DIR` swaps the keychain for a directory of 0600 files
//! (CI, harnesses).

use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub const KEYCHAIN_SERVICE: &str = "obsink";

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

pub fn delete_secret(account: &str) {
    if let Some(dir) = keyring_dir() {
        let _ = fs::remove_file(keyring_file(&dir, account));
        return;
    }
    native::delete(account);
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

    pub fn load(account: &str) -> io::Result<String> {
        let bytes = passwords::get_generic_password(KEYCHAIN_SERVICE, account)
            .map_err(|error| io::Error::new(io::ErrorKind::NotFound, error.to_string()))?;
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

    use super::{keyring_file, write_private};

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
}
