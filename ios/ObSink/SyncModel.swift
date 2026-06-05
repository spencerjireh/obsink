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
    @Published var conflicts: [MobileConflict] = []
    @Published var choices: [String: MobileChoice] = [:]

    private var client: VaultClient?
    private let defaults: UserDefaults

    init() {
        let defaults = UserDefaults(suiteName: Self.appGroup) ?? .standard
        self.defaults = defaults
        self.workerURL = defaults.string(forKey: "workerURL") ?? "https://"
        self.apiKey = defaults.string(forKey: "apiKey") ?? ""
        self.vaultID = defaults.string(forKey: "vaultID") ?? ""
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
                let key = try deriveMasterKey(passphrase: passphrase, vaultId: vaultID)
                let client = try VaultClient(config: config, key: key)
                let outcome = try client.sync()
                await self.apply(outcome: outcome, client: client)
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
        busy = false
        if outcome.completed {
            status = "Synced · ↑\(outcome.uploaded) ↓\(outcome.downloaded)"
        } else if !outcome.conflicts.isEmpty {
            status = "\(outcome.conflicts.count) conflict(s) need attention"
        } else {
            status = "Prepared · ↑\(outcome.uploaded) ↓\(outcome.downloaded)"
        }
    }

    private func fail(_ error: Error) {
        busy = false
        status = "Error: \(error.localizedDescription)"
    }
}
