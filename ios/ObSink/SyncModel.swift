import FileProvider
import Foundation
import UIKit

/// One vault this device holds (spec §10). Its key lives in the Keychain
/// under `account = vaultID`. Entries written before wire format v3 carried
/// a server URL (`serverURL` or the older `workerURL`); both are ignored on
/// read, since every build talks to one server.
struct VaultEntry: Codable, Identifiable, Equatable {
    var vaultID: String
    var name: String
    var id: String { vaultID }

    init(vaultID: String, name: String) {
        self.vaultID = vaultID
        self.name = name
    }

    private enum CodingKeys: String, CodingKey {
        case vaultID, name
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

/// Bridges Rust sync progress events into `SyncModel.progress`, and keeps
/// the per-file transfers of the cycle for the activity log. `onProgress`
/// fires on the sync's background thread (during the blocking Rust call),
/// so it hops to the main actor to update SwiftUI. Lives for one cycle.
final class SyncProgressListener: ProgressListener, @unchecked Sendable {
    private weak var model: SyncModel?
    private var phase: String = "Working"
    private let lock = NSLock()
    private var transfers: [(path: String, kind: MobileActionKind)] = []

    init(model: SyncModel) { self.model = model }

    /// The files this cycle moved, in order.
    var completedTransfers: [(path: String, kind: MobileActionKind)] {
        lock.lock()
        defer { lock.unlock() }
        return transfers
    }

    func onProgress(event: MobileProgressEvent) {
        let info: SyncProgressInfo?
        switch event {
        case .phase(let p):
            phase = SyncProgressInfo.label(for: p)
            info = SyncProgressInfo(phase: phase, current: 0, total: 0, path: nil)
        case .fileStarted(let path, let kind, let index, let total):
            lock.lock()
            transfers.append((path, kind))
            lock.unlock()
            info = SyncProgressInfo(phase: phase, current: Int(index), total: Int(total), path: path)
        case .done:
            info = nil
        case .fileCompleted, .fileFailed:
            return
        }
        Task { @MainActor [weak model] in model?.progress = info }
    }
}

/// Drives the app from the SwiftUI layer by calling the Rust core through
/// the generated UniFFI bindings (`VaultClient`, `createVault`, ...).
///
/// Vault files live in the shared App Group container, one directory and
/// one item database per vault (`Vault/<vaultID>/`, `items-<vaultID>.sqlite`),
/// each exposed through its own File Provider domain so the extension can
/// serve the same data. The entries persist in the group's UserDefaults;
/// the bearer, the user id, the account key, the device id and the vault
/// keys live in the Keychain (spec §6.3). No passphrase is ever stored.
@MainActor
final class SyncModel: ObservableObject {
    static let appGroup = "group.com.obsink.shared"
    /// Spec §6.1: the wrapped account key is the new exposure, so the
    /// passphrase has a floor (the same one every client enforces).
    static let minPassphraseChars = 12

    /// The vaults this device holds.
    @Published var entries: [VaultEntry] = []
    /// The account's vaults as the server last listed them; nil until it
    /// answered (signed out, offline, locked).
    @Published var serverVaults: [MobileVaultSummary]?
    /// The merged list the Vaults tab shows (spec §15.1).
    @Published var rows: [VaultRow] = []
    /// The vault whose conflicts, failures and last result are loaded: the
    /// one synced most recently, or the one the user acted on. Every vault
    /// has its own `VaultState`; this only decides where the detail lives.
    @Published var detailVaultID: String = ""
    /// Per-vault state for the cards, keyed by vault id.
    @Published var vaultStates: [String: VaultState] = [:]
    /// The one-time "Open in Obsidian" card has been dismissed.
    @Published var guidanceDismissed: Bool
    /// Outcome of an account action (`Signed out`, `Account deleted`, ...),
    /// shown on the Settings and Devices tabs.
    @Published var accountNotice: String?

    /// The one server this build talks to (`ServerConfig`).
    var serverURL: String { ServerConfig.defaultURL }
    /// The account on that server (`GET /auth/me`); nil when signed out.
    @Published var account: MobileAccount?
    /// Invites this account minted, newest first.
    @Published var invites: [MobileInvite] = []
    /// The most recently minted invite code, shown until dismissed.
    @Published var issuedInvite: MobileInvite?
    @Published var hasBearer: Bool = false
    /// Signed in, but the account key is not at hand (spec §12.1): no key
    /// in the Keychain, or one from a lost first-set race.
    @Published var locked: Bool = false
    /// The server holds a passphrase: the form is `Unlock`, else `Set
    /// passphrase`.
    @Published var hasServerKey: Bool = false
    /// A bearer call came back 401: the bearer is gone and the account section
    /// offers `Sign in`.
    @Published var sessionExpired: Bool = false
    /// Spec §15.5: the server speaks another wire format.
    @Published var protocolMismatch: Bool = false
    @Published var serverProtocol: UInt32?
    /// One-off failure of an account or vault action (revoke, delete, remove).
    @Published var alert: AppAlert?

