import FileProvider
import Foundation

/// One configured vault (spec §10 — multi-vault). The derived key lives in the
/// Keychain under `account = vaultID`; the server bearer (session token) under
/// `bearer:<serverURL>`. Entries written before the server pivot used the
/// `workerURL` key; decoding accepts both.
struct VaultEntry: Codable, Identifiable, Equatable {
    var serverURL: String
    var vaultID: String
    var name: String
    var id: String { vaultID }

    init(serverURL: String, vaultID: String, name: String) {
        self.serverURL = serverURL
        self.vaultID = vaultID
        self.name = name
    }

    private enum CodingKeys: String, CodingKey {
        case serverURL, workerURL, vaultID, name
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        serverURL = try c.decodeIfPresent(String.self, forKey: .serverURL)
            ?? c.decode(String.self, forKey: .workerURL)
        vaultID = try c.decode(String.self, forKey: .vaultID)
        name = try c.decode(String.self, forKey: .name)
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(serverURL, forKey: .serverURL)
        try c.encode(vaultID, forKey: .vaultID)
        try c.encode(name, forKey: .name)
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
/// Vault files live in the shared App Group container, one directory and one
/// item database per vault (`Vault/<vaultID>/`, `items-<vaultID>.sqlite`),
/// each exposed through its own File Provider domain so the extension can
/// serve the same data. Config persists in the group's UserDefaults; the
/// passphrase is held only in memory.
@MainActor
final class SyncModel: ObservableObject {
    static let appGroup = "group.com.obsink.shared"

    @Published var entries: [VaultEntry] = []
    /// The vault whose conflicts, failures and last result are loaded: the
    /// one synced most recently, or the one the user picked. Every vault has
    /// its own `VaultState`; this only decides where the detail lives.
    @Published var activeVaultID: String = ""
    /// Per-vault state for the cards, keyed by vault id.
    @Published var vaultStates: [String: VaultState] = [:]
    /// The one-time "Open in Obsidian" card has been dismissed.
    @Published var guidanceDismissed: Bool
    /// Outcome of an account action (`Signed out`, `Account deleted`, ...),
    /// shown on the Settings tab.
    @Published var accountNotice: String?

    /// The one server this build talks to (`ServerConfig`).
    var serverURL: String { ServerConfig.defaultURL }
    /// The account on that server (`GET /auth/me`); nil when signed out or
    /// when the bearer is the operator key.
    @Published var account: MobileAccount?
    /// Invites this account minted, newest first.
    @Published var invites: [MobileInvite] = []
    /// The most recently minted invite code, shown until dismissed.
    @Published var issuedInvite: MobileInvite?
    @Published var hasBearer: Bool = false
    /// A bearer call came back 401: the bearer is gone and the account section
    /// offers `Sign in`.
    @Published var sessionExpired: Bool = false
    /// One-off failure of an account or vault action (revoke, delete, remove).
    @Published var alert: AppAlert?

    var accountEmail: String? { account?.email }
    var accountUsage: MobileUsage? { account?.usage }

    /// The active vault's last result, as a sentence (`Synced · ↑n ↓n`,
    /// `Added vault X`, `Error: …`). Shown on that vault's card.
    @Published var status: String = "Not synced"
    @Published var busy: Bool = false
    @Published var conflicts: [MobileConflict] = []
    @Published var choices: [String: MobileChoice] = [:]
    @Published var previews: [String: MobileConflictPreview] = [:]
    @Published var progress: SyncProgressInfo?
    @Published var failures: [MobileSyncFailure] = []

    private var client: VaultClient?
    private let defaults: UserDefaults
    private var resetFileProviderDomain = false

