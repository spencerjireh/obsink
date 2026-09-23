import Foundation

/// What one vault card shows for a vault this device holds, kept per vault
/// so several vaults can be looked at (and synced) without switching a
/// detail first. Pure data with pure derivations, so the wording is
/// unit-tested without the model.
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
    /// The vault key is in the Keychain. Without it (an entry from before
    /// the passphrase, a download that did not finish) the vault reads
    /// `Locked` until it is downloaded again.
    var hasStoredKey = false
    /// The server no longer lists this vault (deleted elsewhere); only
    /// `Remove from this device` applies.
    var deletedOnServer = false
    var lastSyncedAt: Date?

    /// The shared state vocabulary (DESIGN.md §5), worst thing first.
    static func statusLine(_ state: VaultState) -> String {
        switch state.phase {
        case .syncing: return "Syncing…"
        case .resolving: return "Resolving…"
        case .error(let message): return "Error: \(message)"
        case .idle: break
        }
        if state.deletedOnServer { return "Deleted on the server" }
        if !state.hasStoredKey { return "Locked" }
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

/// One row of the Vaults tab (spec §15.1): a vault this device holds (with
/// its `VaultState`), or one the account owns elsewhere (`Download`).
struct VaultRow: Identifiable, Equatable {
    let id: String
    let name: String
    /// Set for a vault this device holds.
    let entry: VaultEntry?
    /// The server's listing, when it answered.
    let summary: MobileVaultSummary?

    var onDevice: Bool { entry != nil }

    /// The merged list: every vault on this device first (its own state),
    /// then the account's other vaults. A stored vault the server no longer
    /// lists is flagged so the card says `Deleted on the server`.
    static func merge(entries: [VaultEntry], server: [MobileVaultSummary]?) -> [VaultRow] {
        var rows: [VaultRow] = entries.map { entry in
            let summary = server?.first { $0.id == entry.vaultID }
            return VaultRow(id: entry.vaultID, name: summary?.name ?? entry.name, entry: entry, summary: summary)
        }
        for summary in server ?? [] where !entries.contains(where: { $0.vaultID == summary.id }) {
            rows.append(VaultRow(id: summary.id, name: summary.name, entry: nil, summary: summary))
        }
        return rows
    }
}

/// `Last synced` wording shared by the cards.
enum RelativeTime {
    static func lastSynced(_ date: Date?, now: Date = Date(), locale: Locale = .current) -> String {
        guard let date else { return "Never synced" }
        return relative(date, now: now, locale: locale)
    }

    /// `Just now`, `5 minutes ago`, `Yesterday`, capitalised.
    static func relative(_ date: Date, now: Date = Date(), locale: Locale = .current) -> String {
        if now.timeIntervalSince(date) < 60 { return "Just now" }
        let formatter = RelativeDateTimeFormatter()
        formatter.locale = locale
        formatter.unitsStyle = .full
        formatter.dateTimeStyle = .named
        let text = formatter.localizedString(for: date, relativeTo: now)
        return text.prefix(1).uppercased() + text.dropFirst()
    }

    static func unix(_ seconds: UInt64?) -> Date? {
        guard let seconds, seconds > 0 else { return nil }
        return Date(timeIntervalSince1970: TimeInterval(seconds))
    }
}

/// The platform nouns of DESIGN.md §5.
enum PlatformLabel {
    static func text(_ platform: String) -> String {
        switch platform {
        case "macos": return "Mac"
        case "ios": return "iPhone"
        case "browser": return "Browser"
        case "cli": return "CLI"
        default: return "Device"
        }
    }

    static func symbol(_ platform: String) -> String {
        switch platform {
        case "macos": return "laptopcomputer"
        case "ios": return "iphone"
        case "browser": return "globe"
        case "cli": return "terminal"
        default: return "questionmark.circle"
        }
    }
}
