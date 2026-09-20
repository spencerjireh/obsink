import Foundation

/// What one vault card shows, kept per vault so several vaults can be
/// looked at (and synced) without switching an "active" one first. Pure
/// data with pure derivations, so the wording is unit-tested without the
/// model.
struct VaultState: Equatable {
    enum Phase: Equatable {
        case idle
        case syncing
        case resolving
        case error(String)
    }

    var phase: Phase = .idle
    /// File Provider writes not uploaded yet (`ItemStore.pendingCount`).
    var pendingLocal: Int = 0
    /// Remote files this device has not pulled (`vaultStatus`).
    var staleDownloads: Int = 0
    var conflicts: Int = 0
    var failures: Int = 0
    var hasStoredKey = false
    /// Configured against another server than this build's; read-only.
    var isForeign = false
    var lastSyncedAt: Date?

    static func isForeign(entryURL: String, defaultURL: String) -> Bool {
        KeychainStore.canonicalServerURL(entryURL) != KeychainStore.canonicalServerURL(defaultURL)
    }

    /// The shared state vocabulary (DESIGN.md §5), worst thing first.
    static func statusLine(_ state: VaultState) -> String {
        switch state.phase {
        case .syncing: return "Syncing…"
        case .resolving: return "Resolving…"
        case .error(let message): return "Error: \(message)"
        case .idle: break
        }
        if state.isForeign { return "On another server" }
        if !state.hasStoredKey { return "Needs passphrase" }
        if state.conflicts > 0 {
            return state.conflicts == 1 ? "1 conflict" : "\(state.conflicts) conflicts"
        }
        if state.staleDownloads > 0 && state.pendingLocal > 0 {
            return "\(state.pendingLocal) to upload · \(state.staleDownloads) to download"
        }
        if state.staleDownloads > 0 { return "\(state.staleDownloads) to download" }
        if state.pendingLocal > 0 { return "\(state.pendingLocal) to upload" }
        return "Up to date"
    }

    /// A finished cycle: `completed` clears what it pulled and records the
    /// time; a stop on conflicts only counts them.
    mutating func apply(outcome: SyncOutcome, now: Date) {
        phase = .idle
        failures = outcome.failures.count
        if outcome.completed {
            lastSyncedAt = now
            staleDownloads = 0
            conflicts = 0
        } else {
            conflicts = outcome.conflicts.count
        }
    }

    /// The stale check: what the server has that this device does not.
    mutating func apply(status: MobileVaultStatus) {
        staleDownloads = Int(status.pendingDownloads)
        conflicts = Int(status.pendingConflicts)
    }
}

/// `Last synced` wording shared by the cards.
enum RelativeTime {
    static func lastSynced(_ date: Date?, now: Date = Date(), locale: Locale = .current) -> String {
        guard let date else { return "Never synced" }
        if now.timeIntervalSince(date) < 60 { return "Just now" }
        let formatter = RelativeDateTimeFormatter()
        formatter.locale = locale
        formatter.unitsStyle = .full
        formatter.dateTimeStyle = .named
        let text = formatter.localizedString(for: date, relativeTo: now)
        return text.prefix(1).uppercased() + text.dropFirst()
    }
}