    init() {
        let defaults = UserDefaults(suiteName: Self.appGroup) ?? .standard
        self.defaults = defaults

        // UI-test hooks: OBSINK_UITEST_SEED carries a JSON [VaultEntry] to start
        // from a known state without driving the Add Vault flow;
        // OBSINK_UITEST_BEARER (+ _URL) seeds the server bearer into the
        // Keychain the way a sign-in would. Inert unless the harness sets them.
        let env = ProcessInfo.processInfo.environment
        let resetForUITest = env["OBSINK_UITEST_RESET"] == "1"
        if resetForUITest {
            defaults.removeObject(forKey: "vaultEntries")
            defaults.removeObject(forKey: "activeVaultID")
            defaults.removeObject(forKey: "vaultID")
            defaults.removeObject(forKey: Self.guidanceDismissedKey)
            for key in defaults.dictionaryRepresentation().keys where key.hasPrefix(Self.lastSyncedPrefix) {
                defaults.removeObject(forKey: key)
            }
        }
        self.guidanceDismissed = defaults.bool(forKey: Self.guidanceDismissedKey)
        self.resetFileProviderDomain = resetForUITest
        if let seed = env["OBSINK_UITEST_SEED"],
           let data = seed.data(using: .utf8),
           let seeded = try? JSONDecoder().decode([VaultEntry].self, from: data) {
            Self.saveEntries(seeded, active: seeded.first?.vaultID ?? "", to: defaults)
        }
        if let bearer = env["OBSINK_UITEST_BEARER"], let url = env["OBSINK_UITEST_BEARER_URL"],
           !bearer.isEmpty, !url.isEmpty {
            KeychainStore.saveBearer(bearer, serverURL: url)
        }

        self.entries = Self.loadEntries(from: defaults)

        if let active = defaults.string(forKey: "activeVaultID"), entries.contains(where: { $0.vaultID == active }) {
            self.activeVaultID = active
        } else if let first = entries.first {
            self.activeVaultID = first.vaultID
        }

        Self.migrateLegacyStorage(activeVaultID: activeVaultID, defaults: defaults)
        rebuildVaultStates()
        loadBearerState()
        syncFileProviderDomains()
        finishPendingRemovals()
    }

    var activeEntry: VaultEntry? {
        entries.first { $0.vaultID == activeVaultID }
    }

    /// Bearer for this build's server (Keychain), empty when signed out.
    var bearer: String {
        KeychainStore.loadBearer(serverURL: serverURL) ?? ""
    }

    func isOnDefaultServer(_ entry: VaultEntry) -> Bool {
        !VaultState.isForeign(entryURL: entry.serverURL, defaultURL: serverURL)
    }

    /// Whether this build can talk to the vault: on its server, with a key.
    func canSync(_ vaultID: String) -> Bool {
        guard let state = vaultStates[vaultID] else { return false }
        return !state.isForeign && !sessionExpired
    }

    private static let guidanceDismissedKey = "guidanceDismissed"
    private static let lastSyncedPrefix = "lastSynced."

    private static func lastSyncedKey(_ vaultID: String) -> String { lastSyncedPrefix + vaultID }

    /// Build every vault's state from what is on the device: key present,
    /// server, last sync time, pending File Provider writes.
    private func rebuildVaultStates() {
        var states: [String: VaultState] = [:]
        for entry in entries {
            states[entry.vaultID] = freshState(for: entry)
        }
        vaultStates = states
    }

    private func freshState(for entry: VaultEntry) -> VaultState {
        var state = VaultState()
        state.hasStoredKey = KeychainStore.load(account: entry.vaultID) != nil
        state.isForeign = !isOnDefaultServer(entry)
        state.lastSyncedAt = defaults.object(forKey: Self.lastSyncedKey(entry.vaultID)) as? Date
        state.pendingLocal = (try? ItemStore.store(for: entry.vaultID).pendingCount()) ?? 0
        return state
    }

    /// Pick up the bearer (if any) and the account behind it.
    private func loadBearerState() {
        hasBearer = KeychainStore.loadBearer(serverURL: serverURL) != nil
        account = nil
        invites = []
        issuedInvite = nil
        refreshAccount()
    }

    func dismissGuidance() {
        guidanceDismissed = true
        defaults.set(true, forKey: Self.guidanceDismissedKey)
    }

