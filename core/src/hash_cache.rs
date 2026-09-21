//! Memo of content hashes keyed by a file's `(mtime, size)`, so the working
//! manifest of an unchanged vault costs a stat per file instead of a read
//! and an HMAC per file. Lives in `.obsink/hash-cache.json`, which the walker
//! already skips.
//!
//! The cache is fingerprinted by the vault's `content_mac` key: a vault that
//! is re-keyed gets a fresh cache rather than hashes that no other device
//! can verify. A missing, corrupt or foreign cache is simply empty; a cache
//! write failure is the caller's to log and never fails a sync.
//!
//! The bet, shared with git and Syncthing: a file rewritten with the same
//! length and the same nanosecond mtime is served its old hash. Asserted by
//! `a_matching_stat_pair_serves_the_memoized_hash` so it stays a decision.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};

use crate::{
    crypto::{content_hmac, CryptoKeys},
    fs_util::write_atomic,
};

const HASH_CACHE_FILE: &str = ".obsink/hash-cache.json";

/// What the memo is keyed by: the file's mtime at nanosecond resolution and
/// its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stat {
    pub mtime_secs: u64,
    pub mtime_nanos: u32,
    pub size: u64,
}

impl TryFrom<&fs::Metadata> for Stat {
    type Error = io::Error;

    fn try_from(metadata: &fs::Metadata) -> Result<Self, io::Error> {
        let mtime = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(Stat {
            mtime_secs: mtime.as_secs(),
            mtime_nanos: mtime.subsec_nanos(),
            size: metadata.len(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CacheEntry {
    stat: Stat,
    hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashCache {
    /// HMAC of a fixed label under the vault's `content_mac` key; a cache
    /// with another `key_id` belongs to another key and is discarded.
    key_id: String,
    entries: BTreeMap<String, CacheEntry>,
}

pub fn hash_cache_path(local_root: &Path) -> PathBuf {
    local_root.join(HASH_CACHE_FILE)
}

fn key_id(keys: &CryptoKeys) -> String {
    content_hmac(&keys.content_mac, b"obsink:hash-cache:v1")
}

impl HashCache {
    /// An empty cache for these keys.
    pub fn empty(keys: &CryptoKeys) -> Self {
        HashCache {
            key_id: key_id(keys),
            entries: BTreeMap::new(),
        }
    }

    /// The cache on disk, or an empty one when it is missing, unreadable,
    /// corrupt or written under another key.
    pub fn load(local_root: &Path, keys: &CryptoKeys) -> Self {
        let expected = key_id(keys);
        fs::read(hash_cache_path(local_root))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<HashCache>(&bytes).ok())
            .filter(|cache| cache.key_id == expected)
            .unwrap_or_else(|| HashCache::empty(keys))
    }

    pub fn save(&self, local_root: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec(self)?;
        write_atomic(&hash_cache_path(local_root), &bytes)
    }

    /// The memoized hash when the file's stat pair still matches.
    pub fn lookup(&self, path: &str, stat: &Stat) -> Option<&str> {
        self.entries
            .get(path)
            .filter(|entry| entry.stat == *stat)
            .map(|entry| entry.hash.as_str())
    }

    pub fn insert(&mut self, path: &str, stat: Stat, hash: String) {
        self.entries
            .insert(path.to_string(), CacheEntry { stat, hash });
    }

    /// Drop entries for paths the walk no longer saw.
    pub fn retain_paths(&mut self, seen: &BTreeSet<String>) {
        self.entries.retain(|path, _| seen.contains(path));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::derive_keys;

    fn keys(master: u8) -> CryptoKeys {
        derive_keys(&[master; 32])
    }

    fn stat(secs: u64, nanos: u32, size: u64) -> Stat {
        Stat {
            mtime_secs: secs,
            mtime_nanos: nanos,
            size,
        }
    }

    #[test]
    fn lookup_needs_the_exact_stat_pair() {
        let mut cache = HashCache::empty(&keys(1));
        cache.insert("a.md", stat(10, 5, 3), "h".into());
        assert_eq!(cache.lookup("a.md", &stat(10, 5, 3)), Some("h"));
        assert_eq!(cache.lookup("a.md", &stat(10, 6, 3)), None);
        assert_eq!(cache.lookup("a.md", &stat(10, 5, 4)), None);
        assert_eq!(cache.lookup("b.md", &stat(10, 5, 3)), None);
    }

    #[test]
    fn round_trips_through_disk_and_is_scoped_to_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = HashCache::empty(&keys(1));
        cache.insert("a.md", stat(10, 5, 3), "h".into());
        cache.save(dir.path()).unwrap();
        assert!(hash_cache_path(dir.path()).exists());

        assert_eq!(HashCache::load(dir.path(), &keys(1)), cache);
        assert!(HashCache::load(dir.path(), &keys(2)).is_empty());
    }

    #[test]
    fn a_missing_or_corrupt_file_is_an_empty_cache() {
        let dir = tempfile::tempdir().unwrap();
        assert!(HashCache::load(dir.path(), &keys(1)).is_empty());
        fs::create_dir_all(dir.path().join(".obsink")).unwrap();
        fs::write(hash_cache_path(dir.path()), b"{garbage").unwrap();
        assert!(HashCache::load(dir.path(), &keys(1)).is_empty());
    }

    #[test]
    fn retain_paths_prunes_deleted_files() {
        let mut cache = HashCache::empty(&keys(1));
        cache.insert("a.md", stat(1, 0, 1), "a".into());
        cache.insert("b.md", stat(1, 0, 1), "b".into());
        cache.retain_paths(&BTreeSet::from(["b.md".to_string()]));
        assert_eq!(cache.len(), 1);
        assert!(cache.lookup("b.md", &stat(1, 0, 1)).is_some());
    }
}
