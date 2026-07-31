import FileProvider
import Foundation

/// Drives sync from the SwiftUI layer by calling the Rust core through the
/// generated UniFFI bindings (`VaultClient`, `deriveMasterKey`, ...).
///
/// Vault files live in the shared App Group container so the File Provider
/// extension can serve the same data. Config persists in the group's
/// UserDefaults; the passphrase is held only in memory.
@MainActor
final class SyncModel: ObservableObject {
    static let appGroup = "group.com.obsink.shared"

    @Published var workerURL: String
    @Published var apiKey: String
    @Published var vaultID: String
    @Published var passphrase: String = ""

    @Published var status: String = "Not synced"
    @Published var busy: Bool = false
    @Published var pendingLocalChanges: Int = 0
    @Published var hasStoredKey: Bool = false
    @Published var conflicts: [MobileConflict] = []
    @Published var choices: [String: MobileChoice] = [:]
    @Published var previews: [String: MobileConflictPreview] = [:]

    private var client: VaultClient?
    private let defaults: UserDefaults

    init() {
        let defaults = UserDefaults(suiteName: Self.appGroup) ?? .standard
        self.defaults = defaults
        self.workerURL = defaults.string(forKey: "workerURL") ?? "https://"
        self.apiKey = defaults.string(forKey: "apiKey") ?? ""
        self.vaultID = defaults.string(forKey: "vaultID") ?? ""
        refreshPending()
        refreshStoredKey()
    }

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

    func persistConfig() {
        defaults.set(workerURL, forKey: "workerURL")
        defaults.set(apiKey, forKey: "apiKey")
        defaults.set(vaultID, forKey: "vaultID")
    }

    func sync() {
        guard !busy else { return }
        persistConfig()
        busy = true
        status = "Syncing…"
        conflicts = []

        let config = MobileVaultConfig(
            workerUrl: workerURL,
            apiKey: apiKey,
            vaultId: vaultID,
            localPath: vaultDirectory.path
        )
        let passphrase = self.passphrase
        let vaultID = self.vaultID

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
                let outcome = try client.sync()
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
        let resolutions = conflicts.map { conflict in
            MobileResolution(path: conflict.path, choice: choices[conflict.path] ?? .keepLocal)
        }
        Task.detached {
            do {
                let outcome = try client.complete(resolutions: resolutions)
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
        busy = false
        if outcome.completed {
            status = "Synced · ↑\(outcome.uploaded) ↓\(outcome.downloaded)"
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
            status = "Prepared · ↑\(outcome.uploaded) ↓\(outcome.downloaded)"
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
        status = "Error: \(error.localizedDescription)"
    }
}