    /// Resolve the account behind the active vault. The operator bearer has
    /// no account (the facade reports `Sync`), so the section stays generic;
    /// a 401 means the session is gone.
    func refreshAccount() {
        guard let token = KeychainStore.loadBearer(serverURL: serverURL) else { return }
        let url = serverURL
        Task.detached { [weak self] in
            let result = Result { try authMe(serverUrl: url, token: token) }
            await MainActor.run { [weak self] in
                guard let self else { return }
                switch result {
                case .success(let account):
                    self.account = account
                    self.sessionExpired = false
                    self.refreshInvites()
                case .failure(let error) where error.isUnauthorized:
                    self.handleUnauthorized()
                case .failure:
                    self.account = nil
                }
            }
        }
    }

    /// Invites this account minted (`GET /auth/invites`). Failures keep the
    /// old list: the operator bearer can list too, so this rarely fails.
    func refreshInvites() {
        guard let token = KeychainStore.loadBearer(serverURL: serverURL) else { return }
        let url = serverURL
        Task.detached { [weak self] in
            guard let invites = try? authListInvites(serverUrl: url, token: token) else { return }
            await MainActor.run { [weak self] in
                self?.invites = invites
            }
        }
    }

    /// Mint an invite code for someone else to join this server.
    func createInvite() {
        guard let token = KeychainStore.loadBearer(serverURL: serverURL) else { return }
        let url = serverURL
        Task.detached { [weak self] in
            do {
                let invite = try authCreateInvite(serverUrl: url, token: token)
                await MainActor.run { [weak self] in
                    self?.issuedInvite = invite
                    self?.refreshInvites()
                }
            } catch {
                await self?.actionFailed("Invite someone", error)
            }
        }
    }

    /// Sign out another device of this account (`DELETE /auth/sessions/:id`).
    func revokeSession(_ sessionID: String) {
        guard let token = KeychainStore.loadBearer(serverURL: serverURL) else { return }
        let url = serverURL
        busy = true
        Task.detached { [weak self] in
            do {
                try authRevokeSession(serverUrl: url, token: token, sessionId: sessionID)
                await MainActor.run { [weak self] in
                    self?.busy = false
                    self?.accountNotice = "Device signed out."
                    self?.refreshAccount()
                }
            } catch {
                await self?.actionFailed("Sign out", error)
            }
        }
    }

    /// Delete the account and everything it owns on the server, then forget
    /// the bearer and every vault on that server here (App Store 5.1.1(v)).
    /// Vault files on this device go with the vaults (they are the server's
    /// copies; the user asked for the account to be gone).
    func deleteAccount() {
        guard let token = KeychainStore.loadBearer(serverURL: serverURL) else { return }
        let url = serverURL
        busy = true
        Task.detached { [weak self] in
            let result = Result { try authDeleteAccount(serverUrl: url, token: token) }
            await MainActor.run { [weak self] in
                guard let self else { return }
                switch result {
                case .success:
                    break
                case .failure(let error) where error.isUnauthorized:
                    // Already gone on the server; finish the local part.
                    break
                case .failure(let error):
                    self.actionFailed("Delete account", error)
                    return
                }
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    let onServer = self.entries.filter {
                        KeychainStore.canonicalServerURL($0.serverURL) == KeychainStore.canonicalServerURL(url)
                    }
                    for entry in onServer {
                        await self.removeVaultLocally(entry.vaultID)
                    }
                    KeychainStore.deleteBearer(serverURL: url)
                    self.account = nil
                    self.invites = []
                    self.issuedInvite = nil
                    self.hasBearer = false
                    self.sessionExpired = false
                    self.busy = false
                    self.accountNotice = "Account deleted."
                }
            }
        }
    }

    /// Delete a vault on the server (`DELETE /vaults/:id`), then forget it here.
    func deleteVaultOnServer(_ vaultID: String) {
        guard let entry = entries.first(where: { $0.vaultID == vaultID }),
              let token = KeychainStore.loadBearer(serverURL: entry.serverURL) else { return }
        busy = true
        Task.detached { [weak self] in
            do {
                try deleteVault(serverUrl: entry.serverURL, apiKey: token, vaultId: vaultID)
                await MainActor.run { [weak self] in
                    Task { @MainActor [weak self] in
                        await self?.removeVaultLocally(vaultID)
                        self?.busy = false
                        self?.status = "Deleted \(entry.name) on the server"
                    }
                }
            } catch {
                await self?.actionFailed("Delete vault on server", error)
            }
        }
    }

