//! Retention: prune old versions and trash (spec §8.1, §9.2), drop expired
//! sessions and stale one-time codes, and sweep blob directories whose vault
//! row is gone. Runs at startup and then on an interval.

use std::{fs, io, path::Path, time::Duration};

use sqlx::Row;

use crate::{
    blobs::{extract_timestamp, remove_dir_if_present, BlobStore, Tier},
    db,
    error::ApiError,
    AppState,
};

pub const MAX_VERSIONS_PER_FILE: usize = 10;
pub const VERSION_RETENTION_SECS: u64 = 14 * 24 * 60 * 60;
pub const TRASH_RETENTION_SECS: u64 = 30 * 24 * 60 * 60;
pub const EMAIL_CODE_RETENTION_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub versions_removed: usize,
    pub trash_removed: usize,
    pub sessions_removed: u64,
    pub codes_removed: u64,
    pub orphan_dirs_removed: usize,
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "versions={} trash={} sessions={} codes={} orphan_dirs={}",
            self.versions_removed,
            self.trash_removed,
            self.sessions_removed,
            self.codes_removed,
            self.orphan_dirs_removed
        )
    }
}

/// Every history directory `<tier>/<vault>/<hash>/` with its entries.
fn history_dirs(store: &BlobStore, tier: Tier) -> io::Result<Vec<std::path::PathBuf>> {
    let mut dirs = Vec::new();
    for vault in store.vault_dirs(tier)? {
        let vault_dir = store.tier_root(tier).join(vault);
        for entry in fs::read_dir(&vault_dir)? {
            let entry = entry?;
            if entry.path().is_dir() {
                dirs.push(entry.path());
            }
        }
    }
    Ok(dirs)
}

fn entries_newest_first(dir: &Path) -> io::Result<Vec<(u64, std::path::PathBuf)>> {
    let mut entries: Vec<(u64, std::path::PathBuf)> = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| {
            (
                extract_timestamp(&entry.file_name().to_string_lossy()),
                entry.path(),
            )
        })
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(entries)
}

fn remove_empty_parents(dir: &Path, stop_at: &Path) {
    let mut current = Some(dir.to_path_buf());
    while let Some(path) = current {
        if path == stop_at || !path.starts_with(stop_at) {
            break;
        }
        if fs::remove_dir(&path).is_err() {
            break;
        }
        current = path.parent().map(Path::to_path_buf);
    }
}

pub fn prune_versions(store: &BlobStore, now: u64) -> io::Result<usize> {
    let mut removed = 0;
    for dir in history_dirs(store, Tier::Versions)? {
        for (index, (ts, path)) in entries_newest_first(&dir)?.into_iter().enumerate() {
            if index >= MAX_VERSIONS_PER_FILE || now.saturating_sub(ts) > VERSION_RETENTION_SECS {
                fs::remove_file(&path)?;
                removed += 1;
            }
        }
        remove_empty_parents(&dir, &store.tier_root(Tier::Versions));
    }
    Ok(removed)
}

pub fn prune_trash(store: &BlobStore, now: u64) -> io::Result<usize> {
    let mut removed = 0;
    for dir in history_dirs(store, Tier::Trash)? {
        for (ts, path) in entries_newest_first(&dir)? {
            if now.saturating_sub(ts) > TRASH_RETENTION_SECS {
                fs::remove_file(&path)?;
                removed += 1;
            }
        }
        remove_empty_parents(&dir, &store.tier_root(Tier::Trash));
    }
    Ok(removed)
}

/// Blob directories for vault ids with no row (crash between a vault delete
/// commit and the directory removal).
pub fn sweep_orphans(store: &BlobStore, live_ids: &[String]) -> io::Result<usize> {
    let mut removed = 0;
    for tier in [Tier::Live, Tier::Versions, Tier::Trash] {
        for vault in store.vault_dirs(tier)? {
            if !live_ids.iter().any(|id| *id == vault) {
                remove_dir_if_present(&store.tier_root(tier).join(&vault))?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

pub async fn run_once(state: &AppState, now: u64) -> Result<Report, ApiError> {
    let mut report = Report::default();
    let store = state.blobs.clone();
    let (versions, trash) = tokio::task::spawn_blocking(move || -> io::Result<(usize, usize)> {
        Ok((prune_versions(&store, now)?, prune_trash(&store, now)?))
    })
    .await
    .map_err(ApiError::internal)??;
    report.versions_removed = versions;
    report.trash_removed = trash;

    report.sessions_removed = sqlx::query("DELETE FROM sessions WHERE expires <= $1")
        .bind(db::to_i64(now))
        .execute(&state.pool)
        .await?
        .rows_affected();
    report.codes_removed = sqlx::query("DELETE FROM email_codes WHERE last_sent < $1")
        .bind(db::to_i64(now.saturating_sub(EMAIL_CODE_RETENTION_SECS)))
        .execute(&state.pool)
        .await?
        .rows_affected();

    let ids: Vec<String> = sqlx::query("SELECT id FROM vaults")
        .fetch_all(&state.pool)
        .await?
        .into_iter()
        .map(|row| row.get("id"))
        .collect();
    let store = state.blobs.clone();
    report.orphan_dirs_removed = tokio::task::spawn_blocking(move || sweep_orphans(&store, &ids))
        .await
        .map_err(ApiError::internal)??;
    Ok(report)
}

/// Background loop: once at startup, then every `interval`.
pub fn spawn(state: AppState) {
    let interval = Duration::from_secs(state.config.retention_interval_secs.max(60));
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            match run_once(&state, db::now()).await {
                Ok(report) => tracing::info!(%report, "retention pass complete"),
                Err(error) => tracing::warn!(?error, "retention pass failed"),
            }
        }
    });
}
