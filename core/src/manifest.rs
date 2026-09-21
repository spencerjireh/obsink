use std::collections::BTreeSet;

use crate::types::{Conflict, FileEntry, Manifest, SyncAction, SyncActionKind};

/// The three-way diff: what to upload, what to download, and what needs a
/// decision. Nothing has been transferred yet, so there are no failures.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestDiff {
    pub upload: Vec<SyncAction>,
    pub download: Vec<SyncAction>,
    pub conflicts: Vec<Conflict>,
}

/// The identity of a path's content on one side. A tombstone and an absent
/// entry are the same version: a tombstone's `hash` only exists so the next
/// write can present it as `X-Parent-Hash`, it says nothing about content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Version<'a> {
    Gone,
    Present(&'a str),
}

fn version(entry: Option<&FileEntry>) -> Version<'_> {
    match entry {
        Some(entry) if !entry.deleted => Version::Present(&entry.hash),
        _ => Version::Gone,
    }
}

/// Three-way diff against the last checkpoint. A side "changed" when its
/// version differs from `base`; only the content hash and the deleted flag
/// take part, never `modified` (the server stamps its own receipt time, so
/// mtimes are not comparable across devices).
///
/// | local changed | remote changed | outcome |
/// |---|---|---|
/// | no | no | nothing |
/// | yes | no | `Upload`, or `DeleteRemote` when local is gone |
/// | no | yes | `Download`, or `DeleteLocal` when remote is gone |
/// | yes | yes, same version | nothing (converged) |
/// | yes | yes, different | `Conflict` |
///
/// Upload/DeleteRemote actions carry the remote entry (tombstone included)
/// because its hash is the parent hash the server checks.
pub fn diff_manifests(base: &Manifest, local: &Manifest, remote: &Manifest) -> ManifestDiff {
    let mut result = ManifestDiff::default();
    let paths = base
        .keys()
        .chain(local.keys())
        .chain(remote.keys())
        .cloned()
        .collect::<BTreeSet<_>>();

    for path in paths {
        let base_entry = base.get(&path);
        let local_entry = local.get(&path);
        let remote_entry = remote.get(&path);
        let base_version = version(base_entry);
        let local_version = version(local_entry);
        let remote_version = version(remote_entry);
        let local_changed = local_version != base_version;
        let remote_changed = remote_version != base_version;

        match (local_changed, remote_changed) {
            (false, false) => {}
            (true, false) => {
                let kind = match local_version {
                    Version::Present(_) => SyncActionKind::Upload,
                    Version::Gone => SyncActionKind::DeleteRemote,
                };
                result.upload.push(action(
                    &path,
                    kind,
                    Some(local_or_tombstone(local_entry, base_entry)),
                    remote_entry.cloned(),
                ));
            }
            (false, true) => {
                let kind = match remote_version {
                    Version::Present(_) => SyncActionKind::Download,
                    Version::Gone => SyncActionKind::DeleteLocal,
                };
                result.download.push(action(
                    &path,
                    kind,
                    local_entry.cloned(),
                    Some(local_or_tombstone(remote_entry, base_entry)),
                ));
            }
            (true, true) if local_version == remote_version => {}
            (true, true) => result.conflicts.push(Conflict {
                path: path.clone(),
                local: local_or_tombstone(local_entry, base_entry),
                remote: local_or_tombstone(remote_entry, base_entry),
            }),
        }
    }

    result
}

/// The next checkpoint after a sync: the re-fetched server manifest, except
/// that every held-back path (a failed transfer or an unresolved conflict)
/// keeps its previous base entry so the next diff still sees it as pending.
pub fn checkpoint_manifest(
    previous_base: &Manifest,
    refetched: &Manifest,
    hold_back: &BTreeSet<String>,
) -> Manifest {
    let mut next = refetched.clone();
    for path in hold_back {
        match previous_base.get(path) {
            Some(entry) => {
                next.insert(path.clone(), entry.clone());
            }
            None => {
                next.remove(path);
            }
        }
    }
    next
}

