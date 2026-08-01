import FileProvider
import Foundation

/// One configured vault (spec §10 — multi-vault). The derived key lives in the
/// Keychain under `account = vaultID`, so each vault's key is stored separately.
struct VaultEntry: Codable, Identifiable, Equatable {
    var workerURL: String
    var apiKey: String
    var vaultID: String
    var name: String
    var id: String { vaultID }
}

/// UI snapshot of sync progress, derived from `MobileProgressEvent`.
struct SyncProgressInfo: Equatable {
    var phase: String
    var current: Int
    var total: Int
    var path: String?

    static func label(for phase: MobileSyncPhase) -> String {
        switch phase {
        case .downloading: return "Downloading"
        case .resolvingConflicts: return "Resolving conflicts"
        case .uploading: return "Uploading"
        }
    }
}

/// Bridges Rust sync progress events into `SyncModel.progress`. `onProgress`
/// fires on the sync's background thread (during the blocking Rust call), so it
/// hops to the main actor to update SwiftUI. Lives for one sync cycle.
final class SyncProgressListener: ProgressListener {
    private weak var model: SyncModel?
    private var phase: String = "Working"

    init(model: SyncModel) { self.model = model }

    func onProgress(event: MobileProgressEvent) {
        let info: SyncProgressInfo?
        switch event {
        case .phase(let p):
            phase = SyncProgressInfo.label(for: p)
            info = SyncProgressInfo(phase: phase, current: 0, total: 0, path: nil)
        case .fileStarted(let path, _, let index, let total):
            info = SyncProgressInfo(phase: phase, current: Int(index), total: Int(total), path: path)
        case .done:
            info = nil
        case .fileCompleted, .fileFailed:
            return
        }
        Task { @MainActor [weak model] in model?.progress = info }
    }
}

/// Drives sync from the SwiftUI layer by calling the Rust core through the
/// generated UniFFI bindings (`VaultClient`, `deriveMasterKey`, ...).
///
/// Vault files live in the shared App Group container so the File Provider
/// extension can serve the same data. Config persists in the group's
/// UserDefaults; the passphrase is held only in memory.
@MainActor
final class SyncModel: ObservableObject {
    static let appGroup = "group.com.obsink.shared"

    @Published var entries: [VaultEntry] = []
    @Published var activeVaultID: String = ""

    @Published var workerURL: String = "https://"
    @Published var apiKey: String = ""
    @Published var vaultID: String = ""
    @Published var passphrase: String = ""

    @Published var status: String = "Not synced"
    @Published var busy: Bool = false
    @Published var pendingLocalChanges: Int = 0
    @Published var hasStoredKey: Bool = false
    @Published var conflicts: [MobileConflict] = []
    @Published var choices: [String: MobileChoice] = [:]
    @Published var previews: [String: MobileConflictPreview] = [:]
    @Published var progress: SyncProgressInfo?
    @Published var failures: [MobileSyncFailure] = []

    private var client: VaultClient?
    private let defaults: UserDefaults

    init() {
        let defaults = UserDefaults(suiteName: Self.appGroup) ?? .standard
        self.defaults = defaults
        self.entries = Self.loadEntries(from: defaults)

        if let active = defaults.string(forKey: "activeVaultID"), entries.contains(where: { $0.vaultID == active }) {
            self.activeVaultID = active
        } else if let first = entries.first {
            self.activeVaultID = first.vaultID
        } else if let oldID = defaults.string(forKey: "vaultID"), !oldID.isEmpty {
            // Migrate a legacy single-vault config into the multi-vault list.
            let entry = VaultEntry(
                workerURL: defaults.string(forKey: "workerURL") ?? "https://",
                apiKey: defaults.string(forKey: "apiKey") ?? "",
                vaultID: oldID,
                name: oldID
            )
            self.entries = [entry]
            self.activeVaultID = oldID
            Self.saveEntries(self.entries, active: self.activeVaultID, to: defaults)
        }

        loadActiveIntoFields()
        refreshPending()
        refreshStoredKey()
    }

    var activeEntry: VaultEntry? {
        entries.first { $0.vaultID == activeVaultID }
    }

    /// Load the active vault's connection details into the editable fields.
    private func loadActiveIntoFields() {
        if let entry = activeEntry {
            workerURL = entry.workerURL
            apiKey = entry.apiKey
            vaultID = entry.vaultID
        } else {
            workerURL = "https://"
            apiKey = ""
            vaultID = ""
        }
        passphrase = ""
    }

    /// Switch the active vault (spec §10.3 vault picker).
    func selectVault(_ id: String) {
        guard entries.contains(where: { $0.vaultID == id }), id != activeVaultID else { return }
        persistConfig()
        activeVaultID = id
        Self.saveEntries(entries, active: activeVaultID, to: defaults)
        loadActiveIntoFields()
        conflicts = []
        choices = [:]
        previews = [:]
        refreshStoredKey()
        status = "Switched to \(activeEntry?.name ?? id)"
    }

    /// Add (or replace) a vault and make it active.
    func addVault(_ entry: VaultEntry) {
        if let idx = entries.firstIndex(where: { $0.vaultID == entry.vaultID }) {
            entries[idx] = entry
        } else {
            entries.append(entry)
        }
        activeVaultID = entry.vaultID
        Self.saveEntries(entries, active: activeVaultID, to: defaults)
        loadActiveIntoFields()
        refreshStoredKey()
        status = "Added vault \(entry.name)"
    }

    // MARK: Persistence

    private static func loadEntries(from defaults: UserDefaults) -> [VaultEntry] {
        guard let data = defaults.data(forKey: "vaultEntries"),
              let entries = try? JSONDecoder().decode([VaultEntry].self, from: data) else {
            return []
        }
        return entries
    }

