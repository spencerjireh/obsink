//! The decisions of a sync cycle that need no filesystem and no network:
//! how uploads are batched, how a conflict choice is applied, what a
//! `KeepBoth` copy is called. `sync_engine` runs them natively; the browser
//! client runs the same code through `core-wasm`, so the two drivers cannot
//! drift on these rules.

use std::path::Path;

use crate::types::{Conflict, ConflictResolutionChoice, SyncAction, SyncActionKind};

/// Ciphertext per batch upload. Well under the server's 128 MiB body cap and
/// the memory a phone can spare for one request.
pub const BATCH_BYTE_BUDGET: u64 = 32 * 1024 * 1024;
/// Operations per batch upload.
pub const BATCH_MAX_OPS: usize = 64;

/// Split uploads into consecutive batches: a batch closes when adding the
/// next file would exceed `BATCH_BYTE_BUDGET` or `BATCH_MAX_OPS`. A file
/// larger than the budget travels alone. Sizes are the plaintext sizes from
/// the manifest (ciphertext adds a constant few bytes), so no file is read
/// before its batch is due.
pub fn chunk_uploads(sizes: &[u64]) -> Vec<std::ops::Range<usize>> {
    let mut batches = Vec::new();
    let mut start = 0usize;
    let mut bytes = 0u64;
    for (index, &size) in sizes.iter().enumerate() {
        let full =
            index > start && (bytes + size > BATCH_BYTE_BUDGET || index - start >= BATCH_MAX_OPS);
        if full {
            batches.push(start..index);
            start = index;
            bytes = 0;
        }
        bytes += size;
    }
    if start < sizes.len() {
        batches.push(start..sizes.len());
    }
    batches
}

/// `KeepBoth` needs two live versions. When one side is a deletion there is
/// nothing to copy, so it collapses to keeping the side that still exists.
pub fn effective_choice(
    choice: &ConflictResolutionChoice,
    conflict: &Conflict,
) -> ConflictResolutionChoice {
    match choice {
        ConflictResolutionChoice::KeepBoth if conflict.remote.deleted => {
            ConflictResolutionChoice::KeepLocal
        }
        ConflictResolutionChoice::KeepBoth if conflict.local.deleted => {
            ConflictResolutionChoice::KeepRemote
        }
        other => other.clone(),
    }
}

/// The upload (or remote delete) that keeps the local side of a conflict; the
/// remote entry rides along as the parent hash the server checks.
pub fn conflict_to_upload(conflict: &Conflict) -> SyncAction {
    SyncAction {
        path: conflict.path.clone(),
        kind: if conflict.local.deleted {
            SyncActionKind::DeleteRemote
        } else {
            SyncActionKind::Upload
        },
        local: Some(conflict.local.clone()),
        remote: Some(conflict.remote.clone()),
    }
}

/// The remote hash the server checks; a synthesized tombstone has none.
pub fn parent_hash(action: &SyncAction) -> Option<&str> {
    action
        .remote
        .as_ref()
        .map(|entry| entry.hash.as_str())
        .filter(|hash| !hash.is_empty())
}

/// Where `KeepBoth` writes the remote version: `notes/today.conflict.md`.
pub fn conflict_copy_path(original: &str) -> String {
    let path = Path::new(original);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(original);
    let extension = path.extension().and_then(|value| value.to_str());
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let file_name = match extension {
        Some(extension) => format!("{stem}.conflict.{extension}"),
        None => format!("{stem}.conflict"),
    };

    match parent {
        Some(parent) => format!("{}/{}", parent.to_string_lossy(), file_name),
        None => file_name,
    }
}

#[cfg(test)]
mod tests {
    use super::{chunk_uploads, conflict_copy_path, BATCH_BYTE_BUDGET, BATCH_MAX_OPS};

    #[test]
    fn conflict_copy_keeps_extension() {
        assert_eq!(
            conflict_copy_path("notes/today.md"),
            "notes/today.conflict.md"
        );
        assert_eq!(conflict_copy_path("todo"), "todo.conflict");
    }

    #[test]
    fn chunk_uploads_splits_by_bytes_and_op_count() {
        assert!(chunk_uploads(&[]).is_empty());
        assert_eq!(chunk_uploads(&[1, 2, 3]), vec![0..3]);

        // Op count: 64 per batch.
        let sizes = vec![1u64; BATCH_MAX_OPS * 2 + 1];
        assert_eq!(chunk_uploads(&sizes), vec![0..64, 64..128, 128..129]);

        // Byte budget: the file that would overflow starts the next batch.
        let half = BATCH_BYTE_BUDGET / 2;
        assert_eq!(chunk_uploads(&[half, half, 1, half]), vec![0..2, 2..4]);

        // An oversize file travels alone, and does not drag its neighbours.
        let huge = BATCH_BYTE_BUDGET + 1;
        assert_eq!(chunk_uploads(&[1, huge, 1]), vec![0..1, 1..2, 2..3]);
        assert_eq!(chunk_uploads(&[huge]), vec![0..1]);

        // Deletes cost nothing.
        let sizes = vec![0u64; 10];
        assert_eq!(chunk_uploads(&sizes), vec![0..10]);
    }
}