    /// Forget a vault on this device: entry, File Provider domain, cache
    /// directory, item database, and key. The vault stays on the server;
    /// connecting again needs the passphrase. A marker in UserDefaults makes
    /// the teardown resumable if the app dies half-way.
    func removeVaultLocally(_ vaultID: String) async {
        guard let entry = entries.first(where: { $0.vaultID == vaultID }) else { return }
        markRemoval(vaultID, pending: true)
        entries.removeAll { $0.vaultID == vaultID }
        vaultStates.removeValue(forKey: vaultID)
        defaults.removeObject(forKey: Self.lastSyncedKey(vaultID))
        if activeVaultID == vaultID {
            activeVaultID = entries.first?.vaultID ?? ""
            clearActiveDetail()
        }
        Self.saveEntries(entries, active: activeVaultID, to: defaults)
        await Self.tearDownStorage(for: entry)
        markRemoval(vaultID, pending: false)
        status = "Removed \(entry.name) from this device"
    }

    /// Drop the loaded detail (conflicts, previews, failures) when the
    /// active vault changes.
    private func clearActiveDetail() {
        client = nil
        conflicts = []
        choices = [:]
        previews = [:]
        failures = []
        progress = nil
    }

    private static func tearDownStorage(for entry: VaultEntry) async {
        // The domain first: removing it stops the extension's enumerators
        // before their files disappear. Errors are logged; the launch-time
        // reconcile drops any domain whose entry is gone.
        let domain = fpDomain(for: entry)
        let removal: Error? = await withCheckedContinuation { continuation in
            NSFileProviderManager.remove(domain, mode: .removeAll) { _, error in
                continuation.resume(returning: error)
            }
        }
        if let removal {
            NSLog("ObSink: File Provider domain removal for \(entry.vaultID): \(removal.localizedDescription)")
        }
        ItemStore.forget(vaultID: entry.vaultID)
        let fm = FileManager.default
        try? fm.removeItem(at: FileProviderPaths.vaultsBase.appendingPathComponent(entry.vaultID, isDirectory: true))
        let db = ItemStore.defaultDatabaseURL(vaultID: entry.vaultID)
        for suffix in ["", "-wal", "-shm"] {
            try? fm.removeItem(at: URL(fileURLWithPath: db.path + suffix))
        }
        KeychainStore.delete(account: entry.vaultID)
    }

    private static let pendingRemovalsKey = "pendingVaultRemovals"

    private func markRemoval(_ vaultID: String, pending: Bool) {
        var ids = Set(defaults.stringArray(forKey: Self.pendingRemovalsKey) ?? [])
        if pending { ids.insert(vaultID) } else { ids.remove(vaultID) }
        defaults.set(Array(ids), forKey: Self.pendingRemovalsKey)
    }

    /// Finish removals a previous run started: the entry is already gone, so
    /// only the on-disk and keychain parts remain.
    private func finishPendingRemovals() {
        let ids = defaults.stringArray(forKey: Self.pendingRemovalsKey) ?? []
        guard !ids.isEmpty else { return }
        Task { @MainActor [weak self] in
            for id in ids {
                await Self.tearDownStorage(for: VaultEntry(serverURL: "", vaultID: id, name: ""))
                self?.markRemoval(id, pending: false)
            }
        }
    }

    /// A bearer call returned 401: the session is gone (revoked, expired,
    /// account deleted). Forget the bearer; the account section offers
    /// `Sign in`.
    func handleUnauthorized() {
        KeychainStore.deleteBearer(serverURL: serverURL)
        hasBearer = false
        sessionExpired = true
        account = nil
        invites = []
        issuedInvite = nil
    }