    var accountEmail: String? { account?.email }
    var accountUsage: MobileUsage? { account?.usage }
    /// The signed-in user id (Keychain), the account key's owner.
    var userID: String? { KeychainStore.loadUserID(serverURL: serverURL) }
    /// This phone's device id (spec §4.1), minted on first use.
    var deviceID: String { KeychainStore.loadOrCreateDeviceID(serverURL: serverURL) { newDeviceId() } }

    /// The detail vault's last result, as a sentence (`Synced · ↑n ↓n`,
    /// `Created vault X`, `Error: …`). Shown on that vault's card.
    @Published var status: String = "Not synced"
    @Published var busy: Bool = false
    /// The live model, for the background refresh handler.
    static weak var shared: SyncModel?
    /// Set by the background task's expiration handler; the auto-sync
    /// routine stops before its next vault.
    var cancelRequested = false
    private var lastAutoSyncAttempt: Date?
    /// Resumed by `apply`/`fail` so `syncAndWait` can run vaults in turn.
    private var syncCompletion: CheckedContinuation<Void, Never>?
    @Published var conflicts: [MobileConflict] = []
    @Published var choices: [String: MobileChoice] = [:]
    @Published var previews: [String: MobileConflictPreview] = [:]
    @Published var progress: SyncProgressInfo?
    @Published var failures: [MobileSyncFailure] = []
    /// The last sync moved its files but could not write the checkpoint.
    @Published var checkpointError: String?

    private var client: VaultClient?
    private let defaults: UserDefaults
    private var resetFileProviderDomain = false

    init() {
        let defaults = UserDefaults(suiteName: Self.appGroup) ?? .standard
        self.defaults = defaults

        // UI-test hooks (inert unless the harness sets them): OBSINK_UITEST_SEED
        // carries a JSON [VaultEntry] to start from a known state without
        // driving the UI; OBSINK_UITEST_BEARER (+ _URL, _USER_ID) seeds the
        // session the way a sign-in would; OBSINK_UITEST_ACCOUNT_KEY (hex) +
        // _ACCOUNT_KEY_ID seed the unlocked account key; OBSINK_UITEST_VAULT_KEY
        // (hex, for the seeded vault) skips the download.
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
            Self.saveEntries(seeded, to: defaults)
            if let hex = env["OBSINK_UITEST_VAULT_KEY"], let key = Data(hexString: hex), let first = seeded.first {
                KeychainStore.save(key, account: first.vaultID)
            }
        }
        if let bearer = env["OBSINK_UITEST_BEARER"], let url = env["OBSINK_UITEST_BEARER_URL"],
           !bearer.isEmpty, !url.isEmpty {
            KeychainStore.saveBearer(bearer, serverURL: url)
            if let userID = env["OBSINK_UITEST_USER_ID"], !userID.isEmpty {
                KeychainStore.saveUserID(userID, serverURL: url)
                if let hex = env["OBSINK_UITEST_ACCOUNT_KEY"], let key = Data(hexString: hex),
                   let keyID = env["OBSINK_UITEST_ACCOUNT_KEY_ID"], !keyID.isEmpty {
                    KeychainStore.saveAccountKey(key, keyID: keyID, userID: userID)
                }
            }
        }

        self.entries = Self.loadEntries(from: defaults)
        self.detailVaultID = entries.first?.vaultID ?? ""

        Self.migrateLegacyStorage(defaults: defaults)
        migrateKeychainAccessibility()
        rebuildVaultStates()
        rebuildRows()
        loadBearerState()
        syncFileProviderDomains()
        finishPendingRemovals()
        checkProtocol()
        Self.shared = self
    }

    /// Items saved before OBS-107 are `WhenUnlocked`; the background refresh
    /// runs with the device locked, so re-save them once as
    /// `AfterFirstUnlock`.
    private func migrateKeychainAccessibility() {
        let flag = "keychainAfterFirstUnlock"
        guard !defaults.bool(forKey: flag) else { return }
        var accounts = entries.map(\.vaultID)
        accounts.append(KeychainStore.bearerAccount(for: serverURL))
        let ok = accounts.allSatisfy { KeychainStore.resave(account: $0) }
        if ok { defaults.set(true, forKey: flag) } else { NSLog("ObSink: keychain accessibility migration incomplete") }
    }

    /// Bearer for this build's server (Keychain), empty when signed out.
    var bearer: String {
        KeychainStore.loadBearer(serverURL: serverURL) ?? ""
    }

    /// This phone as the server knows it at sign-in.
    nonisolated func thisDevice() -> MobileDeviceIdentity {
        let url = ServerConfig.defaultURL
        return MobileDeviceIdentity(id: KeychainStore.loadOrCreateDeviceID(serverURL: url) { newDeviceId() }, name: UIDeviceName.current)
    }

    /// Whether this device can sync the vault: it holds the key, the server
    /// still lists it, and the session is live.
    func canSync(_ vaultID: String) -> Bool {
        guard let state = vaultStates[vaultID] else { return false }
        return state.hasStoredKey && !state.deletedOnServer && !sessionExpired && hasBearer
    }

    private static let guidanceDismissedKey = "guidanceDismissed"
    private static let lastSyncedPrefix = "lastSynced."

