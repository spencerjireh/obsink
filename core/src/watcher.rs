//! Filesystem change events for the daemon: a recursive `notify` watcher
//! over the vault root that hands vault-relative paths to a channel, minus
//! everything the ignore rules cover (`.obsink/` in particular, or the
//! sync's own checkpoint writes would trigger the sync that wrote them).

use std::path::{Path, PathBuf};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::ignore::IgnoreRules;

#[derive(Debug, Error)]
pub enum WatcherError {
    #[error("filesystem watcher error: {0}")]
    Notify(#[from] notify::Error),
}

/// Start watching `root`. Events arrive on `tx` as batches of
/// vault-relative `/`-separated paths; the watcher stops when the returned
/// handle is dropped. The callback runs on notify's own thread and never
/// needs a runtime.
pub fn spawn_watcher(
    root: PathBuf,
    ignore: IgnoreRules,
    tx: mpsc::UnboundedSender<Vec<String>>,
) -> Result<RecommendedWatcher, WatcherError> {
    // FSEvents reports canonical paths (`/private/var/...` for `/var/...`),
    // so the prefix to strip is the canonical root; the non-canonical
    // spelling is kept as a fallback for watchers that echo the given path.
    let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
    let watched = root.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        let Ok(event) = event else { return };
        let paths: Vec<String> = event
            .paths
            .iter()
            .filter_map(|path| {
                relative_key(&canonical, path).or_else(|| relative_key(&watched, path))
            })
            .filter(|key| !ignore.is_ignored(key))
            .collect();
        if !paths.is_empty() {
            // A closed receiver means the daemon is gone; nothing to do.
            let _ = tx.send(paths);
        }
    })?;
    watcher.watch(&root, RecursiveMode::Recursive)?;
    Ok(watcher)
}

/// `path` relative to `root`, `/`-separated; `None` for the root itself or
/// anything outside it.
pub fn relative_key(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let key = relative.to_string_lossy().replace('\\', "/");
    (!key.is_empty()).then_some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_keys_are_vault_relative_and_slash_separated() {
        let root = Path::new("/v");
        assert_eq!(
            relative_key(root, Path::new("/v/notes/a.md")).as_deref(),
            Some("notes/a.md")
        );
        assert_eq!(relative_key(root, Path::new("/v")), None);
        assert_eq!(relative_key(root, Path::new("/elsewhere/a.md")), None);
    }

    #[tokio::test]
    async fn a_write_reaches_the_channel_and_ignored_paths_do_not() {
        // The non-canonical temp path (`/var/...` on macOS) checks that
        // events reported under `/private/var/...` still map.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".obsink")).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _watcher = spawn_watcher(root.clone(), IgnoreRules::defaults(), tx).unwrap();
        // FSEvents needs a moment before it reports on a fresh watch.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        std::fs::write(root.join(".obsink/manifest.json"), "{}").unwrap();
        std::fs::write(root.join("note.md"), "hello").unwrap();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen = Vec::new();
        while tokio::time::Instant::now() < deadline && !seen.contains(&"note.md".to_string()) {
            if let Ok(Some(paths)) =
                tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await
            {
                seen.extend(paths);
            }
        }
        assert!(seen.contains(&"note.md".to_string()), "saw {seen:?}");
        assert!(
            !seen.iter().any(|path| path.starts_with(".obsink")),
            "saw {seen:?}"
        );
    }
}