    /// After the sign-in sheet closes: pick up a new bearer, if any.
    func reloadBearerState() {
        hasBearer = KeychainStore.loadBearer(serverURL: serverURL) != nil
        if hasBearer {
            sessionExpired = false
            // A card that failed on the old session starts clean.
            for (id, state) in vaultStates {
                if case .error = state.phase { vaultStates[id]?.phase = .idle }
            }
            refreshAccount()
            checkStale()
        }
    }

    private func actionFailed(_ action: String, _ error: Error) {
        busy = false
        if error.isUnauthorized {
            handleUnauthorized()
            status = "Error: \(MobileError.sessionExpiredMessage)"
        } else {
            alert = AppAlert(title: action, message: error.obsinkMessage)
        }
    }

    /// `412 MiB of 1 GiB` for one vault, `412 MiB` when the server sets no
    /// cap; nil until the account is known or when it does not own the vault.
    func vaultUsageText(for vaultID: String) -> String? {
        guard !vaultID.isEmpty, let usage = accountUsage,
              let entry = usage.vaults.first(where: { $0.id == vaultID }) else { return nil }
        let used = Self.formatBytes(entry.bytes)
        guard let cap = usage.maxVaultBytes else { return used }
        return "\(used) of \(Self.formatBytes(cap))"
    }

    /// Binary units (`1.2 MiB`), as DESIGN.md §5 requires and the desktop
    /// prints; `ByteCountFormatter` would say "Zero KB" and "1 GB".
    nonisolated static func formatBytes(_ value: UInt64) -> String {
        if value < 1024 { return "\(value) B" }
        let units = ["KiB", "MiB", "GiB", "TiB"]
        var size = Double(value) / 1024
        var unit = 0
        while size >= 1024 && unit < units.count - 1 {
            size /= 1024
            unit += 1
        }
        return String(format: size < 10 ? "%.1f %@" : "%.0f %@", size, units[unit])
    }

    /// "12 MiB used · 2 of 10 vaults · 1.0 GiB per vault", or nil when unknown.
    var usageText: String? {
        guard let usage = accountUsage else { return nil }
        var parts = [Self.formatBytes(usage.totalBytes) + " used"]
        if let maxVaults = usage.maxVaults {
            parts.append("\(usage.vaults.count) of \(maxVaults) vaults")
        } else {
            parts.append("\(usage.vaults.count) vaults")
        }
        if let perVault = usage.maxVaultBytes {
            parts.append(Self.formatBytes(perVault) + " per vault")
        }
        return parts.joined(separator: " · ")
    }

    /// Sign out of the active vault's server: revoke the session (best effort)
    /// and forget the bearer. Vault entries stay; sync needs a sign-in.
    func signOut() {
        let url = serverURL
        if let token = KeychainStore.loadBearer(serverURL: url) {
            Task.detached { try? authLogout(serverUrl: url, token: token) }
        }
        KeychainStore.deleteBearer(serverURL: url)
        account = nil
        invites = []
        issuedInvite = nil
        hasBearer = false
        sessionExpired = false
        accountNotice = "Signed out."
    }

    /// Make a vault the one whose detail (conflicts, failures, last result)
    /// is loaded. Each vault has its own directory, item database, File
    /// Provider domain and `VaultState`; this only moves the detail.
    func selectVault(_ id: String) {
        guard entries.contains(where: { $0.vaultID == id }), id != activeVaultID else { return }
        activeVaultID = id
        Self.saveEntries(entries, active: activeVaultID, to: defaults)
        clearActiveDetail()
        status = "Not synced"
    }

