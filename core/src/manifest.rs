use std::collections::BTreeSet;

use crate::types::{Conflict, FileEntry, Manifest, SyncAction, SyncActionKind, SyncResult};

pub type ManifestDiff = SyncResult;

pub fn diff_manifests(local: &Manifest, remote: &Manifest) -> ManifestDiff {
    let mut result = ManifestDiff::default();
    let paths = local
        .keys()
        .chain(remote.keys())
        .cloned()
        .collect::<BTreeSet<_>>();

    for path in paths {
        let local_entry = local.get(&path);
        let remote_entry = remote.get(&path);

        match (local_entry, remote_entry) {
            (Some(local_entry), Some(remote_entry)) => {
                classify_existing(&path, local_entry, remote_entry, &mut result);
            }
            (Some(local_entry), None) if !local_entry.deleted => {
                result.upload.push(action(
                    &path,
                    SyncActionKind::Upload,
                    Some(local_entry.clone()),
                    None,
                ));
            }
            (None, Some(remote_entry)) if !remote_entry.deleted => {
                result.download.push(action(
                    &path,
                    SyncActionKind::Download,
                    None,
                    Some(remote_entry.clone()),
                ));
            }
            _ => {}
        }
    }

    result
}

fn classify_existing(path: &str, local: &FileEntry, remote: &FileEntry, result: &mut ManifestDiff) {
    if local.hash == remote.hash && local.deleted == remote.deleted {
        return;
    }

    match (local.deleted, remote.deleted) {
        (true, false) => push_by_newer(
            path,
            local,
            remote,
            SyncActionKind::DeleteRemote,
            SyncActionKind::Download,
            result,
        ),
        (false, true) => push_by_newer(
            path,
            local,
            remote,
            SyncActionKind::Upload,
            SyncActionKind::DeleteLocal,
            result,
        ),
        (true, true) => {}
        (false, false) => push_by_newer(
            path,
            local,
            remote,
            SyncActionKind::Upload,
            SyncActionKind::Download,
            result,
        ),
    }
}

fn push_by_newer(
    path: &str,
    local: &FileEntry,
    remote: &FileEntry,
    local_kind: SyncActionKind,
    remote_kind: SyncActionKind,
    result: &mut ManifestDiff,
) {
    if local.modified > remote.modified {
        result.upload.push(action(
            path,
            local_kind,
            Some(local.clone()),
            Some(remote.clone()),
        ));
    } else if remote.modified > local.modified {
        result.download.push(action(
            path,
            remote_kind,
            Some(local.clone()),
            Some(remote.clone()),
        ));
    } else {
        result.conflicts.push(Conflict {
            path: path.to_string(),
            local: local.clone(),
            remote: remote.clone(),
        });
    }
}

fn action(
    path: &str,
    kind: SyncActionKind,
    local: Option<FileEntry>,
    remote: Option<FileEntry>,
) -> SyncAction {
    SyncAction {
        path: path.to_string(),
        kind,
        local,
        remote,
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        diff_manifests,
        types::{FileEntry, Manifest, SyncActionKind},
    };

    fn entry(hash: &str, modified: u64, deleted: bool) -> FileEntry {
        FileEntry {
            hash: hash.to_string(),
            modified,
            size: 10,
            deleted,
        }
    }

    fn manifest(entries: &[(&str, FileEntry)]) -> Manifest {
        entries
            .iter()
            .map(|(path, entry)| ((*path).to_string(), entry.clone()))
            .collect()
    }

    #[test]
    fn detects_new_local_file() {
        let local = manifest(&[("note.md", entry("a", 2, false))]);
        let remote = Manifest::new();

        let diff = diff_manifests(&local, &remote);

        assert_eq!(diff.upload.len(), 1);
        assert_eq!(diff.upload[0].kind, SyncActionKind::Upload);
    }

    #[test]
    fn detects_new_remote_file() {
        let local = Manifest::new();
        let remote = manifest(&[("note.md", entry("a", 2, false))]);

        let diff = diff_manifests(&local, &remote);

        assert_eq!(diff.download.len(), 1);
        assert_eq!(diff.download[0].kind, SyncActionKind::Download);
    }

    #[test]
    fn prefers_newer_local_change() {
        let local = manifest(&[("note.md", entry("local", 3, false))]);
        let remote = manifest(&[("note.md", entry("remote", 2, false))]);

        let diff = diff_manifests(&local, &remote);

        assert_eq!(diff.upload.len(), 1);
        assert_eq!(diff.conflicts.len(), 0);
    }

    #[test]
    fn prefers_newer_remote_deletion() {
        let local = manifest(&[("note.md", entry("local", 1, false))]);
        let remote = manifest(&[("note.md", entry("local", 2, true))]);

        let diff = diff_manifests(&local, &remote);

        assert_eq!(diff.download.len(), 1);
        assert_eq!(diff.download[0].kind, SyncActionKind::DeleteLocal);
    }

    #[test]
    fn reports_conflict_on_same_timestamp_different_content() {
        let local = manifest(&[("note.md", entry("local", 2, false))]);
        let remote = manifest(&[("note.md", entry("remote", 2, false))]);

        let diff = diff_manifests(&local, &remote);

        assert_eq!(diff.conflicts.len(), 1);
        assert_eq!(diff.upload.len(), 0);
        assert_eq!(diff.download.len(), 0);
    }

    #[test]
    fn ignores_identical_entries() {
        let local = manifest(&[("note.md", entry("same", 2, false))]);
        let remote = manifest(&[("note.md", entry("same", 2, false))]);

        let diff = diff_manifests(&local, &remote);

        assert!(diff.upload.is_empty());
        assert!(diff.download.is_empty());
        assert!(diff.conflicts.is_empty());
    }
}
