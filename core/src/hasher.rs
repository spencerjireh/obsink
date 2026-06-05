use std::{
    fs, io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use thiserror::Error;
use walkdir::WalkDir;

use crate::crypto::{content_hmac, CryptoKeys, KeyBytes};
use crate::types::{FileEntry, Manifest};

#[derive(Debug, Error)]
pub enum HasherError {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("walkdir error: {0}")]
    WalkDir(#[from] walkdir::Error),
    #[error("failed to strip directory prefix for {path}")]
    StripPrefix { path: PathBuf },
    #[error("system time error for {path}")]
    InvalidModifiedTime { path: PathBuf },
}

/// Keyed content hash (HMAC-SHA256) of raw bytes, hex-encoded.
pub fn hash_bytes(mac_key: &KeyBytes, bytes: &[u8]) -> String {
    content_hmac(mac_key, bytes)
}

/// Keyed content hash of a file's contents.
pub fn hash_file(mac_key: &KeyBytes, path: &Path) -> Result<String, HasherError> {
    let bytes = fs::read(path)?;
    Ok(hash_bytes(mac_key, &bytes))
}

/// Build a manifest of the directory, keyed by real path. Entry hashes are
/// keyed HMACs; `enc_path` is left empty because the local manifest is already
/// keyed by the real path (the server populates `enc_path` on upload).
pub fn build_manifest_from_dir(root: &Path, keys: &CryptoKeys) -> Result<Manifest, HasherError> {
    let mut manifest = Manifest::new();

    for entry in WalkDir::new(root) {
        let entry = entry?;
        let path = entry.path();

        if !entry.file_type().is_file() {
            continue;
        }

        let relative = path
            .strip_prefix(root)
            .map_err(|_| HasherError::StripPrefix {
                path: path.to_path_buf(),
            })?;
        let relative_key = relative.to_string_lossy().replace('\\', "/");

        if relative_key == ".obsink" || relative_key.starts_with(".obsink/") {
            continue;
        }

        let metadata = entry.metadata()?;
        let modified = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_err(|_| HasherError::InvalidModifiedTime {
                path: path.to_path_buf(),
            })?
            .as_secs();

        manifest.insert(
            relative_key,
            FileEntry {
                hash: hash_file(&keys.content_mac, path)?,
                modified,
                size: metadata.len(),
                deleted: false,
                enc_path: String::new(),
            },
        );
    }

    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{build_manifest_from_dir, hash_bytes};
    use crate::crypto::{derive_key, derive_keys, CryptoKeys};

    fn test_keys() -> CryptoKeys {
        derive_keys(&derive_key("hunter2", b"obsink-salt").unwrap())
    }

    #[test]
    fn hashing_is_deterministic() {
        let keys = test_keys();
        let first = hash_bytes(&keys.content_mac, b"obsink");
        let second = hash_bytes(&keys.content_mac, b"obsink");

        assert_eq!(first, second);
    }

    #[test]
    fn handles_empty_file() {
        let keys = test_keys();
        let dir = tempdir().unwrap();
        let file = dir.path().join("empty.md");
        fs::write(&file, []).unwrap();

        let manifest = build_manifest_from_dir(dir.path(), &keys).unwrap();
        let entry = manifest.get("empty.md").unwrap();

        assert_eq!(entry.size, 0);
        assert_eq!(entry.hash, hash_bytes(&keys.content_mac, b""));
    }

    #[test]
    fn handles_binary_file() {
        let keys = test_keys();
        let dir = tempdir().unwrap();
        let file = dir.path().join("image.bin");
        fs::write(&file, [0, 159, 146, 150, 255]).unwrap();

        let manifest = build_manifest_from_dir(dir.path(), &keys).unwrap();
        let entry = manifest.get("image.bin").unwrap();

        assert_eq!(entry.size, 5);
        assert_eq!(
            entry.hash,
            hash_bytes(&keys.content_mac, &[0, 159, 146, 150, 255])
        );
    }
}
