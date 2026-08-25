import FileProvider
import Foundation

/// One configured vault (spec §10 — multi-vault). The derived key lives in the
/// Keychain under `account = vaultID`; the server bearer (ObSink Cloud session
/// or self-hosted API key) under `bearer:<workerURL>`. `apiKey` is only decoded
/// from pre-accounts configs and UI-test seeds, then moved into the Keychain.
struct VaultEntry: Codable, Identifiable, Equatable {
    var workerURL: String
    var apiKey: String? = nil
    var vaultID: String
    var name: String
    var id: String { vaultID }

    init(workerURL: String, apiKey: String? = nil, vaultID: String, name: String) {
        self.workerURL = workerURL
        self.apiKey = apiKey
        self.vaultID = vaultID
        self.name = name
    }

    /// True when the vault lives on ObSink Cloud (the hosted Worker).
    var isHosted: Bool {
        KeychainStore.canonicalWorkerURL(workerURL) == KeychainStore.canonicalWorkerURL(hostedWorkerUrl())
    }
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
    /// Editable self-hosted API key for the active vault; mirrors the Keychain
    /// bearer for `workerURL` (hidden for ObSink Cloud vaults).
    @Published var apiKey: String = ""
    @Published var vaultID: String = ""
    @Published var passphrase: String = ""
    /// Email of the ObSink Cloud account behind the active vault (nil when
    /// self-hosted or signed out).
    @Published var accountEmail: String?
    @Published var hasBearer: Bool = false

    @Published var status: String = "Not synced"
    @Published var busy: Bool = false
    @Published var pendingLocalChanges: Int = 0
    @Published var hasStoredKey: Bool = false
    @Published var conflicts: [MobileConflict] = []
    @Published var choices: [String: MobileChoice] = [:]
    @Published var previews: [String: MobileConflictPreview] = [:]
    @Published var progress: SyncProgressInfo?
    @Published var failures: [MobileSyncFailure] = []

    /// Remote files this device hasn't pulled yet — the stale-vault warning's
    /// data source (spec §3.4, OBS-33). Refreshed on open and vault switch.
    @Published var staleDownloads: Int = 0

    private var client: VaultClient?
    private let defaults: UserDefaults
    private var resetFileProviderDomain = false

    init() {
        let defaults = UserDefaults(suiteName: Self.appGroup) ?? .standard
        self.defaults = defaults

        // UI-test hook: OBSINK_UITEST_SEED carries a JSON [VaultEntry] to start
        // from a known state without driving the Add Vault flow. Inert unless
        // the harness sets it.
        let env = ProcessInfo.processInfo.environment
        let resetForUITest = env["OBSINK_UITEST_RESET"] == "1"
        if resetForUITest {
            defaults.removeObject(forKey: "vaultEntries")
            defaults.removeObject(forKey: "activeVaultID")
            defaults.removeObject(forKey: "vaultID")
        }
        self.resetFileProviderDomain = resetForUITest
        if let seed = env["OBSINK_UITEST_SEED"],
           let data = seed.data(using: .utf8),
           let seeded = try? JSONDecoder().decode([VaultEntry].self, from: data) {
            Self.saveEntries(seeded, active: seeded.first?.vaultID ?? "", to: defaults)
        }

        self.entries = Self.loadEntries(from: defaults)
        if Self.migrateBearers(&self.entries) {
            Self.saveEntries(self.entries, active: defaults.string(forKey: "activeVaultID") ?? "", to: defaults)
        }

        if let active = defaults.string(forKey: "activeVaultID"), entries.contains(where: { $0.vaultID == active }) {
            self.activeVaultID = active
        } else if let first = entries.first {
            self.activeVaultID = first.vaultID
        } else if let oldID = defaults.string(forKey: "vaultID"), !oldID.isEmpty {
            // Migrate a legacy single-vault config into the multi-vault list.
            var entry = VaultEntry(
                workerURL: defaults.string(forKey: "workerURL") ?? "https://",
                apiKey: defaults.string(forKey: "apiKey"),
                vaultID: oldID,
                name: oldID
            )
            var migrating = [entry]
            _ = Self.migrateBearers(&migrating)
            entry = migrating[0]
            self.entries = [entry]
            self.activeVaultID = oldID
            Self.saveEntries(self.entries, active: self.activeVaultID, to: defaults)
        }

        loadActiveIntoFields()
        refreshPending()
        refreshStoredKey()
        registerFileProviderDomain()
    }

    var activeEntry: VaultEntry? {
        entries.first { $0.vaultID == activeVaultID }
    }

    var activeIsHosted: Bool { activeEntry?.isHosted ?? false }

    /// Bearer for the active vault's Worker: Keychain first, then the field.
    var bearer: String {
        KeychainStore.loadBearer(workerURL: workerURL) ?? apiKey
    }

    /// Move any plaintext `apiKey` (legacy config / UI-test seed) into the
    /// Keychain. Returns true when the entry list changed and must be re-saved.
    static func migrateBearers(_ entries: inout [VaultEntry]) -> Bool {
        var changed = false
        for idx in entries.indices {
            if let key = entries[idx].apiKey, !key.isEmpty {
                KeychainStore.saveBearer(key, workerURL: entries[idx].workerURL)
                entries[idx].apiKey = nil
                changed = true
            }
        }
        return changed
    }

