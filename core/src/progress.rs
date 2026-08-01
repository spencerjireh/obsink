//! Progress reporting for the sync engine.
//!
//! Sync can move many files; the [`ProgressSink`] trait lets each facade observe
//! the run without the core knowing about Tauri events, UniFFI callbacks, or
//! stderr. Pass a [`NoProgress`] when observation isn't needed (e.g. tests).

use serde::{Deserialize, Serialize};

use crate::types::SyncActionKind;

/// Broad phase of the sync cycle, reported before the per-file events for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncPhase {
    Downloading,
    ResolvingConflicts,
    Uploading,
}

/// A single observable event emitted during `prepare_sync` / `complete_sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProgressEvent {
    /// A new phase is starting (download / resolve / upload).
    Phase(SyncPhase),
    /// One file in a batch is about to be transferred. `index` is 0-based.
    FileStarted {
        path: String,
        kind: SyncActionKind,
        index: usize,
        total: usize,
    },
    /// A file finished transferring; `bytes` is the payload size moved.
    FileCompleted { path: String, bytes: u64 },
    /// A single file could not be transferred (partial-sync recovery).
    FileFailed { path: String, error: String },
    /// The sync cycle ended. Counts are per-cycle totals.
    Done {
        uploaded: usize,
        downloaded: usize,
        failed: usize,
    },
}

/// Receiver of sync progress events. Implementations translate events into the
/// facade's native channel (Tauri emit, UniFFI callback, stderr print).
pub trait ProgressSink: Send + Sync {
    fn report(&self, event: ProgressEvent);
}

/// Sink that discards every event. The default for call sites that don't care.
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn report(&self, _event: ProgressEvent) {}
}

/// Emit a helper when the sink is optional-looking; keeps call sites tidy.
pub fn emit(sink: &dyn ProgressSink, event: ProgressEvent) {
    sink.report(event);
}