    private static func saveEntries(_ entries: [VaultEntry], active: String, to defaults: UserDefaults) {
        if let data = try? JSONEncoder().encode(entries) {
            defaults.set(data, forKey: "vaultEntries")
        }
        defaults.set(active, forKey: "activeVaultID")
    }

    /// Persist the active vault's current fields back into the entry list.
    func persistConfig() {
        guard let idx = entries.firstIndex(where: { $0.vaultID == activeVaultID }) else { return }
        entries[idx].workerURL = workerURL
        entries[idx].apiKey = apiKey
        Self.saveEntries(entries, active: activeVaultID, to: defaults)
    }

    // MARK: Sync state helpers

    /// Count of File-Provider-queued local changes (pendingUpload/pendingDeletion),
    /// read from the shared item DB. Surfaces a "Sync to push" hint in the UI.
    func refreshPending() {
        pendingLocalChanges = (try? ItemStore.shared.pendingCount()) ?? 0
    }

    /// Whether a derived key is already in the Keychain for this vault (so sync
    /// can run without re-entering the passphrase).
    func refreshStoredKey() {
        hasStoredKey = !vaultID.isEmpty && KeychainStore.load(account: vaultID) != nil
    }

    /// Directory the Rust core reads/writes; Obsidian (via File Provider) sees the same files.
    var vaultDirectory: URL {
        let base = FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: Self.appGroup)
            ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        let dir = base.appendingPathComponent("Vault", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    func sync() {
        guard !busy else { return }
        persistConfig()
        busy = true
        status = "Syncing…"
        conflicts = []
        progress = nil
        failures = []

        let config = MobileVaultConfig(
            workerUrl: workerURL,
            apiKey: apiKey,
            vaultId: vaultID,
            localPath: vaultDirectory.path
        )
        let passphrase = self.passphrase
        let vaultID = self.vaultID
        let listener = SyncProgressListener(model: self)

        Task.detached {
            do {
                // Prefer the stored key; only derive (and store) on first setup.
                let key: Data
                if let stored = KeychainStore.load(account: vaultID) {
                    key = stored
                } else {
                    guard !passphrase.isEmpty else {
                        await self.fail(NSError(domain: "obsink", code: 1, userInfo: [
                            NSLocalizedDescriptionKey: "Enter a passphrase to set up this vault."
                        ]))
                        return
                    }
                    key = try deriveMasterKey(passphrase: passphrase, vaultId: vaultID)
                    KeychainStore.save(key, account: vaultID)
                }
                let client = try VaultClient(config: config, key: key)
                let outcome = try client.sync(listener: listener)
                await self.apply(outcome: outcome, client: client)
                await MainActor.run { self.refreshStoredKey() }
            } catch {
                await self.fail(error)
            }
        }
    }

    func resolve() {
        guard let client, !busy else { return }
        busy = true
        status = "Resolving…"
        progress = nil
        failures = []
        let resolutions = conflicts.map { conflict in
            MobileResolution(path: conflict.path, choice: choices[conflict.path] ?? .keepLocal)
        }
        let listener = SyncProgressListener(model: self)
        Task.detached {
            do {
                let outcome = try client.complete(resolutions: resolutions, listener: listener)
                await self.apply(outcome: outcome, client: client)
            } catch {
                await self.fail(error)
            }
        }
    }

    private func apply(outcome: SyncOutcome, client: VaultClient) {
        self.client = client
        conflicts = outcome.conflicts
        choices = Dictionary(uniqueKeysWithValues: outcome.conflicts.map { ($0.path, .keepLocal) })
        previews = [:]
        failures = outcome.failures
        progress = nil
        busy = false
        let failedSuffix = outcome.failures.isEmpty
            ? ""
            : " · \(outcome.failures.count) failed"
        if outcome.completed {
            status = "Synced · ↑\(outcome.uploaded) ↓\(outcome.downloaded)\(failedSuffix)"
            // OBS-20/21: mirror the freshly synced vault into the item DB, then
            // tell the File Provider to re-enumerate so Obsidian/Files see it.
            try? ItemStore.shared.reconcileAfterSync(completed: true, vaultRoot: vaultDirectory)
            // OBS-22/23: the core sync already pushed uploads/deletes by scanning
            // the vault dir; clear the FP's pending flags now.
            try? ItemStore.shared.drainPendingAfterSync(completed: true)
            signalFileProvider()
            refreshPending()
        } else if !outcome.conflicts.isEmpty {
            status = "\(outcome.conflicts.count) conflict(s) need attention"
            loadPreviews()
        } else {
            status = "Prepared · ↑\(outcome.uploaded) ↓\(outcome.downloaded)\(failedSuffix)"
        }
    }

    /// Fetch read-only local/remote content previews for each pending conflict
    /// (OBS-25) so the detail screen can show both versions.
    private func loadPreviews() {
        guard let client else { return }
        let paths = conflicts.map(\.path)
        Task.detached {
            var loaded: [String: MobileConflictPreview] = [:]
            for path in paths {
                if let preview = try? client.conflictPreview(path: path) {
                    loaded[path] = preview
                }
            }
            await MainActor.run { self.previews = loaded }
        }
    }

    /// Ask the system to re-enumerate the working set so the File Provider picks
    /// up the DB changes from `reconcileAfterSync`. Errors are ignored: on a fresh
    /// install or in the simulator the default domain may not be registered yet.
    private func signalFileProvider() {
        NSFileProviderManager.default.signalEnumerator(for: .workingSet) { _ in }
    }

    private func fail(_ error: Error) {
        busy = false
        progress = nil
        status = "Error: \(error.localizedDescription)"
    }
}
