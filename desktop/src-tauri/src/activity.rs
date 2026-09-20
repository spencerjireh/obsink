//! Per-vault activity log: what each sync did, file by file, plus when the
//! vault last completed a sync. Written by the desktop after every cycle so
//! the popover and the Activity tab have something to show once syncs run
//! unattended. Core knows nothing about it.
//!
//! One JSON file per vault under `~/.obsink/activity/`, newest event last,
//! capped at [`MAX_EVENTS`]. A write failure is logged and never fails the
//! sync that produced it.

use std::{
    fs, io,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use dirs::home_dir;
use obsink_core::{write_atomic, SyncActionKind, SyncResult};
use serde::{Deserialize, Serialize};

pub const MAX_EVENTS: usize = 200;
const ACTIVITY_DIR: &str = ".obsink/activity";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Uploaded,
    Downloaded,
    DeletedHere,
    DeletedOnServer,
    Conflict,
    Error,
    Synced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEvent {
    /// Unix seconds.
    pub at: u64,
    pub vault_id: String,
    pub kind: ActivityKind,
    /// The file, for per-file events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The error text, or `↑n ↓n` for a `Synced` summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct VaultActivity {
    /// Newest last.
    events: Vec<ActivityEvent>,
    last_synced: Option<u64>,
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn activity_path(vault_id: &str) -> io::Result<PathBuf> {
    let home = home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory not found"))?;
    Ok(home.join(ACTIVITY_DIR).join(format!("{vault_id}.json")))
}

/// Missing or unreadable logs start empty; nothing here is worth failing for.
fn load(vault_id: &str) -> VaultActivity {
    activity_path(vault_id)
        .and_then(fs::read)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(vault_id: &str, log: &VaultActivity) -> io::Result<()> {
    let bytes = serde_json::to_vec(log)?;
    write_atomic(&activity_path(vault_id)?, &bytes)
}

fn push(log: &mut VaultActivity, event: ActivityEvent) {
    log.events.push(event);
    if log.events.len() > MAX_EVENTS {
        let excess = log.events.len() - MAX_EVENTS;
        log.events.drain(..excess);
    }
}

fn event(
    vault_id: &str,
    at: u64,
    kind: ActivityKind,
    path: Option<&str>,
    detail: Option<String>,
) -> ActivityEvent {
    ActivityEvent {
        at,
        vault_id: vault_id.to_string(),
        kind,
        path: path.map(str::to_string),
        detail,
    }
}

/// Record a completed cycle: one event per file action, conflict and
/// failure, then a `Synced` summary; `last_synced` moves to now.
pub fn record_sync(vault_id: &str, result: &SyncResult) -> io::Result<()> {
    let at = now_seconds();
    let mut log = load(vault_id);
    for action in result.upload.iter().chain(result.download.iter()) {
        let kind = match action.kind {
            SyncActionKind::Upload => ActivityKind::Uploaded,
            SyncActionKind::Download => ActivityKind::Downloaded,
            SyncActionKind::DeleteLocal => ActivityKind::DeletedHere,
            SyncActionKind::DeleteRemote => ActivityKind::DeletedOnServer,
        };
        push(
            &mut log,
            event(vault_id, at, kind, Some(&action.path), None),
        );
    }
    for conflict in &result.conflicts {
        push(
            &mut log,
            event(
                vault_id,
                at,
                ActivityKind::Conflict,
                Some(&conflict.path),
                None,
            ),
        );
    }
    for failure in &result.failures {
        let detail = if failure.fatal {
            format!("FATAL {}", failure.error)
        } else {
            failure.error.clone()
        };
        let path = (!failure.path.is_empty()).then_some(failure.path.as_str());
        push(
            &mut log,
            event(vault_id, at, ActivityKind::Error, path, Some(detail)),
        );
    }
    push(
        &mut log,
        event(
            vault_id,
            at,
            ActivityKind::Synced,
            None,
            Some(format!(
                "↑{} ↓{}",
                result.upload.len(),
                result.download.len()
            )),
        ),
    );
    log.last_synced = Some(at);
    save(vault_id, &log)
}

/// A sync or resolve that failed before producing a result.
pub fn record_error(vault_id: &str, message: &str) -> io::Result<()> {
    let mut log = load(vault_id);
    push(
        &mut log,
        event(
            vault_id,
            now_seconds(),
            ActivityKind::Error,
            None,
            Some(message.to_string()),
        ),
    );
    save(vault_id, &log)
}

#[allow(dead_code)] // read by get_vault_states in the next commit
pub fn last_synced(vault_id: &str) -> Option<u64> {
    load(vault_id).last_synced
}

/// Drop the log with the vault (best effort).
pub fn forget(vault_id: &str) {
    if let Ok(path) = activity_path(vault_id) {
        let _ = fs::remove_file(path);
    }
}

/// The newest `limit` events across the given vaults (or one of them),
/// newest first. Events from one file keep their order; across files they
/// interleave by time.
pub fn list(vault_ids: &[String], vault_id: Option<&str>, limit: usize) -> Vec<ActivityEvent> {
    let mut events: Vec<ActivityEvent> = vault_ids
        .iter()
        .filter(|id| vault_id.is_none_or(|wanted| wanted == id.as_str()))
        .flat_map(|id| {
            let mut events = load(id).events;
            events.reverse();
            events
        })
        .collect();
    // Stable, so same-second events keep their per-file order.
    events.sort_by_key(|event| std::cmp::Reverse(event.at));
    events.truncate(limit);
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use obsink_core::{Conflict, FileEntry, SyncAction, SyncFailure};
    use std::path::Path;

    fn sandbox(name: &str) -> PathBuf {
        let dir = PathBuf::from(format!(
            "/tmp/obsink-desktop-activity-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);
        dir
    }

    fn action(kind: SyncActionKind, path: &str) -> SyncAction {
        SyncAction {
            path: path.to_string(),
            kind,
            local: None,
            remote: None,
        }
    }

    #[test]
    fn a_sync_result_maps_to_per_file_events_and_a_summary() {
        let _lock = crate::TEST_ENV_LOCK.lock().unwrap();
        let dir = sandbox("map");
        let result = SyncResult {
            upload: vec![
                action(SyncActionKind::Upload, "notes/a.md"),
                action(SyncActionKind::DeleteRemote, "notes/gone.md"),
            ],
            download: vec![
                action(SyncActionKind::Download, "notes/b.md"),
                action(SyncActionKind::DeleteLocal, "notes/old.md"),
            ],
            conflicts: vec![Conflict {
                path: "notes/c.md".to_string(),
                local: FileEntry::default(),
                remote: FileEntry::default(),
            }],
            failures: vec![SyncFailure {
                path: "notes/d.md".to_string(),
                kind: SyncActionKind::Upload,
                error: "boom".to_string(),
                fatal: true,
            }],
        };
        record_sync("vault_a", &result).unwrap();

        let events = list(&["vault_a".to_string()], None, 100);
        let kinds: Vec<(ActivityKind, Option<&str>)> =
            events.iter().map(|e| (e.kind, e.path.as_deref())).collect();
        assert_eq!(
            kinds,
            vec![
                (ActivityKind::Synced, None),
                (ActivityKind::Error, Some("notes/d.md")),
                (ActivityKind::Conflict, Some("notes/c.md")),
                (ActivityKind::DeletedHere, Some("notes/old.md")),
                (ActivityKind::Downloaded, Some("notes/b.md")),
                (ActivityKind::DeletedOnServer, Some("notes/gone.md")),
                (ActivityKind::Uploaded, Some("notes/a.md")),
            ]
        );
        assert_eq!(events[0].detail.as_deref(), Some("↑2 ↓2"));
        assert_eq!(events[1].detail.as_deref(), Some("FATAL boom"));
        assert!(last_synced("vault_a").is_some());
        assert!(Path::new(&dir)
            .join(".obsink/activity/vault_a.json")
            .exists());

        forget("vault_a");
        assert!(list(&["vault_a".to_string()], None, 100).is_empty());
        assert_eq!(last_synced("vault_a"), None);
    }

    #[test]
    fn the_log_keeps_the_newest_max_events() {
        let _lock = crate::TEST_ENV_LOCK.lock().unwrap();
        sandbox("ring");
        for i in 0..(MAX_EVENTS + 25) {
            record_error("vault_b", &format!("e{i}")).unwrap();
        }
        let events = list(&["vault_b".to_string()], None, usize::MAX);
        assert_eq!(events.len(), MAX_EVENTS);
        assert_eq!(events[0].detail.as_deref(), Some("e224"));
        assert_eq!(events[MAX_EVENTS - 1].detail.as_deref(), Some("e25"));
    }

    #[test]
    fn listing_merges_vaults_and_filters_by_vault() {
        let _lock = crate::TEST_ENV_LOCK.lock().unwrap();
        sandbox("merge");
        record_error("vault_c", "c1").unwrap();
        record_error("vault_d", "d1").unwrap();
        record_error("vault_c", "c2").unwrap();
        let ids = vec!["vault_c".to_string(), "vault_d".to_string()];

        let all = list(&ids, None, 10);
        assert_eq!(all.len(), 3);
        let only_c = list(&ids, Some("vault_c"), 10);
        assert_eq!(only_c.len(), 2);
        assert_eq!(only_c[0].detail.as_deref(), Some("c2"));
        assert_eq!(list(&ids, None, 1).len(), 1);
        assert!(list(&ids, Some("vault_x"), 10).is_empty());
    }
}
