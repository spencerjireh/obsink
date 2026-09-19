//! Filesystem blob store.
//!
//! Layout under `<data>/blobs`:
//!
//! ```text
//! live/<vault_id>/<ab>/<sha256(path)>            current sealed blob
//! _versions/<vault_id>/<sha256(path)>/<unix>[-n]  previous versions (spec §8)
//! _trash/<vault_id>/<sha256(path)>/<unix>[-n]     soft-deleted blobs (spec §9)
//! ```
//!
//! File names are hashes of the client's path token, so nothing the client
//! sends can escape the store or collide a file with a directory. Vault ids
//! are validated before any path is built.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use rand::RngCore;

use crate::crypto::sha256;

pub const MAX_PATH_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Live,
    Versions,
    Trash,
}

impl Tier {
    fn dir(self) -> &'static str {
        match self {
            Tier::Live => "live",
            Tier::Versions => "_versions",
            Tier::Trash => "_trash",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BlobStore {
    root: PathBuf,
}

pub fn valid_vault_id(id: &str) -> bool {
    match id.strip_prefix("vault_") {
        Some(rest) => rest.len() == 36 && rest.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'),
        None => false,
    }
}

pub fn valid_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= MAX_PATH_BYTES && !path.bytes().any(|b| b == 0)
}

fn path_name(path: &str) -> String {
    hex::encode(sha256(path.as_bytes()))
}

impl BlobStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("blobs"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn tier_root(&self, tier: Tier) -> PathBuf {
        self.root.join(tier.dir())
    }

    fn vault_dir(&self, tier: Tier, vault_id: &str) -> PathBuf {
        debug_assert!(valid_vault_id(vault_id));
        self.tier_root(tier).join(vault_id)
    }

    fn live_path(&self, vault_id: &str, path: &str) -> PathBuf {
        let name = path_name(path);
        self.vault_dir(Tier::Live, vault_id)
            .join(&name[..2])
            .join(name)
    }

    fn history_dir(&self, tier: Tier, vault_id: &str, path: &str) -> PathBuf {
        self.vault_dir(tier, vault_id).join(path_name(path))
    }

    pub fn get_live(&self, vault_id: &str, path: &str) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.live_path(vault_id, path)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn live_exists(&self, vault_id: &str, path: &str) -> bool {
        self.live_path(vault_id, path).is_file()
    }

    /// Atomic write: temp file in the same directory, fsync, rename.
    pub fn put_live(&self, vault_id: &str, path: &str, sealed: &[u8]) -> io::Result<()> {
        let target = self.live_path(vault_id, path);
        let dir = target.parent().expect("live path has a parent");
        fs::create_dir_all(dir)?;
        let mut suffix = [0u8; 8];
        rand::rngs::OsRng.fill_bytes(&mut suffix);
        let tmp = dir.join(format!(".tmp-{}", hex::encode(suffix)));
        {
            use std::io::Write;
            let mut file = fs::File::create(&tmp)?;
            file.write_all(sealed)?;
            file.sync_all()?;
        }
        if let Err(error) = fs::rename(&tmp, &target) {
            let _ = fs::remove_file(&tmp);
            return Err(error);
        }
        Ok(())
    }

    /// Move the live blob into `_versions/<ts>`; no-op when there is none.
    pub fn archive_version(&self, vault_id: &str, path: &str, now: u64) -> io::Result<()> {
        self.move_live(Tier::Versions, vault_id, path, now)
    }

    /// Move the live blob into `_trash/<ts>`; no-op when there is none.
    pub fn move_to_trash(&self, vault_id: &str, path: &str, now: u64) -> io::Result<()> {
        self.move_live(Tier::Trash, vault_id, path, now)
    }

    fn move_live(&self, tier: Tier, vault_id: &str, path: &str, now: u64) -> io::Result<()> {
        let source = self.live_path(vault_id, path);
        if !source.is_file() {
            return Ok(());
        }
        let dir = self.history_dir(tier, vault_id, path);
        fs::create_dir_all(&dir)?;
        let mut target = dir.join(now.to_string());
        let mut seq = 1;
        while target.exists() {
            target = dir.join(format!("{now}-{seq}"));
            seq += 1;
        }
        fs::rename(source, target)
    }

    pub fn delete_vault(&self, vault_id: &str) -> io::Result<()> {
        for tier in [Tier::Live, Tier::Versions, Tier::Trash] {
            remove_dir_if_present(&self.vault_dir(tier, vault_id))?;
        }
        Ok(())
    }

    /// Timestamps (newest first) of the stored history entries for a path.
    pub fn list_history(&self, tier: Tier, vault_id: &str, path: &str) -> io::Result<Vec<String>> {
        let dir = self.history_dir(tier, vault_id, path);
        let mut names = match fs::read_dir(&dir) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        names.sort_by_key(|name| std::cmp::Reverse(extract_timestamp(name)));
        Ok(names)
    }

    /// Vault directories present under a tier (for orphan sweeps).
    pub fn vault_dirs(&self, tier: Tier) -> io::Result<Vec<String>> {
        match fs::read_dir(self.tier_root(tier)) {
            Ok(entries) => Ok(entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().is_dir())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }
}

pub fn remove_dir_if_present(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Leading digits of a history file name (`<unix>` or `<unix>-<n>`), 0 if none.
pub fn extract_timestamp(name: &str) -> u64 {
    let digits: String = name.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VAULT: &str = "vault_00000000-0000-4000-8000-000000000000";

    #[test]
    fn validates_ids_and_paths() {
        assert!(valid_vault_id(VAULT));
        assert!(!valid_vault_id("vault_../x"));
        assert!(!valid_vault_id(
            "other_00000000-0000-4000-8000-000000000000"
        ));
        assert!(valid_path("a/b"));
        assert!(!valid_path(""));
        assert!(!valid_path("a\0b"));
        assert!(!valid_path(&"x".repeat(MAX_PATH_BYTES + 1)));
    }

    #[test]
    fn blob_paths_never_escape_root() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path());
        let root = dir.path().join("blobs");
        for path in ["../../etc/passwd", "/abs/olute", "..", "a/../../b", "plain"] {
            store.put_live(VAULT, path, b"x").unwrap();
            let live = store.live_path(VAULT, path).canonicalize().unwrap();
            assert!(live.starts_with(root.canonicalize().unwrap()), "{path}");
            store.archive_version(VAULT, path, 5).unwrap();
            let history = store
                .history_dir(Tier::Versions, VAULT, path)
                .canonicalize()
                .unwrap();
            assert!(history.starts_with(root.canonicalize().unwrap()), "{path}");
        }
    }

    #[test]
    fn archive_then_trash_moves_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path());
        store.put_live(VAULT, "note", b"v1").unwrap();
        store.archive_version(VAULT, "note", 100).unwrap();
        assert!(store.get_live(VAULT, "note").unwrap().is_none());
        store.put_live(VAULT, "note", b"v2").unwrap();
        store.archive_version(VAULT, "note", 100).unwrap();
        assert_eq!(
            store
                .list_history(Tier::Versions, VAULT, "note")
                .unwrap()
                .len(),
            2
        );
        store.put_live(VAULT, "note", b"v3").unwrap();
        store.move_to_trash(VAULT, "note", 200).unwrap();
        assert_eq!(
            store.list_history(Tier::Trash, VAULT, "note").unwrap(),
            vec!["200"]
        );
        assert!(!store.live_exists(VAULT, "note"));
        store.move_to_trash(VAULT, "note", 201).unwrap(); // no-op
        store.delete_vault(VAULT).unwrap();
        assert!(store.vault_dirs(Tier::Live).unwrap().is_empty());
        assert!(store.vault_dirs(Tier::Trash).unwrap().is_empty());
    }

    #[test]
    fn extract_timestamp_parses_leading_digits() {
        assert_eq!(extract_timestamp("1713100800"), 1713100800);
        assert_eq!(extract_timestamp("1713100800-2"), 1713100800);
        assert_eq!(extract_timestamp("junk"), 0);
    }
}
