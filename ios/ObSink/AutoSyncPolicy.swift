import Foundation

/// Why an automatic sync is running; shapes the throttle only.
enum AutoSyncReason {
    case launch
    case foreground
    case background
}

/// Decides, per vault, whether an automatic sync should run now. Pure so
/// the rules are unit-tested without the model: a vault syncs when it has
/// File Provider writes waiting, the server is ahead, or its last sync is
/// older than `staleAfter`; never while busy, foreign, keyless or holding
/// conflicts (those need the user).
enum AutoSyncPolicy {
    /// A vault that has not synced for this long is synced on the next
    /// foreground or background refresh. Matches the shortest interval
    /// `BGAppRefreshTask` honours.
    static let staleAfter: TimeInterval = 15 * 60

    static func shouldSync(_ state: VaultState, now: Date) -> Bool {
        guard state.phase == .idle, !state.isForeign, state.hasStoredKey, state.conflicts == 0 else {
            return false
        }
        if state.pendingLocal > 0 || state.staleDownloads > 0 { return true }
        guard let last = state.lastSyncedAt else { return true }
        return now.timeIntervalSince(last) > staleAfter
    }
}