    /// Load the active vault's connection details into the editable fields.
    private func loadActiveIntoFields() {
        if let entry = activeEntry {
            workerURL = entry.workerURL
            apiKey = entry.isHosted ? "" : (KeychainStore.loadBearer(workerURL: entry.workerURL) ?? "")
            vaultID = entry.vaultID
        } else {
            workerURL = "https://"
            apiKey = ""
            vaultID = ""
        }
        passphrase = ""
        hasBearer = KeychainStore.loadBearer(workerURL: workerURL) != nil
        accountEmail = nil
        refreshAccount()
    }

    /// Resolve the ObSink Cloud account behind the active vault (for the
    /// "signed in as" line). No-op for self-hosted vaults.
    func refreshAccount() {
        guard activeIsHosted, let token = KeychainStore.loadBearer(workerURL: workerURL),
              token.hasPrefix("os_") else { return }
        let url = workerURL
        Task.detached { [weak self] in
            let email = (try? authMe(workerUrl: url, token: token))?.email
            await MainActor.run { [weak self] in
                guard let self, self.workerURL == url else { return }
                self.accountEmail = email
            }
        }
    }

    /// Sign out of the active vault's Worker: revoke the Cloud session (best
    /// effort) and forget the bearer. Vault entries stay; sync needs a sign-in.
    func signOut() {
        let url = workerURL
        if let token = KeychainStore.loadBearer(workerURL: url), token.hasPrefix("os_") {
            Task.detached { try? authLogout(workerUrl: url, token: token) }
        }
        KeychainStore.deleteBearer(workerURL: url)
        apiKey = ""
        accountEmail = nil
        hasBearer = false
        status = "Signed out"
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
        staleDownloads = 0
        refreshStoredKey()
        status = "Switched to \(activeEntry?.name ?? id)"
        checkStale()
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

    /// Persist the active vault's current fields: URL into the entry list, a
    /// (self-hosted) API key into the Keychain.
    func persistConfig() {
        guard let idx = entries.firstIndex(where: { $0.vaultID == activeVaultID }) else { return }
        entries[idx].workerURL = workerURL
        Self.saveEntries(entries, active: activeVaultID, to: defaults)
        if !activeIsHosted, !apiKey.isEmpty, KeychainStore.loadBearer(workerURL: workerURL) != apiKey {
            KeychainStore.saveBearer(apiKey, workerURL: workerURL)
        }
        hasBearer = KeychainStore.loadBearer(workerURL: workerURL) != nil
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
            apiKey: bearer,
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
            staleDownloads = 0
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

    // MARK: File Provider domain

    /// The single ObSink File Provider domain (spec §11). Registered on launch
    /// so synced files appear under "ObSink" in the Files app and Obsidian.
    static let fpDomain = NSFileProviderDomain(
        identifier: NSFileProviderDomainIdentifier(rawValue: "obsink"),
        displayName: "ObSink"
    )

    /// Register the domain with the system. Adding an already-registered domain
    /// is a no-op, so this is safe to call on every launch.
    private func registerFileProviderDomain() {
        let domain = Self.fpDomain
        #if targetEnvironment(simulator)
        // The simulator keeps third-party domains user-disabled (FP error
        // -2011) with no UI to enable them; testing modes force the domain on.
        // Simulator builds only — on device the user enables it in Files.
        domain.testingModes = [.alwaysEnabled, .interactive]
        #endif
        let add = {
            NSFileProviderManager.add(domain) { error in
                if let error {
                    NSLog("ObSink: File Provider domain registration failed: \(error.localizedDescription)")
                }
            }
        }
        if resetFileProviderDomain {
            // UI-test reset: domain state survives app reinstall, so drop it
            // before re-adding to start from a clean slate.
            NSFileProviderManager.remove(domain) { _ in add() }
        } else {
            add()
        }
    }

    /// Ask the system to re-enumerate the working set so the File Provider picks
    /// up the DB changes from `reconcileAfterSync`. Errors are ignored: on a fresh
    /// install the domain registration may still be in flight.
    private func signalFileProvider() {
        NSFileProviderManager(for: Self.fpDomain)?.signalEnumerator(for: .workingSet) { _ in }
    }

    // MARK: Stale-vault warning (spec §3.4, OBS-33)

    /// Compare the local working manifest against the server without syncing.
    /// Runs only when the vault is fully configured with a stored key (no
    /// passphrase prompt on open); quietly does nothing otherwise.
    func checkStale() {
        guard !busy, !vaultID.isEmpty else { return }
        let config = MobileVaultConfig(
            workerUrl: workerURL,
            apiKey: bearer,
            vaultId: vaultID,
            localPath: vaultDirectory.path
        )
        guard let key = KeychainStore.load(account: vaultID) else { return }
        Task.detached { [weak self] in
            // Retry a couple of times: a transient network error on open would
            // otherwise silently suppress the warning until the next foreground.
            var pending: UInt32 = 0
            for attempt in 1...3 {
                do {
                    pending = try VaultClient(config: config, key: key).vaultStatus().pendingDownloads
                    break
                } catch {
                    NSLog("ObSink: stale check attempt %d failed: %@", attempt, error.localizedDescription)
                    try? await Task.sleep(nanoseconds: 2_000_000_000)
                }
            }
            await MainActor.run { [weak self] in
                guard let self, !self.busy else { return }
                self.staleDownloads = Int(pending)
            }
        }
    }

    private func fail(_ error: Error) {
        busy = false
        progress = nil
        status = "Error: \(error.localizedDescription)"
    }
}
