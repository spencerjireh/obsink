import Foundation

/// One line of a vault's activity (DESIGN.md §5): what happened, then the
/// path. Mirrors the desktop's `activity::ActivityEvent`.
struct ActivityEvent: Codable, Identifiable, Equatable {
    enum Kind: String, Codable {
        case uploaded, downloaded, deletedHere, deletedOnServer, conflict, error, synced
    }

    var at: Date
    var kind: Kind
    var path: String?
    var detail: String?
    var id: String { "\(at.timeIntervalSince1970)-\(kind.rawValue)-\(path ?? detail ?? "")" }

    var line: String {
        let path = self.path ?? ""
        switch kind {
        case .uploaded: return "Uploaded \(path)"
        case .downloaded: return "Downloaded \(path)"
        case .deletedHere: return "Deleted here \(path)"
        case .deletedOnServer: return "Deleted on server \(path)"
        case .conflict: return "Conflict \(path)"
        case .error: return self.path == nil ? "Error: \(detail ?? "")" : "Failed \(path): \(detail ?? "")"
        case .synced: return "Synced · \(detail ?? "")"
        }
    }
}

/// Per-vault activity log, newest last on disk, capped. One JSON file per
/// vault under the App Group container (the desktop keeps the same shape
/// under `~/.obsink/activity/`). A write failure is logged and never fails
/// the sync that produced it.
enum ActivityLog {
    static let maxEvents = 200

    private static func url(vaultID: String) -> URL {
        let base = FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: FileProviderPaths.appGroup)
            ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        return base.appendingPathComponent("activity-\(vaultID).json")
    }

    /// Newest first.
    static func list(vaultID: String) -> [ActivityEvent] {
        guard let data = try? Data(contentsOf: url(vaultID: vaultID)),
              let events = try? JSONDecoder().decode([ActivityEvent].self, from: data) else { return [] }
        return events.reversed()
    }

    static func record(vaultID: String, outcome: SyncOutcome, plan: SyncOutcome?, now: Date = Date()) {
        var events = list(vaultID: vaultID).reversed() as [ActivityEvent]
        for failure in outcome.failures {
            events.append(ActivityEvent(at: now, kind: .error, path: failure.path, detail: failure.error))
        }
        for conflict in outcome.conflicts {
            events.append(ActivityEvent(at: now, kind: .conflict, path: conflict.path, detail: nil))
        }
        if outcome.completed {
            events.append(ActivityEvent(at: now, kind: .synced, path: nil, detail: "↑\(outcome.uploaded) ↓\(outcome.downloaded)"))
        }
        _ = plan
        save(vaultID: vaultID, events: events)
    }

    static func recordError(vaultID: String, message: String, now: Date = Date()) {
        var events = list(vaultID: vaultID).reversed() as [ActivityEvent]
        events.append(ActivityEvent(at: now, kind: .error, path: nil, detail: message))
        save(vaultID: vaultID, events: events)
    }

    /// Per-file lines from the progress stream of one cycle, appended when
    /// it finishes so they sit before the `Synced` summary.
    static func record(vaultID: String, transfers: [(path: String, kind: MobileActionKind)], now: Date = Date()) {
        guard !transfers.isEmpty else { return }
        var events = list(vaultID: vaultID).reversed() as [ActivityEvent]
        for transfer in transfers {
            let kind: ActivityEvent.Kind
            switch transfer.kind {
            case .upload: kind = .uploaded
            case .download: kind = .downloaded
            case .deleteLocal: kind = .deletedHere
            case .deleteRemote: kind = .deletedOnServer
            }
            events.append(ActivityEvent(at: now, kind: kind, path: transfer.path, detail: nil))
        }
        save(vaultID: vaultID, events: events)
    }

    static func forget(vaultID: String) {
        try? FileManager.default.removeItem(at: url(vaultID: vaultID))
    }

    private static func save(vaultID: String, events: [ActivityEvent]) {
        let kept = Array(events.suffix(maxEvents))
        do {
            let data = try JSONEncoder().encode(kept)
            try data.write(to: url(vaultID: vaultID), options: .atomic)
        } catch {
            NSLog("ObSink: activity log for %@ not written: %@", vaultID, "\(error)")
        }
    }
}
