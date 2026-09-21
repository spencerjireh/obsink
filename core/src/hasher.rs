use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

use thiserror::Error;
use walkdir::WalkDir;

use crate::crypto::{content_hmac, CryptoKeys, KeyBytes};
use crate::hash_cache::{HashCache, Stat};
use crate::ignore::IgnoreRules;
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

/// Keyed content hash (HMAC-SHA256, hex) of a file's contents.
pub fn hash_file(mac_key: &KeyBytes, path: &Path) -> Result<String, HasherError> {
    let bytes = fs::read(path)?;
    Ok(content_hmac(mac_key, &bytes))
}

/// Build a manifest of the directory, keyed by real path. Entry hashes are
/// keyed HMACs; `enc_path` is left empty because the local manifest is already
/// keyed by the real path (the server populates `enc_path` on upload).
///
/// Every file is read and hashed. Callers that walk repeatedly use
/// [`build_manifest_with_cache`].
pub fn build_manifest_from_dir(root: &Path, keys: &CryptoKeys) -> Result<Manifest, HasherError> {
    build_manifest_with_cache(
        root,
        keys,
        &mut HashCache::empty(keys),
        &IgnoreRules::defaults(),
    )
}

/// [`build_manifest_from_dir`] with a `(mtime, size)` memo and a vault's
/// ignore rules: a file whose stat pair matches the cache keeps its memoized
/// hash without being read, and an ignored path is not walked at all. The
/// cache is updated in place and pruned to the paths the walk saw; the
/// caller decides whether to persist it.
pub fn build_manifest_with_cache(
    root: &Path,
    keys: &CryptoKeys,
    cache: &mut HashCache,
    ignore: &IgnoreRules,
) -> Result<Manifest, HasherError> {
    let mut manifest = Manifest::new();
    let mut seen = BTreeSet::new();

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

        // `.obsink/`, atomic-write temp files, workspace state, OS noise.
        if ignore.is_ignored(&relative_key) {
            continue;
        }

        let metadata = entry.metadata()?;
        let stat = Stat::try_from(&metadata).map_err(|_| HasherError::InvalidModifiedTime {
            path: path.to_path_buf(),
        })?;

        let hash = match cache.lookup(&relative_key, &stat) {
            Some(hash) => hash.to_string(),
            None => {
                let hash = hash_file(&keys.content_mac, path)?;
                cache.insert(&relative_key, stat, hash.clone());
                hash
            }
        };

        manifest.insert(
            relative_key.clone(),
            FileEntry {
                hash,
                modified: stat.mtime_secs,
                size: stat.size,
                deleted: false,
                enc_path: String::new(),
            },
        );
        seen.insert(relative_key);
    }

    cache.retain_paths(&seen);
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{build_manifest_from_dir, build_manifest_with_cache};
    use crate::{
        crypto::{content_hmac as hash_bytes, derive_key, derive_keys, CryptoKeys},
        hash_cache::{hash_cache_path, HashCache},
        ignore::IgnoreRules,
    };

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
    fn ignores_obsink_temp_files() {
        let keys = test_keys();
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("note.md"), b"note").unwrap();
        fs::write(
            dir.path()
                .join(format!(".note.md.1-2{}", crate::fs_util::TEMP_SUFFIX)),
            b"partial",
        )
        .unwrap();

        let manifest = build_manifest_from_dir(dir.path(), &keys).unwrap();

        assert_eq!(manifest.len(), 1);
        assert!(manifest.contains_key("note.md"));
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

    #[test]
    fn a_warm_cache_yields_the_cold_manifest() {
        let keys = test_keys();
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "alpha").unwrap();
        fs::create_dir_all(dir.path().join("notes")).unwrap();
        fs::write(dir.path().join("notes/b.md"), "beta").unwrap();

        let cold = build_manifest_from_dir(dir.path(), &keys).unwrap();
        let mut cache = HashCache::empty(&keys);
        let first =
            build_manifest_with_cache(dir.path(), &keys, &mut cache, &IgnoreRules::defaults())
                .unwrap();
        assert_eq!(cache.len(), 2);
        let warm =
            build_manifest_with_cache(dir.path(), &keys, &mut cache, &IgnoreRules::defaults())
                .unwrap();
        assert_eq!(cold, first);
        assert_eq!(cold, warm);
    }

    /// The bet the cache makes: same length, same nanosecond mtime, same
    /// hash. Rewriting the bytes and restoring the mtime proves the file was
    /// not read; touching it proves the file is.
    #[test]
    fn a_matching_stat_pair_serves_the_memoized_hash() {
        let keys = test_keys();
        let dir = tempdir().unwrap();
        let note = dir.path().join("note.md");
        fs::write(&note, "aaaa").unwrap();
        let original_mtime = fs::metadata(&note).unwrap().modified().unwrap();

        let mut cache = HashCache::empty(&keys);
        let before =
            build_manifest_with_cache(dir.path(), &keys, &mut cache, &IgnoreRules::defaults())
                .unwrap();
        assert_eq!(
            before["note.md"].hash,
            hash_bytes(&keys.content_mac, b"aaaa")
        );

        fs::write(&note, "bbbb").unwrap();
        fs::File::options()
            .write(true)
            .open(&note)
            .unwrap()
            .set_modified(original_mtime)
            .unwrap();
        let stale =
            build_manifest_with_cache(dir.path(), &keys, &mut cache, &IgnoreRules::defaults())
                .unwrap();
        assert_eq!(
            stale["note.md"].hash, before["note.md"].hash,
            "served from the memo"
        );

        let later = original_mtime + std::time::Duration::from_secs(5);
        fs::File::options()
            .write(true)
            .open(&note)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let fresh =
            build_manifest_with_cache(dir.path(), &keys, &mut cache, &IgnoreRules::defaults())
                .unwrap();
        assert_eq!(
            fresh["note.md"].hash,
            hash_bytes(&keys.content_mac, b"bbbb")
        );
    }

    #[test]
    fn a_changed_file_is_rehashed_and_a_deleted_one_pruned() {
        let keys = test_keys();
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "one").unwrap();
        fs::write(dir.path().join("b.md"), "two").unwrap();
        let mut cache = HashCache::empty(&keys);
        build_manifest_with_cache(dir.path(), &keys, &mut cache, &IgnoreRules::defaults()).unwrap();

        fs::write(dir.path().join("a.md"), "one more byte").unwrap();
        fs::remove_file(dir.path().join("b.md")).unwrap();
        let manifest =
            build_manifest_with_cache(dir.path(), &keys, &mut cache, &IgnoreRules::defaults())
                .unwrap();
        assert_eq!(
            manifest["a.md"].hash,
            hash_bytes(&keys.content_mac, b"one more byte")
        );
        assert!(!manifest.contains_key("b.md"));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn the_cache_file_lives_under_obsink_and_stays_out_of_the_manifest() {
        let keys = test_keys();
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "one").unwrap();
        let mut cache = HashCache::empty(&keys);
        build_manifest_with_cache(dir.path(), &keys, &mut cache, &IgnoreRules::defaults()).unwrap();
        cache.save(dir.path()).unwrap();
        assert!(hash_cache_path(dir.path()).starts_with(dir.path().join(".obsink")));

        let manifest = build_manifest_from_dir(dir.path(), &keys).unwrap();
        assert_eq!(manifest.keys().collect::<Vec<_>>(), vec!["a.md"]);
    }

    /// `cargo test -p obsink-core timing_3000_files -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn timing_3000_files() {
        let keys = test_keys();
        let dir = tempdir().unwrap();
        for i in 0..3000 {
            let folder = dir.path().join(format!("folder{}", i % 30));
            fs::create_dir_all(&folder).unwrap();
            fs::write(
                folder.join(format!("note{i}.md")),
                "x".repeat(2000 + i % 500),
            )
            .unwrap();
        }
        for i in 0..40 {
            fs::write(
                dir.path().join(format!("attachment{i}.bin")),
                vec![7u8; 2 * 1024 * 1024],
            )
            .unwrap();
        }

        let mut cache = HashCache::empty(&keys);
        let started = std::time::Instant::now();
        build_manifest_with_cache(dir.path(), &keys, &mut cache, &IgnoreRules::defaults()).unwrap();
        let cold = started.elapsed();
        let started = std::time::Instant::now();
        build_manifest_with_cache(dir.path(), &keys, &mut cache, &IgnoreRules::defaults()).unwrap();
        let warm = started.elapsed();
        fs::write(dir.path().join("folder0/note0.md"), "edited").unwrap();
        let started = std::time::Instant::now();
        build_manifest_with_cache(dir.path(), &keys, &mut cache, &IgnoreRules::defaults()).unwrap();
        let one_edit = started.elapsed();
        println!("cold {cold:?}  warm {warm:?}  one edit {one_edit:?}");
        assert!(warm < cold);
    }
}