    /// Add (or replace) a vault and make it active.
    func addVault(_ entry: VaultEntry) {
        if let idx = entries.firstIndex(where: { $0.vaultID == entry.vaultID }) {
            entries[idx] = entry
        } else {
            entries.append(entry)
        }
        vaultStates[entry.vaultID] = freshState(for: entry)
        activeVaultID = entry.vaultID
        Self.saveEntries(entries, active: activeVaultID, to: defaults)
        clearActiveDetail()
        syncFileProviderDomains()
        status = "Added vault \(entry.name)"
        refreshAccount()
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

    // MARK: Sync state helpers

    /// Count of File-Provider-queued local changes (pendingUpload/pendingDeletion)
    /// for one vault, read from its item DB; the card shows `n to upload`.
    func refreshPending(for vaultID: String) {
        guard vaultStates[vaultID] != nil else { return }
        vaultStates[vaultID]?.pendingLocal = (try? ItemStore.store(for: vaultID).pendingCount()) ?? 0
    }

    func refreshAllPending() {
        for id in vaultStates.keys { refreshPending(for: id) }
    }

    /// Whether a derived key is already in the Keychain for a vault (so sync
    /// can run without re-entering the passphrase).
    func refreshStoredKey(for vaultID: String) {
        guard vaultStates[vaultID] != nil else { return }
        vaultStates[vaultID]?.hasStoredKey = KeychainStore.load(account: vaultID) != nil
    }

    /// Directory the Rust core reads/writes for a vault; Obsidian (via that
    /// vault's File Provider domain) sees the same files.
    static func vaultDirectory(for vaultID: String) -> URL {
        FileProviderPaths.vaultRoot(vaultID: vaultID)
    }

    /// Builds before per-vault storage kept every vault in one `Vault/` dir
    /// and one `obsink.sqlite`. On the first launch after the update, move
    /// that content under the active vault so nothing is lost; the other
    /// vaults re-download into their own directories on their next sync.
    static func migrateLegacyStorage(activeVaultID: String, defaults: UserDefaults) {
        let flag = "storageLayoutV2"
        guard !defaults.bool(forKey: flag) else { return }
        defer { defaults.set(true, forKey: flag) }
        let fm = FileManager.default
        let base = FileProviderPaths.vaultsBase
        let legacyDB = ItemStore.legacyDatabaseURL()
        guard !activeVaultID.isEmpty else { return }
        let target = base.appendingPathComponent(activeVaultID, isDirectory: true)
        if let children = try? fm.contentsOfDirectory(atPath: base.path), !children.isEmpty,
           !fm.fileExists(atPath: target.path) {
            try? fm.createDirectory(at: target, withIntermediateDirectories: true)
            for child in children where child != activeVaultID {
                try? fm.moveItem(at: base.appendingPathComponent(child), to: target.appendingPathComponent(child))
            }
        }
        for suffix in ["", "-wal", "-shm"] {
            let from = URL(fileURLWithPath: legacyDB.path + suffix)
            let to = URL(fileURLWithPath: ItemStore.defaultDatabaseURL(vaultID: activeVaultID).path + suffix)
            if fm.fileExists(atPath: from.path), !fm.fileExists(atPath: to.path) {
                try? fm.moveItem(at: from, to: to)
            }
        }
    }

    /// Run a full cycle for one vault. The vault becomes the active one so
    /// its conflicts and result are the loaded detail. One sync at a time.
    func sync(vaultID: String, passphrase: String) {
        guard !busy, let entry = entries.first(where: { $0.vaultID == vaultID }),
              isOnDefaultServer(entry) else { return }
        selectVault(vaultID)
        busy = true
        status = "Syncing…"
        conflicts = []
        progress = nil
        failures = []
        vaultStates[vaultID]?.phase = .syncing

        let config = MobileVaultConfig(
            serverUrl: entry.serverURL,
            apiKey: bearer,
            vaultId: vaultID,
            localPath: Self.vaultDirectory(for: vaultID).path
        )
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
                        ]), vaultID: vaultID)
                        return
                    }
                    key = try deriveMasterKey(passphrase: passphrase, vaultId: vaultID)
                    KeychainStore.save(key, account: vaultID)
                }
                await MainActor.run { self.refreshStoredKey(for: vaultID) }
                let client = try VaultClient(config: config, key: key)
                let outcome = try client.sync(listener: listener)
                await self.apply(outcome: outcome, client: client, vaultID: vaultID)
            } catch {
                await self.fail(error, vaultID: vaultID)
            }
        }
    }

    func resolve() {
        guard let client, !busy else { return }
        let vaultID = activeVaultID
        busy = true
        status = "Resolving…"
        progress = nil
        failures = []
        vaultStates[vaultID]?.phase = .resolving
        let resolutions = conflicts.map { conflict in
            MobileResolution(path: conflict.path, choice: choices[conflict.path] ?? .keepLocal)
        }
        let listener = SyncProgressListener(model: self)
        Task.detached {
            do {
                let outcome = try client.complete(resolutions: resolutions, listener: listener)
                await self.apply(outcome: outcome, client: client, vaultID: vaultID)
            } catch {
                await self.fail(error, vaultID: vaultID)
            }
        }
    }

    private func apply(outcome: SyncOutcome, client: VaultClient, vaultID: String) {
        self.client = client
        conflicts = outcome.conflicts
        choices = Dictionary(uniqueKeysWithValues: outcome.conflicts.map { ($0.path, .keepLocal) })
        previews = [:]
        failures = outcome.failures
        progress = nil
        busy = false
        let now = Date()
        vaultStates[vaultID]?.apply(outcome: outcome, now: now)
        let failedSuffix = outcome.failures.isEmpty
            ? ""
            : " · \(outcome.failures.count) failed"
        if outcome.completed {
            status = "Synced · ↑\(outcome.uploaded) ↓\(outcome.downloaded)\(failedSuffix)"
            defaults.set(now, forKey: Self.lastSyncedKey(vaultID))
            // OBS-20/21: mirror the freshly synced vault into the item DB, then
            // tell the File Provider to re-enumerate so Obsidian/Files see it.
            // OBS-22/23: the core sync already pushed uploads/deletes by
            // scanning the vault dir; clear the FP's pending flags now.
            if let store = try? ItemStore.store(for: vaultID) {
                try? store.reconcileAfterSync(completed: true, vaultRoot: Self.vaultDirectory(for: vaultID))
                try? store.drainPendingAfterSync(completed: true)
            }
            signalFileProvider(for: vaultID)
            refreshPending(for: vaultID)
        } else if !outcome.conflicts.isEmpty {
            let count = outcome.conflicts.count
            status = count == 1 ? "1 conflict needs attention" : "\(count) conflicts need attention"
            // The prepare step already applied the non-conflicting downloads
            // to disk, so the item DB and the File Provider must see them now;
            // the pending flags stay until the uploads run.
            if let store = try? ItemStore.store(for: vaultID) {
                try? store.reconcile(vaultRoot: Self.vaultDirectory(for: vaultID))
            }
            signalFileProvider(for: vaultID)
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

    /// One File Provider domain per vault (spec §11): the identifier is the
    /// vault ID (which picks the directory and database in the extension) and
    /// the display name is what Files and Obsidian show.
    static func fpDomain(for entry: VaultEntry) -> NSFileProviderDomain {
        let domain = NSFileProviderDomain(
            identifier: NSFileProviderDomainIdentifier(rawValue: entry.vaultID),
            displayName: entry.name.isEmpty ? "ObSink" : entry.name
        )
        #if targetEnvironment(simulator)
        // The simulator keeps third-party domains user-disabled (FP error
        // -2011) with no UI to enable them; testing modes force the domain on.
        // Simulator builds only — on device the user enables it in Files.
        domain.testingModes = [.alwaysEnabled, .interactive]
        #endif
        return domain
    }

    /// Make the registered domains match the configured vaults: add missing
    /// ones, drop the ones whose vault is gone (including the single
    /// "obsink" domain from before per-vault storage). Adding an
    /// already-registered domain is a no-op, so this is safe on every launch.
    private func syncFileProviderDomains() {
        let wanted = entries
        let reset = resetFileProviderDomain
        resetFileProviderDomain = false
        NSFileProviderManager.getDomainsWithCompletionHandler { registered, error in
            if let error {
                NSLog("ObSink: could not list File Provider domains: \(error.localizedDescription)")
            }
            let wantedIDs = Set(wanted.map(\.vaultID))
            // UI-test reset: domain state survives app reinstall, so drop
            // every domain before re-adding to start from a clean slate.
            let stale = registered.filter { reset || !wantedIDs.contains($0.identifier.rawValue) }
            let group = DispatchGroup()
            for domain in stale {
                group.enter()
                NSFileProviderManager.remove(domain) { _ in group.leave() }
            }
            group.notify(queue: .main) {
                for entry in wanted {
                    NSFileProviderManager.add(Self.fpDomain(for: entry)) { error in
                        if let error {
                            NSLog("ObSink: File Provider domain registration failed for \(entry.vaultID): \(error.localizedDescription)")
                        }
                    }
                }
            }
        }
    }

    /// Ask the system to re-enumerate a vault's working set and root so the
    /// File Provider picks up the DB changes from a reconcile. Errors are
    /// ignored: on a fresh install the domain registration may still be in
    /// flight.
    private func signalFileProvider(for vaultID: String) {
        guard let entry = entries.first(where: { $0.vaultID == vaultID }),
              let manager = NSFileProviderManager(for: Self.fpDomain(for: entry)) else { return }
        manager.signalEnumerator(for: .workingSet) { _ in }
        manager.signalEnumerator(for: .rootContainer) { _ in }
    }

    // MARK: Stale-vault warning (spec §3.4, OBS-33)

    /// Compare each vault's local working manifest against the server without
    /// syncing, one vault after another (each call blocks a thread). Only
    /// vaults on this build's server with a stored key take part; a 401 on
    /// any of them ends the session once.
    func checkStale() {
        guard !busy else { return }
        let bearer = self.bearer
        let targets: [(String, MobileVaultConfig, Data)] = entries.compactMap { entry in
            guard isOnDefaultServer(entry), let key = KeychainStore.load(account: entry.vaultID) else { return nil }
            let config = MobileVaultConfig(
                serverUrl: entry.serverURL,
                apiKey: bearer,
                vaultId: entry.vaultID,
                localPath: Self.vaultDirectory(for: entry.vaultID).path
            )
            return (entry.vaultID, config, key)
        }
        guard !targets.isEmpty else { return }
        Task.detached { [weak self] in
            for (vaultID, config, key) in targets {
                // Retry a couple of times: a transient network error on open
                // would otherwise silently suppress the warning until the next
                // foreground.
                var status: MobileVaultStatus?
                var unauthorized = false
                for attempt in 1...3 {
                    do {
                        status = try VaultClient(config: config, key: key).vaultStatus()
                        break
                    } catch {
                        if error.isUnauthorized {
                            unauthorized = true
                            break
                        }
                        NSLog("ObSink: stale check attempt %d failed: %@", attempt, error.obsinkMessage)
                        try? await Task.sleep(nanoseconds: 2_000_000_000)
                    }
                }
                let stop = await MainActor.run { [weak self] () -> Bool in
                    guard let self else { return true }
                    if unauthorized {
                        self.handleUnauthorized()
                        return true
                    }
                    guard let status, let current = self.vaultStates[vaultID],
                          current.phase == .idle else { return false }
                    // Conflicts already loaded for the active vault are the
                    // authoritative count until they are resolved.
                    let keepConflicts = vaultID == self.activeVaultID && !self.conflicts.isEmpty
                    var next = current
                    next.apply(status: status)
                    if keepConflicts { next.conflicts = current.conflicts }
                    self.vaultStates[vaultID] = next
                    return false
                }
                if stop { return }
            }
        }
    }

    private func fail(_ error: Error, vaultID: String) {
        busy = false
        progress = nil
        if error.isUnauthorized {
            handleUnauthorized()
        }
        vaultStates[vaultID]?.phase = .error(error.obsinkMessage)
        status = "Error: \(error.obsinkMessage)"
    }
}

/// A failed account or vault action, shown as an alert.
struct AppAlert: Identifiable {
    let id = UUID()
    let title: String
    let message: String
}