    private static func lastSyncedKey(_ vaultID: String) -> String { lastSyncedPrefix + vaultID }

    /// Build every vault's state from what is on the device: key present,
    /// last sync time, pending File Provider writes.
    private func rebuildVaultStates() {
        var states: [String: VaultState] = [:]
        for entry in entries {
            var state = freshState(for: entry)
            state.deletedOnServer = vaultStates[entry.vaultID]?.deletedOnServer ?? false
            states[entry.vaultID] = state
        }
        vaultStates = states
    }

    private func freshState(for entry: VaultEntry) -> VaultState {
        var state = VaultState()
        state.hasStoredKey = KeychainStore.load(account: entry.vaultID) != nil
        state.lastSyncedAt = defaults.object(forKey: Self.lastSyncedKey(entry.vaultID)) as? Date
        state.pendingLocal = (try? ItemStore.store(for: entry.vaultID).pendingCount()) ?? 0
        return state
    }

    /// The merged list (spec §15.1), and the `deletedOnServer` flag of the
    /// stored vaults the server no longer lists.
    private func rebuildRows() {
        rows = VaultRow.merge(entries: entries, server: serverVaults)
        if let server = serverVaults {
            for entry in entries {
                vaultStates[entry.vaultID]?.deletedOnServer = !server.contains { $0.id == entry.vaultID }
            }
        }
    }

    /// Pick up the bearer (if any) and the account behind it.
    private func loadBearerState() {
        hasBearer = KeychainStore.loadBearer(serverURL: serverURL) != nil
        account = nil
        invites = []
        issuedInvite = nil
        serverVaults = nil
        rebuildRows()
        refreshAccount()
    }

    func dismissGuidance() {
        guidanceDismissed = true
        defaults.set(true, forKey: Self.guidanceDismissedKey)
    }

    /// Spec §15.5: `GET /` once per launch; a mismatch replaces every screen.
    private func checkProtocol() {
        let url = serverURL
        Task.detached { [weak self] in
            guard let caps = try? authCapabilities(serverUrl: url) else { return }
            await MainActor.run { [weak self] in
                self?.serverProtocol = caps.protocol
                self?.protocolMismatch = caps.protocol != 0 && caps.protocol != mobileProtocolVersion()
            }
        }
    }