/// The entry for a side, or a tombstone synthesized from `base` when the side
/// has no entry at all (the working manifest always has one; the server side
/// only lacks one when the row vanished).
fn local_or_tombstone(entry: Option<&FileEntry>, base: Option<&FileEntry>) -> FileEntry {
    match entry {
        Some(entry) => entry.clone(),
        None => FileEntry {
            hash: base.map(|entry| entry.hash.clone()).unwrap_or_default(),
            modified: base.map(|entry| entry.modified).unwrap_or(0),
            size: base.map(|entry| entry.size).unwrap_or(0),
            deleted: true,
            enc_path: base.map(|entry| entry.enc_path.clone()).unwrap_or_default(),
        },
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
    use std::collections::BTreeSet;

    use crate::{
        checkpoint_manifest, diff_manifests,
        types::{FileEntry, Manifest, SyncActionKind},
    };

    fn entry(hash: &str, modified: u64, deleted: bool) -> FileEntry {
        FileEntry {
            hash: hash.to_string(),
            modified,
            size: 10,
            deleted,
            enc_path: String::new(),
        }
    }

    fn manifest(entries: &[(&str, FileEntry)]) -> Manifest {
        entries
            .iter()
            .map(|(path, entry)| ((*path).to_string(), entry.clone()))
            .collect()
    }

    fn live(hash: &str) -> Option<FileEntry> {
        Some(entry(hash, 1, false))
    }

    fn tomb(hash: &str) -> Option<FileEntry> {
        Some(entry(hash, 1, true))
    }

    fn side(entry: &Option<FileEntry>) -> Manifest {
        match entry {
            Some(entry) => manifest(&[("note.md", entry.clone())]),
            None => Manifest::new(),
        }
    }

    type Case = (
        &'static str,
        Option<FileEntry>,
        Option<FileEntry>,
        Option<FileEntry>,
        Expect,
    );

    #[derive(Debug, PartialEq)]
    enum Expect {
        Nothing,
        Up(SyncActionKind),
        Down(SyncActionKind),
        Conflict,
    }

    fn outcome(
        base: Option<FileEntry>,
        local: Option<FileEntry>,
        remote: Option<FileEntry>,
    ) -> Expect {
        let diff = diff_manifests(&side(&base), &side(&local), &side(&remote));
        let lists = [
            !diff.upload.is_empty(),
            !diff.download.is_empty(),
            !diff.conflicts.is_empty(),
        ];
        assert!(
            lists.iter().filter(|hit| **hit).count() <= 1,
            "one path landed in more than one list: {diff:?}"
        );
        if let Some(action) = diff.upload.first() {
            Expect::Up(action.kind.clone())
        } else if let Some(action) = diff.download.first() {
            Expect::Down(action.kind.clone())
        } else if !diff.conflicts.is_empty() {
            Expect::Conflict
        } else {
            Expect::Nothing
        }
    }

    #[test]
    fn three_way_decision_table() {
        use Expect::*;
        use SyncActionKind::*;
        let table: Vec<Case> = vec![
            ("all absent", None, None, None, Nothing),
            ("first sync, identical", None, live("h"), live("h"), Nothing),
            (
                "first sync, differing",
                None,
                live("h1"),
                live("h2"),
                Conflict,
            ),
            ("new local", None, live("h"), None, Up(Upload)),
            ("new remote", None, None, live("h"), Down(Download)),
            ("unchanged", live("h1"), live("h1"), live("h1"), Nothing),
            ("local edit", live("h1"), live("h2"), live("h1"), Up(Upload)),
            (
                "remote edit",
                live("h1"),
                live("h1"),
                live("h2"),
                Down(Download),
            ),
            (
                "both edited, same",
                live("h1"),
                live("h2"),
                live("h2"),
                Nothing,
            ),
            (
                "both edited, differ",
                live("h1"),
                live("h2"),
                live("h3"),
                Conflict,
            ),
            (
                "remote tombstone, local unchanged",
                live("h1"),
                live("h1"),
                tomb("h1"),
                Down(DeleteLocal),
            ),
            (
                "local deleted, remote unchanged",
                live("h1"),
                tomb("h1"),
                live("h1"),
                Up(DeleteRemote),
            ),
            (
                "local deleted, remote edited",
                live("h1"),
                tomb("h1"),
                live("h2"),
                Conflict,
            ),
            (
                "local edited, remote deleted",
                live("h1"),
                live("h2"),
                tomb("h1"),
                Conflict,
            ),
            ("both deleted", live("h1"), tomb("h1"), tomb("h1"), Nothing),
            (
                "both gone, local absent",
                live("h1"),
                None,
                tomb("h1"),
                Nothing,
            ),
            (
                "remote row vanished, local edited",
                live("h1"),
                live("h2"),
                None,
                Conflict,
            ),
            (
                "remote row vanished, local unchanged",
                live("h1"),
                live("h1"),
                None,
                Down(DeleteLocal),
            ),
            (
                "recreated over tombstone base",
                tomb("h1"),
                live("h2"),
                tomb("h1"),
                Up(Upload),
            ),
            (
                "recreated elsewhere over tombstone base",
                tomb("h1"),
                None,
                live("h2"),
                Down(Download),
            ),
            (
                "never checkpointed, remote tombstone",
                None,
                live("h1"),
                tomb("h1"),
                Up(Upload),
            ),
            ("only base, both absent", live("h1"), None, None, Nothing),
        ];
        for (name, base, local, remote, expected) in table {
            assert_eq!(outcome(base, local, remote), expected, "case: {name}");
        }
    }

    #[test]
    fn mtime_never_decides() {
        let base = manifest(&[("note.md", entry("h1", 5, false))]);
        let local = manifest(&[("note.md", entry("h2", 1, false))]);
        let remote = manifest(&[("note.md", entry("h1", 9, false))]);

        let diff = diff_manifests(&base, &local, &remote);

        assert_eq!(diff.upload.len(), 1);
        assert_eq!(diff.upload[0].kind, SyncActionKind::Upload);
        assert_eq!(diff.upload[0].remote.as_ref().unwrap().hash, "h1");

        let same_hash_other_mtime = manifest(&[("note.md", entry("h1", 99, false))]);
        let diff = diff_manifests(&base, &base, &same_hash_other_mtime);
        assert!(diff.upload.is_empty() && diff.download.is_empty() && diff.conflicts.is_empty());
    }

    #[test]
    fn upload_over_remote_tombstone_carries_tombstone_as_parent() {
        let base = manifest(&[("note.md", entry("h1", 1, true))]);
        let local = manifest(&[("note.md", entry("h2", 1, false))]);
        let remote = manifest(&[("note.md", entry("h1", 1, true))]);

        let diff = diff_manifests(&base, &local, &remote);

        let action = &diff.upload[0];
        assert_eq!(action.kind, SyncActionKind::Upload);
        let parent = action.remote.as_ref().unwrap();
        assert!(parent.deleted);
        assert_eq!(parent.hash, "h1");
    }

    #[test]
    fn conflict_synthesizes_tombstone_for_missing_side() {
        let base = manifest(&[("note.md", entry("h1", 1, false))]);
        let local = manifest(&[("note.md", entry("h2", 1, false))]);
        let remote = Manifest::new();

        let diff = diff_manifests(&base, &local, &remote);

        let conflict = &diff.conflicts[0];
        assert_eq!(conflict.local.hash, "h2");
        assert!(conflict.remote.deleted);
        assert_eq!(conflict.remote.hash, "h1");
    }

    #[test]
    fn detects_new_local_file() {
        let local = manifest(&[("note.md", entry("a", 2, false))]);

        let diff = diff_manifests(&Manifest::new(), &local, &Manifest::new());

        assert_eq!(diff.upload.len(), 1);
        assert_eq!(diff.upload[0].kind, SyncActionKind::Upload);
        assert!(diff.upload[0].remote.is_none());
    }

    #[test]
    fn detects_new_remote_file() {
        let remote = manifest(&[("note.md", entry("a", 2, false))]);

        let diff = diff_manifests(&Manifest::new(), &Manifest::new(), &remote);

        assert_eq!(diff.download.len(), 1);
        assert_eq!(diff.download[0].kind, SyncActionKind::Download);
    }

    #[test]
    fn remote_tombstone_deletes_unchanged_local() {
        let base = manifest(&[("note.md", entry("local", 1, false))]);
        let local = manifest(&[("note.md", entry("local", 1, false))]);
        let remote = manifest(&[("note.md", entry("local", 2, true))]);

        let diff = diff_manifests(&base, &local, &remote);

        assert_eq!(diff.download.len(), 1);
        assert_eq!(diff.download[0].kind, SyncActionKind::DeleteLocal);
    }

    #[test]
    fn first_sync_with_differing_content_is_conflict() {
        let local = manifest(&[("note.md", entry("local", 9, false))]);
        let remote = manifest(&[("note.md", entry("remote", 2, false))]);

        let diff = diff_manifests(&Manifest::new(), &local, &remote);

        assert_eq!(diff.conflicts.len(), 1);
        assert_eq!(diff.upload.len(), 0);
        assert_eq!(diff.download.len(), 0);
    }

    #[test]
    fn ignores_identical_entries() {
        let same = manifest(&[("note.md", entry("same", 2, false))]);
        let other = manifest(&[("note.md", entry("older", 1, false))]);

        for base in [Manifest::new(), same.clone(), other] {
            let diff = diff_manifests(&base, &same, &same);
            assert!(diff.upload.is_empty());
            assert!(diff.download.is_empty());
            assert!(diff.conflicts.is_empty());
        }
    }

    #[test]
    fn checkpoint_manifest_holds_back_paths() {
        let base = manifest(&[("a", entry("h1", 1, false)), ("b", entry("h1", 1, false))]);
        let refetched = manifest(&[
            ("a", entry("h2", 2, false)),
            ("b", entry("h2", 2, false)),
            ("c", entry("h2", 2, false)),
        ]);
        let hold = BTreeSet::from(["a".to_string(), "c".to_string()]);

        let next = checkpoint_manifest(&base, &refetched, &hold);

        assert_eq!(next["a"].hash, "h1");
        assert_eq!(next["b"].hash, "h2");
        assert!(!next.contains_key("c"));
    }
}