    /// Resolve the account behind the bearer, the key state and the vault
    /// list. A 401 means the session is gone.
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
                    KeychainStore.saveUserID(account.userId, serverURL: url)
                    Task { @MainActor [weak self] in
                        await self?.refreshKeyState()
                        await self?.refreshVaultList()
                    }
                    self.refreshInvites()
                case .failure(let error) where error.isUnauthorized:
                    self.handleUnauthorized()
                case .failure(let error) where (error as? MobileError)?.isProtocolMismatch == true:
                    self.protocolMismatch = true
                case .failure:
                    self.account = nil
                }
            }
        }
    }

    /// Unlocked means the Keychain holds the account's key: the one the
    /// server reports, not one from a lost first-set race.
    func refreshKeyState() async {
        guard let token = KeychainStore.loadBearer(serverURL: serverURL), let userID else {
            locked = false
            return
        }
        let url = serverURL
        do {
            let blob = try await Task.detached { try authGetKeys(serverUrl: url, token: token) }.value
            hasServerKey = blob != nil
            let stored = KeychainStore.loadAccountKey(userID: userID)
            locked = !(stored != nil && blob?.keyId == stored?.keyID)
        } catch {
            if error.isUnauthorized { handleUnauthorized() }
        }
    }

    /// `GET /vaults`: the account's vaults, merged into the rows.
    func refreshVaultList() async {
        guard let token = KeychainStore.loadBearer(serverURL: serverURL), !locked else {
            serverVaults = nil
            rebuildRows()
            return
        }
        let url = serverURL
        do {
            let listed = try await Task.detached { try listVaults(serverUrl: url, bearer: token) }.value
            serverVaults = listed
            // A rename elsewhere reaches the stored entry.
            for summary in listed {
                if let index = entries.firstIndex(where: { $0.vaultID == summary.id }), entries[index].name != summary.name {
                    entries[index].name = summary.name
                    Self.saveEntries(entries, to: defaults)
                }
            }
        } catch {
            if error.isUnauthorized { handleUnauthorized() }
        }
        rebuildRows()
    }

    /// Pull-to-refresh on the Vaults tab.
    func refreshAll() async {
        refreshAllPending()
        refreshAccount()
        await refreshVaultList()
        await checkStale()
    }

    /// Invites this account minted (`GET /auth/invites`). Failures keep the
    /// old list.
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

    // MARK: Sign-in and the account key (spec §12.1)

    /// After a sign-in: keep the bearer and the user id; the passphrase step
    /// follows in the sheet.
    func signedIn(session: MobileSession) {
        KeychainStore.saveBearer(session.token, serverURL: serverURL)
        KeychainStore.saveUserID(session.userId, serverURL: serverURL)
        hasBearer = true
        sessionExpired = false
        accountNotice = nil
    }

    /// Set the passphrase (create-only). `.mismatch` means another device
    /// set it first with another passphrase: the caller asks for it.
    func setPassphrase(_ passphrase: String) async throws -> MobileKeysOutcome {
        guard let token = KeychainStore.loadBearer(serverURL: serverURL), let userID else {
            throw MobileError.Sync(message: "Sign in first.")
        }
        let url = serverURL
        let result = try await Task.detached {
            try authSetPassphrase(serverUrl: url, token: token, userId: userID, passphrase: passphrase)
        }.value
        hasServerKey = true
        switch result.outcome {
        case .created, .exists:
            KeychainStore.saveAccountKey(result.key, keyID: result.keyId, userID: userID)
            locked = false
            accountNotice = result.outcome == .created
                ? "Passphrase set. There is no recovery if it is lost."
                : "A passphrase was already set on another device; it matched."
            await unlocked()
        case .mismatch:
            locked = true
        }
        return result.outcome
    }

    /// Enter the passphrase on a phone that does not hold the account key.
    func unlock(_ passphrase: String) async throws {
        guard let token = KeychainStore.loadBearer(serverURL: serverURL), let userID else {
            throw MobileError.Sync(message: "Sign in first.")
        }
        let url = serverURL
        let key = try await Task.detached {
            try authUnlock(serverUrl: url, token: token, userId: userID, passphrase: passphrase)
        }.value
        KeychainStore.deleteAccountKey(userID: userID)
        KeychainStore.saveAccountKey(key.key, keyID: key.keyId, userID: userID)
        hasServerKey = true
        locked = false
        accountNotice = "Unlocked."
        await unlocked()
    }

    /// The account key is at hand: the vault list, and a stored vault whose
    /// key is missing can be downloaded again.
    private func unlocked() async {
        refreshAccount()
        await refreshVaultList()
        await checkStale()
    }

    /// DESIGN.md §5 `Change passphrase`.
    func changePassphrase(current: String, next: String) async throws {
        guard let token = KeychainStore.loadBearer(serverURL: serverURL), let userID,
              let stored = KeychainStore.loadAccountKey(userID: userID) else {
            throw MobileError.Sync(message: "Set a passphrase first.")
        }
        let url = serverURL
        try await Task.detached {
            try authChangePassphrase(serverUrl: url, token: token, userId: userID, accountKey: stored.key,
                                     current: current, next: next)
        }.value
    }

    /// The unlocked account key, or the message a vault action shows before
    /// the unlock (DESIGN.md §5).
    private func requireAccountKey() throws -> Data {
        guard let userID, let stored = KeychainStore.loadAccountKey(userID: userID) else {
            throw MobileError.Sync(message: "Set a passphrase first.")
        }
        return stored.key
    }

    // MARK: Devices (spec §15.3)

    /// Sign another device of this account out (`DELETE /auth/devices/:id`).
    func revokeDevice(_ deviceID: String) {
        guard let token = KeychainStore.loadBearer(serverURL: serverURL) else { return }
        let url = serverURL
        busy = true
        Task.detached { [weak self] in
            do {
                try authRevokeDevice(serverUrl: url, token: token, deviceId: deviceID)
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

    func renameDevice(_ deviceID: String, name: String) async throws {
        guard let token = KeychainStore.loadBearer(serverURL: serverURL) else {
            throw MobileError.Sync(message: "Sign in first.")
        }
        let url = serverURL
        try await Task.detached { try authRenameDevice(serverUrl: url, token: token, deviceId: deviceID, name: name) }.value
        accountNotice = "Device renamed."
        refreshAccount()
    }

    /// Delete the account and everything it owns on the server, then forget
    /// the bearer, the account key and every vault here (App Store 5.1.1(v)).
    /// Vault files on this device go with the vaults (they are the server's
    /// copies; the user asked for the account to be gone).
    func deleteAccount() {
        guard let token = KeychainStore.loadBearer(serverURL: serverURL) else { return }
        let url = serverURL
        let userID = self.userID
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
                    for entry in self.entries {
                        await self.removeVaultLocally(entry.vaultID, detach: false)
                    }
                    KeychainStore.deleteBearer(serverURL: url)
                    KeychainStore.delete(account: KeychainStore.userAccount(for: url))
                    if let userID { KeychainStore.deleteAccountKey(userID: userID) }
                    self.account = nil
                    self.invites = []
                    self.issuedInvite = nil
                    self.serverVaults = nil
                    self.hasBearer = false
                    self.locked = false
                    self.sessionExpired = false
                    self.busy = false
                    self.rebuildRows()
                    self.accountNotice = "Account deleted."
                }
            }
        }
    }

    /// Sign out of the server: revoke the session (best effort) and forget
    /// the bearer. Vault entries and keys stay; sync needs a sign-in.
    func signOut() {
        let url = serverURL
        if let token = KeychainStore.loadBearer(serverURL: url) {
            Task.detached { try? authLogout(serverUrl: url, token: token) }
        }
        KeychainStore.deleteBearer(serverURL: url)
        account = nil
        invites = []
        issuedInvite = nil
        serverVaults = nil
        hasBearer = false
        locked = false
        sessionExpired = false
        rebuildRows()
        accountNotice = "Signed out."
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
        serverVaults = nil
        rebuildRows()
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
            Task { await checkStale() }
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

    // MARK: Vaults: create, download, rename, remove, delete

    /// Spec §12.2: a fresh vault key wrapped under the account key, the vault
    /// on the server, this phone attached, then the first cycle.
    func createVault(name: String) async throws {
        guard let token = KeychainStore.loadBearer(serverURL: serverURL) else {
            throw MobileError.Sync(message: "Sign in first.")
        }
        let accountKey = try requireAccountKey()
        let url = serverURL, device = deviceID
        let created = try await Task.detached {
            try ObSink.createVault(serverUrl: url, bearer: token, deviceId: device, name: name, accountKey: accountKey)
        }.value
        KeychainStore.save(created.key, account: created.vault.id)
        adopt(VaultEntry(vaultID: created.vault.id, name: created.vault.name))
        status = "Created vault \(created.vault.name)"
        await refreshVaultList()
        await syncAndWait(vaultID: created.vault.id)
    }

    /// Spec §12.3 (`Download`): the vault key from the member blob, this
    /// phone attached, then the first cycle. Runs in place on the card.
    func download(vaultID: String) {
        guard !busy else { return }
        busy = true
        detailVaultID = vaultID
        status = "Downloading…"
        Task { @MainActor in
            do {
                try await downloadVault(vaultID: vaultID)
            } catch {
                busy = false
                if error.isUnauthorized { handleUnauthorized() }
                status = "Error: \(error.obsinkMessage)"
            }
        }
    }

    private func downloadVault(vaultID: String) async throws {
        guard let token = KeychainStore.loadBearer(serverURL: serverURL) else {
            throw MobileError.Sync(message: "Sign in first.")
        }
        let accountKey = try requireAccountKey()
        let url = serverURL
        let summary: MobileVaultSummary
        if let listed = serverVaults?.first(where: { $0.id == vaultID }) {
            summary = listed
        } else {
            let listed = try await Task.detached { try listVaults(serverUrl: url, bearer: token) }.value
            serverVaults = listed
            guard let found = listed.first(where: { $0.id == vaultID }) else {
                throw MobileError.Sync(message: "This vault is not one of the account's.")
            }
            summary = found
        }
        guard let wrapped = summary.wrappedKey else {
            throw MobileError.Sync(message: "This vault has no key for the account (it was created before the passphrase).")
        }
        let key = try await Task.detached { try unwrapVaultKey(accountKey: accountKey, wrapped: wrapped, vaultId: vaultID) }.value
        KeychainStore.save(key, account: vaultID)
        adopt(VaultEntry(vaultID: vaultID, name: summary.name))
        let config = self.config(for: vaultID)
        try await Task.detached { try attachDevice(config: config) }.value
        status = "Downloaded \(summary.name)"
        busy = false
        await refreshVaultList()
        await syncAndWait(vaultID: vaultID)
    }

    /// Keep (or replace) an entry and make it the detail vault.
    private func adopt(_ entry: VaultEntry) {
        if let idx = entries.firstIndex(where: { $0.vaultID == entry.vaultID }) {
            entries[idx] = entry
        } else {
            entries.append(entry)
        }
        vaultStates[entry.vaultID] = freshState(for: entry)
        detailVaultID = entry.vaultID
        Self.saveEntries(entries, to: defaults)
        clearDetail()
        rebuildRows()
        syncFileProviderDomains()
    }

    /// Spec §4.3 `PATCH /vaults/:id`; the entry and the list follow.
    func renameVault(vaultID: String, name: String) async throws {
        guard KeychainStore.loadBearer(serverURL: serverURL) != nil else {
            throw MobileError.Sync(message: "Sign in first.")
        }
        let config = self.config(for: vaultID)
        try await Task.detached { try ObSink.renameVault(config: config, name: name) }.value
        if let index = entries.firstIndex(where: { $0.vaultID == vaultID }) {
            entries[index].name = name
            Self.saveEntries(entries, to: defaults)
        }
        status = "Renamed to \(name)"
        await refreshVaultList()
    }

    /// Delete a vault on the server (`DELETE /vaults/:id`), then forget it here.
    func deleteVaultOnServer(_ vaultID: String) {
        guard let token = KeychainStore.loadBearer(serverURL: serverURL) else { return }
        let name = rows.first { $0.id == vaultID }?.name ?? vaultID
        let url = serverURL
        busy = true
        Task.detached { [weak self] in
            do {
                try deleteVault(serverUrl: url, bearer: token, vaultId: vaultID)
                await MainActor.run { [weak self] in
                    Task { @MainActor [weak self] in
                        guard let self else { return }
                        self.serverVaults?.removeAll { $0.id == vaultID }
                        await self.removeVaultLocally(vaultID, detach: false)
                        self.busy = false
                        self.status = "Deleted \(name) on the server"
                        await self.refreshVaultList()
                    }
                }
            } catch {
                await self?.actionFailed("Delete vault on server", error)
            }
        }
    }

    /// Forget a vault on this device: the server stops listing this phone
    /// for it, then the entry, File Provider domain, cache directory, item
    /// database, activity log and key go. The vault stays on the server and
    /// can be downloaded again. A marker in UserDefaults makes the teardown
    /// resumable if the app dies half-way.
    func removeVaultLocally(_ vaultID: String, detach: Bool = true) async {
        guard let entry = entries.first(where: { $0.vaultID == vaultID }) else { return }
        if detach, KeychainStore.loadBearer(serverURL: serverURL) != nil {
            let config = self.config(for: vaultID)
            // Best effort: the server learns on the next attach or sign-in.
            _ = try? await Task.detached { try detachDevice(config: config) }.value
        }
        markRemoval(vaultID, pending: true)
        entries.removeAll { $0.vaultID == vaultID }
        vaultStates.removeValue(forKey: vaultID)
        defaults.removeObject(forKey: Self.lastSyncedKey(vaultID))
        if detailVaultID == vaultID {
            detailVaultID = entries.first?.vaultID ?? ""
            clearDetail()
        }
        Self.saveEntries(entries, to: defaults)
        rebuildRows()
        await Self.tearDownStorage(for: entry)
        markRemoval(vaultID, pending: false)
        status = "Removed \(entry.name) from this device"
        await refreshVaultList()
    }

    /// Drop the loaded detail (conflicts, previews, failures) when the
    /// detail vault changes.
    private func clearDetail() {
        client = nil
        conflicts = []
        choices = [:]
        previews = [:]
        failures = []
        checkpointError = nil
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
        ActivityLog.forget(vaultID: entry.vaultID)
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
                await Self.tearDownStorage(for: VaultEntry(vaultID: id, name: ""))
                self?.markRemoval(id, pending: false)
            }
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

    /// Make a vault the one whose detail (conflicts, failures, last result)
    /// is loaded.
    private func selectDetail(_ id: String) {
        guard entries.contains(where: { $0.vaultID == id }), id != detailVaultID else { return }
        detailVaultID = id
        clearDetail()
        status = "Not synced"
    }

    // MARK: Persistence

    private static func loadEntries(from defaults: UserDefaults) -> [VaultEntry] {
        guard let data = defaults.data(forKey: "vaultEntries"),
              let entries = try? JSONDecoder().decode([VaultEntry].self, from: data) else {
            return []
        }
        return entries
    }

    private static func saveEntries(_ entries: [VaultEntry], to defaults: UserDefaults) {
        if let data = try? JSONEncoder().encode(entries) {
            defaults.set(data, forKey: "vaultEntries")
        }
        defaults.removeObject(forKey: "activeVaultID")
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

    /// Directory the Rust core reads/writes for a vault; Obsidian (via that
    /// vault's File Provider domain) sees the same files.
    static func vaultDirectory(for vaultID: String) -> URL {
        FileProviderPaths.vaultRoot(vaultID: vaultID)
    }

    /// The core's view of one vault on this phone: the bearer, the device
    /// id (so checkpoints report), the cache directory.
    func config(for vaultID: String) -> MobileVaultConfig {
        MobileVaultConfig(
            serverUrl: serverURL,
            bearer: bearer,
            vaultId: vaultID,
            localPath: Self.vaultDirectory(for: vaultID).path,
            deviceId: deviceID
        )
    }

    /// Builds before per-vault storage kept every vault in one `Vault/` dir
    /// and one `obsink.sqlite`. On the first launch after the update, move
    /// that content under the first vault so nothing is lost; the other
    /// vaults re-download into their own directories on their next sync.
    static func migrateLegacyStorage(defaults: UserDefaults) {
        let flag = "storageLayoutV2"
        guard !defaults.bool(forKey: flag) else { return }
        defer { defaults.set(true, forKey: flag) }
        let fm = FileManager.default
        let base = FileProviderPaths.vaultsBase
        let legacyDB = ItemStore.legacyDatabaseURL()
        guard let first = loadEntries(from: defaults).first?.vaultID else { return }
        let target = base.appendingPathComponent(first, isDirectory: true)
        if let children = try? fm.contentsOfDirectory(atPath: base.path), !children.isEmpty,
           !fm.fileExists(atPath: target.path) {
            try? fm.createDirectory(at: target, withIntermediateDirectories: true)
            for child in children where child != first {
                try? fm.moveItem(at: base.appendingPathComponent(child), to: target.appendingPathComponent(child))
            }
        }
        for suffix in ["", "-wal", "-shm"] {
            let from = URL(fileURLWithPath: legacyDB.path + suffix)
            let to = URL(fileURLWithPath: ItemStore.defaultDatabaseURL(vaultID: first).path + suffix)
            if fm.fileExists(atPath: from.path), !fm.fileExists(atPath: to.path) {
                try? fm.moveItem(at: from, to: to)
            }
        }
    }

    /// Run a full cycle for one vault with its stored key. The vault becomes
    /// the detail one so its conflicts and result are the loaded detail. One
    /// sync at a time.
    func sync(vaultID: String) {
        guard !busy, let entry = entries.first(where: { $0.vaultID == vaultID }),
              vaultStates[vaultID]?.deletedOnServer != true else { return }
        selectDetail(vaultID)
        busy = true
        status = "Syncing…"
        conflicts = []
        progress = nil
        failures = []
        checkpointError = nil
        vaultStates[vaultID]?.phase = .syncing

        let config = self.config(for: entry.vaultID)
        let listener = SyncProgressListener(model: self)

        Task.detached {
            do {
                guard let key = KeychainStore.load(account: vaultID) else {
                    await self.fail(MobileError.Sync(message: "The key for this vault is not on this device. Download it again."), vaultID: vaultID)
                    return
                }
                let client = try VaultClient(config: config, key: key)
                let outcome = try client.sync(listener: listener)
                await self.apply(outcome: outcome, client: client, vaultID: vaultID, listener: listener)
            } catch {
                await self.fail(error, vaultID: vaultID)
            }
        }
    }

    func resolve() {
        guard let client, !busy else { return }
        let vaultID = detailVaultID
        busy = true
        status = "Resolving…"
        progress = nil
        failures = []
        checkpointError = nil
        vaultStates[vaultID]?.phase = .resolving
        let resolutions = conflicts.map { conflict in
            MobileResolution(path: conflict.path, choice: choices[conflict.path] ?? .keepLocal)
        }
        let listener = SyncProgressListener(model: self)
        Task.detached {
            do {
                let outcome = try client.complete(resolutions: resolutions, listener: listener)
                await self.apply(outcome: outcome, client: client, vaultID: vaultID, listener: listener)
            } catch {
                await self.fail(error, vaultID: vaultID)
            }
        }
    }

    private func apply(outcome: SyncOutcome, client: VaultClient, vaultID: String, listener: SyncProgressListener) {
        defer { finishSyncWait() }
        self.client = client
        conflicts = outcome.conflicts
        choices = Dictionary(uniqueKeysWithValues: outcome.conflicts.map { ($0.path, .keepLocal) })
        previews = [:]
        failures = outcome.failures
        checkpointError = outcome.checkpointError
        progress = nil
        busy = false
        let now = Date()
        vaultStates[vaultID]?.apply(outcome: outcome, now: now)
        ActivityLog.record(vaultID: vaultID, transfers: listener.completedTransfers, now: now)
        ActivityLog.record(vaultID: vaultID, outcome: outcome, plan: nil, now: now)
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
        // The devices section: this phone's checkpoint just moved.
        Task { await refreshVaultList() }
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

    // MARK: Activity and history (spec §15.2, §8.2, §9.3)

    /// This vault's log, newest first.
    func activity(for vaultID: String) -> [ActivityEvent] {
        ActivityLog.list(vaultID: vaultID)
    }

    /// Every file in the vault's cache (for the history picker), sorted;
    /// the bookkeeping under `.obsink/` is not a file of the vault.
    func listFiles(for vaultID: String) -> [String] {
        let root = Self.vaultDirectory(for: vaultID)
        guard let enumerator = FileManager.default.enumerator(at: root, includingPropertiesForKeys: [.isRegularFileKey]) else {
            return []
        }
        var files: [String] = []
        for case let url as URL in enumerator {
            let relative = url.path.dropFirst(root.path.count + 1)
            if relative.hasPrefix(".obsink/") || relative.isEmpty { continue }
            if (try? url.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile) == true {
                files.append(String(relative))
            }
        }
        return files.sorted()
    }

    private func vaultKey(for vaultID: String) throws -> Data {
        guard let key = KeychainStore.load(account: vaultID) else {
            throw MobileError.Sync(message: "The key for this vault is not on this device. Download it again.")
        }
        return key
    }

    func versions(for vaultID: String, path: String) async throws -> [MobileVersion] {
        let config = self.config(for: vaultID), key = try vaultKey(for: vaultID)
        return try await Task.detached { try listVersions(config: config, key: key, path: path) }.value
    }

    /// The decrypted text of a version, or nil for bytes that are not UTF-8.
    func versionText(for vaultID: String, path: String, name: String) async throws -> String? {
        let config = self.config(for: vaultID), key = try vaultKey(for: vaultID)
        let bytes = try await Task.detached { try fetchVersion(config: config, key: key, path: path, name: name) }.value
        return Self.previewText(bytes)
    }

    /// Restore writes the version into the cache; the next sync uploads it
    /// through the ordinary conflict-gated path.
    func restoreVersion(vaultID: String, path: String, name: String) async throws {
        let config = self.config(for: vaultID), key = try vaultKey(for: vaultID)
        let bytes = try await Task.detached { try fetchVersion(config: config, key: key, path: path, name: name) }.value
        try writeRestored(vaultID: vaultID, path: path, bytes: bytes)
    }

    func trash(for vaultID: String) async throws -> [MobileTrashEntry] {
        let config = self.config(for: vaultID), key = try vaultKey(for: vaultID)
        return try await Task.detached { try listTrash(config: config, key: key) }.value
    }

    func trashText(for vaultID: String, path: String) async throws -> String? {
        let config = self.config(for: vaultID), key = try vaultKey(for: vaultID)
        let bytes = try await Task.detached { try fetchTrash(config: config, key: key, path: path) }.value
        return Self.previewText(bytes)
    }

    /// A restored deletion syncs with the tombstone as its parent: the base
    /// manifest still holds it, so the ordinary upload path presents its hash.
    func restoreTrash(vaultID: String, path: String) async throws {
        let config = self.config(for: vaultID), key = try vaultKey(for: vaultID)
        let bytes = try await Task.detached { try fetchTrash(config: config, key: key, path: path) }.value
        try writeRestored(vaultID: vaultID, path: path, bytes: bytes)
    }

    nonisolated static func previewText(_ bytes: Data) -> String? {
        guard let text = String(data: bytes, encoding: .utf8), !text.contains("\0") else { return nil }
        return text
    }

    private func writeRestored(vaultID: String, path: String, bytes: Data) throws {
        let root = Self.vaultDirectory(for: vaultID)
        guard let target = FileProviderPaths.url(forLocalPath: path, root: root), target.path != root.path else {
            throw MobileError.Sync(message: "Refusing the path \(path).")
        }
        try FileManager.default.createDirectory(at: target.deletingLastPathComponent(), withIntermediateDirectories: true)
        try bytes.write(to: target, options: .atomic)
        if let store = try? ItemStore.store(for: vaultID) {
            try? store.reconcile(vaultRoot: root)
        }
        signalFileProvider(for: vaultID)
        refreshPending(for: vaultID)
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

    /// Make the registered domains match the vaults on this device: add
    /// missing ones (a download), drop the ones whose vault is gone (a
    /// removal, including the single "obsink" domain from before per-vault
    /// storage). Adding an already-registered domain is a no-op, so this is
    /// safe on every launch.
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
    /// vaults with a stored key take part; a 401 on any of them ends the
    /// session once.
    func checkStale() async {
        guard !busy, hasBearer else { return }
        let targets: [(String, MobileVaultConfig, Data)] = entries.compactMap { entry in
            guard let key = KeychainStore.load(account: entry.vaultID),
                  vaultStates[entry.vaultID]?.deletedOnServer != true else { return nil }
            return (entry.vaultID, config(for: entry.vaultID), key)
        }
        guard !targets.isEmpty else { return }
        await Task.detached { [weak self] in
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
                    // Conflicts already loaded for the detail vault are the
                    // authoritative count until they are resolved.
                    let keepConflicts = vaultID == self.detailVaultID && !self.conflicts.isEmpty
                    var next = current
                    next.apply(status: status)
                    if keepConflicts { next.conflicts = current.conflicts }
                    self.vaultStates[vaultID] = next
                    return false
                }
                if stop { return }
            }
        }.value
    }

    // MARK: Automatic sync (OBS-107)

    /// Sync every vault `AutoSyncPolicy` picks, one at a time, after a fresh
    /// stale check. Runs on launch, on activation and from the background
    /// refresh; foreground runs within a minute of each other are skipped.
    func autoSync(reason: AutoSyncReason, now: Date = Date()) async {
        guard hasBearer, !sessionExpired, !protocolMismatch else { return }
        if reason != .background, let last = lastAutoSyncAttempt, now.timeIntervalSince(last) < 60 {
            return
        }
        lastAutoSyncAttempt = now
        refreshAllPending()
        await checkStale()
        for entry in entries {
            if cancelRequested { break }
            guard let state = vaultStates[entry.vaultID],
                  AutoSyncPolicy.shouldSync(state, now: Date()) else { continue }
            await syncAndWait(vaultID: entry.vaultID)
        }
    }

    /// `sync(vaultID:)`, returning when the cycle has applied or failed.
    /// Returns at once when the sync is declined (busy, missing vault).
    func syncAndWait(vaultID: String) async {
        guard !busy else { return }
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            syncCompletion = continuation
            sync(vaultID: vaultID)
            if !busy {
                syncCompletion = nil
                continuation.resume()
            }
        }
    }

    private func finishSyncWait() {
        syncCompletion?.resume()
        syncCompletion = nil
    }

    private func fail(_ error: Error, vaultID: String) {
        defer { finishSyncWait() }
        busy = false
        progress = nil
        if error.isUnauthorized {
            handleUnauthorized()
        }
        ActivityLog.recordError(vaultID: vaultID, message: error.obsinkMessage)
        vaultStates[vaultID]?.phase = .error(error.obsinkMessage)
        status = "Error: \(error.obsinkMessage)"
    }
}

/// The phone's name, off the main actor (`UIDevice` is main-actor bound in
/// newer SDKs; the value is read once and cached).
enum UIDeviceName {
    static let current: String = {
        if Thread.isMainThread {
            return MainActor.assumeIsolated { "\(UIDevice.current.name) (iOS)" }
        }
        return DispatchQueue.main.sync { "\(UIDevice.current.name) (iOS)" }
    }()
}

/// A failed account or vault action, shown as an alert.
struct AppAlert: Identifiable {
    let id = UUID()
    let title: String
    let message: String
}
