import AuthenticationServices
import SwiftUI

struct ContentView: View {
    @StateObject private var model = SyncModel()
    @Environment(\.scenePhase) private var scenePhase
    @State private var showingAddVault = false
    @State private var showingSignIn = false
    @State private var confirmingDeleteAccount = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    if model.entries.isEmpty {
                        Text("No vault yet. Tap Add vault.")
                            .font(.caption).foregroundStyle(.secondary)
                    } else {
                        Picker("Active", selection: Binding(
                            get: { model.activeVaultID },
                            set: { model.selectVault($0) }
                        )) {
                            ForEach(model.entries) { entry in
                                Text(entry.name).tag(entry.vaultID)
                            }
                        }
                        .accessibilityIdentifier("activeVaultPicker")
                        if let usage = model.vaultUsageText(for: model.activeVaultID) {
                            Text(usage).font(.caption.monospaced()).foregroundStyle(.secondary)
                                .accessibilityIdentifier("vaultUsageText")
                        }
                        NavigationLink {
                            VaultDetailView(model: model, vaultID: model.activeVaultID)
                        } label: {
                            Label("Manage vault", systemImage: "externaldrive")
                        }
                        .accessibilityIdentifier("manageVaultButton")
                    }
                    Button("Add vault") { showingAddVault = true }
                        .accessibilityIdentifier("addVaultButton")
                } header: {
                    Label("Vaults", systemImage: "folder")
                }

                Section {
                    Text(model.status)
                        .font(.callout)
                        .foregroundStyle(model.status.hasPrefix("Error") ? .red : .primary)
                        .accessibilityIdentifier("statusText")
                    if model.staleDownloads > 0 {
                        // Stale-vault warning (spec §3.4, OBS-33).
                        Label("\(model.staleDownloads) file\(model.staleDownloads == 1 ? "" : "s") changed on another device. Sync before editing.", systemImage: "exclamationmark.triangle")
                            .font(.caption)
                            .foregroundStyle(.orange)
                            .accessibilityIdentifier("staleBanner")
                    }
                    if model.pendingLocalChanges > 0 {
                        Label("\(model.pendingLocalChanges) local change\(model.pendingLocalChanges == 1 ? "" : "s") not uploaded yet", systemImage: "arrow.up.circle")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                    // The passphrase is asked for where it unblocks Sync now,
                    // and only until the derived key is in the Keychain.
                    if !model.vaultID.isEmpty && !model.hasStoredKey {
                        SecureField("Passphrase", text: $model.passphrase)
                            .accessibilityIdentifier("passphraseField")
                    }
                    // The one primary action (DESIGN.md §1): full width, amber
                    // fill with ink text in both appearances. The identifier
                    // stays on the Button so the UI tests keep finding it.
                    Button(action: model.sync) {
                        HStack(spacing: 8) {
                            if model.busy { ProgressView().tint(Color("Ink")) }
                            Text(model.busy ? "Working…" : "Sync now")
                                .fontWeight(.semibold)
                        }
                        .foregroundStyle(Color("Ink"))
                        .frame(maxWidth: .infinity, minHeight: 32)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Color("Amber"))
                    .disabled(model.busy || model.vaultID.isEmpty || (model.passphrase.isEmpty && !model.hasStoredKey))
                    .accessibilityIdentifier("syncButton")

                    if model.busy, let p = model.progress {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("\(p.phase)\(p.path.map { " · \($0)" } ?? "")")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            if p.total > 0 {
                                ProgressView(value: Double(p.current), total: Double(p.total))
                            }
                        }
                    }
                } header: {
                    Label("Status", systemImage: "arrow.triangle.2.circlepath")
                }

                Section {
                    if model.hasBearer {
                        Text(model.accountEmail.map { "Signed in as \($0)" } ?? "Signed in")
                            .font(.callout)
                            .accessibilityIdentifier("accountText")
                        if let usage = model.usageText {
                            Text(usage).font(.caption).foregroundStyle(.secondary)
                                .accessibilityIdentifier("usageText")
                        }
                        if model.account != nil {
                            NavigationLink {
                                DevicesView(model: model)
                            } label: {
                                Label("Devices", systemImage: "iphone")
                                    .badge(model.account?.devices.count ?? 0)
                            }
                            .accessibilityIdentifier("devicesLink")
                        }
                        NavigationLink {
                            InvitesView(model: model)
                        } label: {
                            Label("Invites", systemImage: "envelope")
                                .badge(model.invites.filter { $0.status == "active" }.count)
                        }
                        .accessibilityIdentifier("invitesLink")
                        Button("Sign out", role: .destructive) { model.signOut() }
                            .disabled(model.busy)
                            .accessibilityIdentifier("signOutButton")
                        if model.account != nil {
                            Button("Delete account", role: .destructive) { confirmingDeleteAccount = true }
                                .disabled(model.busy)
                                .accessibilityIdentifier("deleteAccountButton")
                        }
                    } else {
                        if model.sessionExpired {
                            Label(MobileError.sessionExpiredMessage, systemImage: "exclamationmark.triangle")
                                .font(.caption).foregroundStyle(.red)
                                .accessibilityIdentifier("sessionExpiredText")
                        } else {
                            Text("Signed out.").font(.caption).foregroundStyle(.orange)
                        }
                        if !model.vaultID.isEmpty {
                            Button("Sign in") { showingSignIn = true }
                                .disabled(model.busy)
                                .accessibilityIdentifier("signInButton")
                        }
                    }
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Server").font(.caption).foregroundStyle(.secondary)
                        Text(model.serverURL).font(.caption.monospaced()).textSelection(.enabled)
                    }
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Vault ID").font(.caption).foregroundStyle(.secondary)
                        Text(model.vaultID).font(.caption.monospaced()).textSelection(.enabled)
                    }
                    if model.hasStoredKey {
                        Label("Key saved on this device", systemImage: "key.fill")
                            .font(.caption).foregroundStyle(.green)
                    }
                } header: {
                    Label("Account", systemImage: "person.crop.circle")
                }

                // Always visible: conflicts are first-class (DESIGN.md §1).
                Section {
                    if model.conflicts.isEmpty {
                        Text("No conflicts. Sync pauses here when both devices changed the same file.")
                            .font(.caption).foregroundStyle(.secondary)
                    } else {
                        ForEach(model.conflicts, id: \.path) { conflict in
                            NavigationLink {
                                ConflictDetailView(
                                    path: conflict.path,
                                    model: model,
                                    choice: Binding(
                                        get: { model.choices[conflict.path] ?? .keepLocal },
                                        set: { model.choices[conflict.path] = $0 }
                                    )
                                )
                            } label: {
                                ConflictRow(conflict: conflict, choice: Binding(
                                    get: { model.choices[conflict.path] ?? .keepLocal },
                                    set: { model.choices[conflict.path] = $0 }
                                ))
                            }
                            .accessibilityIdentifier("conflictRow")
                        }
                        Button("Apply resolutions", action: model.resolve)
                            .disabled(model.busy)
                            .accessibilityIdentifier("applyResolutionsButton")
                    }
                } header: {
                    Label("Conflicts", systemImage: "arrow.triangle.branch")
                }

                if !model.failures.isEmpty {
                    Section {
                        ForEach(model.failures, id: \.self) { failure in
                            VStack(alignment: .leading, spacing: 2) {
                                HStack(spacing: 6) {
                                    Text(failure.fatal ? "FATAL" : "skipped")
                                        .font(.caption2.weight(.bold))
                                        .foregroundStyle(failure.fatal ? .red : .orange)
                                    Text(failure.path).font(.caption)
                                }
                                Text(failure.error)
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    } header: {
                        Label("Failed this sync", systemImage: "xmark.octagon")
                    }
                }
            }
            .navigationTitle("ObSink")
            .task { model.checkStale() }
            .onChange(of: scenePhase) { _, phase in
                if phase == .active {
                    model.refreshPending()
                    model.checkStale()
                }
            }
            .sheet(isPresented: $showingAddVault) {
                AddVaultView { model.addVault($0) }
            }
            .sheet(isPresented: $showingSignIn, onDismiss: { model.reloadBearerState() }) {
                AddVaultView(initialServerURL: model.serverURL, signInOnly: true)
            }
            .sheet(isPresented: $confirmingDeleteAccount) {
                TypedConfirmationSheet(
                    title: "Delete account",
                    message: "This deletes your account, every vault it owns on \(model.serverURL), and every signed-in device. The copies on this device are removed too.",
                    expected: model.accountEmail ?? "delete",
                    caseInsensitive: true,
                    confirmLabel: "Delete account"
                ) {
                    model.deleteAccount()
                }
            }
            .alert(item: $model.alert) { alert in
                Alert(title: Text(alert.title), message: Text(alert.message), dismissButton: .default(Text("OK")))
            }
        }
    }
}

private struct LabeledField: View {
    let label: String
    let identifier: String
    @Binding var text: String

    /// `identifier` defaults to the label; the Add Vault sheet passes distinct
    /// ids so UI tests don't collide with the identical root-form fields.
    init(_ label: String, text: Binding<String>, identifier: String? = nil) {
        self.label = label
        self.identifier = identifier ?? label
        self._text = text
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label).font(.caption).foregroundStyle(.secondary)
            TextField(label, text: $text)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .accessibilityIdentifier(identifier)
        }
    }
}

private struct ConflictRow: View {
    let conflict: MobileConflict
    @Binding var choice: MobileChoice

    private static let formatter: DateFormatter = {
        let f = DateFormatter()
        f.dateStyle = .short
        f.timeStyle = .short
        return f
    }()

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(conflict.path).font(.subheadline.weight(.semibold))
                .accessibilityIdentifier("conflictRowTitle")
            VStack(alignment: .leading, spacing: 1) {
                Text("This device · \(conflict.localSize)B · \(Self.formatter.string(from: Date(timeIntervalSince1970: TimeInterval(conflict.localModified))))")
                Text("Other device · \(conflict.remoteSize)B · \(Self.formatter.string(from: Date(timeIntervalSince1970: TimeInterval(conflict.remoteModified))))")
            }
            .font(.caption).foregroundStyle(.secondary)
            ResolutionPicker(
                title: "Resolution",
                localDeleted: conflict.localDeleted,
                remoteDeleted: conflict.remoteDeleted,
                choice: $choice
            )
        }
        .padding(.vertical, 4)
    }
}

/// The winner picker for one conflict. "Keep both" needs two live versions;
/// with a deletion on one side the only question is which side wins, and the
/// labels say what that means.
private struct ResolutionPicker: View {
    let title: String
    let localDeleted: Bool
    let remoteDeleted: Bool
    @Binding var choice: MobileChoice

    var body: some View {
        Picker(title, selection: $choice) {
            Text(localDeleted ? "Delete on server" : "Keep local").tag(MobileChoice.keepLocal)
            Text(remoteDeleted ? "Delete here" : "Keep remote").tag(MobileChoice.keepRemote)
            if !localDeleted && !remoteDeleted {
                Text("Keep both").tag(MobileChoice.keepBoth)
            }
        }
        .pickerStyle(.segmented)
        .onAppear {
            if choice == .keepBoth && (localDeleted || remoteDeleted) { choice = .keepLocal }
        }
    }
}

/// Per-conflict detail screen (OBS-24/25): segmented winner + read-only preview
/// of both versions' decrypted content.
private struct ConflictDetailView: View {
    let path: String
    @ObservedObject var model: SyncModel
    @Binding var choice: MobileChoice

    private var conflict: MobileConflict? { model.conflicts.first { $0.path == path } }

    var body: some View {
        Form {
            Section("Resolution") {
                ResolutionPicker(
                    title: "Winner",
                    localDeleted: conflict?.localDeleted ?? false,
                    remoteDeleted: conflict?.remoteDeleted ?? false,
                    choice: $choice
                )
                .accessibilityIdentifier("winnerPicker")
            }
            Section("This device") {
                previewText(model.previews[path]?.localText, deleted: model.previews[path]?.localDeleted ?? false)
            }
            Section("Other device") {
                previewText(model.previews[path]?.remoteText, deleted: model.previews[path]?.remoteDeleted ?? false)
            }
        }
        .navigationTitle(path)
    }

    @ViewBuilder
    private func previewText(_ text: String?, deleted: Bool) -> some View {
        if deleted {
            Text("(deleted on this side)").font(.caption).foregroundStyle(.secondary)
        } else if let text {
            Text(text.isEmpty ? "(empty)" : text)
                .font(.system(.body, design: .monospaced))
                .textSelection(.enabled)
        } else {
            Text("Loading…").foregroundStyle(.secondary)
        }
    }
}

/// Add a vault: enter the server URL, sign in (Sign in with Apple or an
/// emailed code), then create a new vault or connect to an existing one
/// (spec §12.1/§12.2). The bearer goes to the Keychain under the server URL;
/// the derived vault key under the vault ID.
struct AddVaultView: View {
    var onAdd: (VaultEntry) -> Void = { _ in }
    /// Sign-in only: no vault sections, a `Done` button once signed in. Used
    /// after a session expired.
    var signInOnly = false
    @Environment(\.dismiss) private var dismiss

    private static let lastServerKey = "lastServerURL"

    init(initialServerURL: String? = nil, signInOnly: Bool = false, onAdd: @escaping (VaultEntry) -> Void = { _ in }) {
        self.onAdd = onAdd
        self.signInOnly = signInOnly
        let remembered = (UserDefaults(suiteName: SyncModel.appGroup) ?? .standard)
            .string(forKey: AddVaultView.lastServerKey)
        _serverURL = State(initialValue: initialServerURL ?? remembered ?? "https://")
    }

    @State private var mode: Mode = .create
    @State private var serverURL: String = "https://"
    /// `GET /` for the typed server; nil until fetched (then everything shows).
    @State private var capabilities: MobileCapabilities?
    @State private var capabilitiesTask: Task<Void, Never>?
    /// The server refused a sign-up without an invite: show the field even if
    /// capabilities said none was needed.
    @State private var inviteForced = false
    @FocusState private var inviteFocused: Bool
    @State private var name = ""
    @State private var passphrase = ""
    @State private var available: [MobileVaultSummary] = []
    @State private var pickedVaultID: String?
    @State private var status = ""
    @State private var busy = false

    // Sign-in state for the typed server.
    @State private var accountEmail: String? = nil
    @State private var signedIn = false
    @State private var authEmail = ""
    @State private var authCode = ""
    @State private var inviteCode = ""
    @State private var codeSent = false
    /// Sign in with Apple gave a token without an email claim plus an email
    /// hint; the server wants a one-time code for that address before it
    /// links the two. Held until the code is verified.
    @State private var pendingAppleToken: String? = nil

    private var inviteOrNil: String? {
        let trimmed = inviteCode.trimmingCharacters(in: .whitespaces)
        return trimmed.isEmpty ? nil : trimmed
    }

    private enum Mode: String, CaseIterable, Identifiable {
        case create = "Create", connect = "Connect"
        var id: String { rawValue }
    }

    private var canonicalURL: String { KeychainStore.canonicalServerURL(serverURL) }
    private var hasServer: Bool {
        let url = canonicalURL
        return url.contains("://") && url != "https://" && url != "http://"
    }
    /// Bearer for the typed server, if we have one yet.
    private var bearer: String? {
        hasServer ? KeychainStore.loadBearer(serverURL: canonicalURL) : nil
    }
    private var canSubmit: Bool {
        !busy && bearer != nil && !passphrase.isEmpty
            && (mode == .create ? !name.isEmpty : pickedVaultID != nil)
    }

    var body: some View {
        NavigationStack {
            Form {
                Section("Server") {
                    LabeledField("Server URL", text: $serverURL, identifier: "addVaultServerURL")
                        .keyboardType(.URL)
                        .onChange(of: serverURL) { _, _ in
                            available = []; pickedVaultID = nil; status = ""
                            inviteForced = false
                            refreshSignInState()
                            scheduleCapabilities()
                        }
                        .onSubmit { fetchCapabilities() }
                    signInSection
                }
                if !signInOnly {
                Section {
                    Picker("", selection: $mode) {
                        ForEach(Mode.allCases) { Text($0.rawValue).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .accessibilityIdentifier("modePicker")
                }
                }
                if signInOnly {
                    // Nothing else: the vault sections belong to Add vault.
                } else if mode == .create {
                    Section("New vault") {
                        LabeledField("Vault name", text: $name, identifier: "addVaultName")
                    }
                } else {
                    Section("Connect") {
                        Button("List vaults") { fetchVaults() }
                            .disabled(busy || bearer == nil)
                            .accessibilityIdentifier("listVaultsButton")
                        if available.isEmpty {
                            Text("Tap List vaults, then pick one.")
                                .font(.caption).foregroundStyle(.secondary)
                        } else {
                            Picker("Vault", selection: $pickedVaultID) {
                                Text("—").tag(String?.none)
                                ForEach(available, id: \.id) { v in
                                    Text(v.name).tag(Optional(v.id))
                                }
                            }
                            .accessibilityIdentifier("vaultPicker")
                        }
                    }
                }
                if !signInOnly {
                    Section {
                        SecureField("Passphrase", text: $passphrase)
                            .accessibilityIdentifier("addVaultPassphraseField")
                    }
                }
                if !status.isEmpty {
                    Text(status).font(.caption).foregroundStyle(.red)
                        .accessibilityIdentifier("addVaultStatusText")
                }
                if !signInOnly {
                    Section {
                        Button(mode == .create ? "Create vault" : "Connect vault") { submit() }
                            .disabled(!canSubmit)
                            .accessibilityIdentifier("addVaultSubmitButton")
                    }
                }
            }
            .navigationTitle(signInOnly ? "Sign in" : "Add vault")
            .toolbar {
                if signInOnly {
                    Button("Done") { dismiss() }
                        .disabled(!signedIn)
                        .accessibilityIdentifier("addVaultDoneButton")
                } else {
                    Button("Cancel") { dismiss() }
                }
            }
            .onAppear {
                refreshSignInState()
                fetchCapabilities()
            }
        }
    }

    // MARK: Capabilities (`GET /`)

    /// Refetch 400 ms after the last keystroke; a stale answer for an old URL
    /// is dropped.
    private func scheduleCapabilities() {
        capabilitiesTask?.cancel()
        capabilities = nil
        guard hasServer else { return }
        capabilitiesTask = Task {
            try? await Task.sleep(nanoseconds: 400_000_000)
            guard !Task.isCancelled else { return }
            fetchCapabilities()
        }
    }

    private func fetchCapabilities() {
        guard hasServer else { capabilities = nil; return }
        let url = canonicalURL
        Task.detached {
            let caps = try? authCapabilities(serverUrl: url)
            await MainActor.run { if canonicalURL == url { capabilities = caps } }
        }
    }

    private var offersApple: Bool { capabilities?.apple ?? true }
    private var offersEmail: Bool { capabilities?.email ?? true }
    private var showInviteField: Bool { (capabilities?.inviteRequired ?? true) || inviteForced }

    // MARK: Sign-in

    @ViewBuilder
    private var signInSection: some View {
        if !hasServer {
            Text("Enter your server's URL to sign in.").font(.caption).foregroundStyle(.secondary)
        } else if signedIn {
            HStack {
                Text(accountEmail.map { "Signed in as \($0)" } ?? "Signed in")
                    .font(.callout)
                    .accessibilityIdentifier("addVaultAccountText")
                Spacer()
                Button("Sign out") { signOut() }.disabled(busy)
            }
        } else if !offersApple && !offersEmail {
            Text("This server has no sign-in method this app supports.")
                .font(.caption).foregroundStyle(.secondary)
        } else {
            if offersApple {
                SignInWithAppleButton(.signIn) { request in
                    request.requestedScopes = [.email]
                } onCompletion: { result in
                    handleApple(result)
                }
                .signInWithAppleButtonStyle(.black)
                .frame(height: 44)
                .accessibilityIdentifier("signInWithAppleButton")
            }
            if offersApple && offersEmail {
                Text("or use your email").font(.caption).foregroundStyle(.secondary)
            }
            if offersEmail {
                LabeledField("Email", text: $authEmail, identifier: "addVaultEmail")
                    .keyboardType(.emailAddress)
                    .disabled(codeSent)
            }
            if showInviteField {
                LabeledField("Invite code", text: $inviteCode, identifier: "addVaultInviteCode")
                    .textInputAutocapitalization(.characters)
                    .autocorrectionDisabled()
                    .focused($inviteFocused)
            }
            if offersEmail && codeSent {
                if pendingAppleToken != nil {
                    Text("Enter the code sent to \(authEmail) to link it to your Apple ID.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                LabeledField("6-digit code", text: $authCode, identifier: "addVaultCode")
                    .keyboardType(.numberPad)
                Button("Verify and sign in") { verifyCode() }
                    .disabled(busy || authCode.trimmingCharacters(in: .whitespaces).count != 6)
                    .accessibilityIdentifier("verifyCodeButton")
                Button("Change email") { codeSent = false; authCode = ""; pendingAppleToken = nil }.disabled(busy)
            } else if offersEmail {
                Button("Send sign-in code") { sendCode() }
                    .disabled(busy || !authEmail.contains("@"))
                    .accessibilityIdentifier("sendCodeButton")
            }
        }
    }

    private func refreshSignInState() {
        guard hasServer, let token = KeychainStore.loadBearer(serverURL: canonicalURL) else {
            signedIn = false
            accountEmail = nil
            return
        }
        signedIn = true
        let url = canonicalURL
        Task.detached {
            // The operator bearer has no account and answers with `Sync`; only
            // a 401 means the stored bearer is dead.
            let result = Result { try authMe(serverUrl: url, token: token) }
            await MainActor.run {
                guard canonicalURL == url else { return }
                switch result {
                case .success(let account):
                    accountEmail = account.email
                case .failure(let error) where error.isUnauthorized:
                    KeychainStore.deleteBearer(serverURL: url)
                    signedIn = false
                    accountEmail = nil
                    status = MobileError.sessionExpiredMessage
                case .failure:
                    break
                }
            }
        }
    }

    /// Every sign-in failure lands here: an invite refusal reveals and
    /// focuses the field; everything else shows its message.
    private func signInFailed(_ error: Error) {
        busy = false
        if let mobile = error as? MobileError, mobile.isInviteRequired {
            inviteForced = true
            inviteFocused = true
            status = "Enter the invite code you were given."
        } else {
            status = error.obsinkMessage
        }
    }

    /// A vault call failed after sign-in: a 401 drops the bearer and shows
    /// the sign-in controls again.
    private func vaultCallFailed(_ error: Error) {
        busy = false
        if error.isUnauthorized {
            KeychainStore.deleteBearer(serverURL: canonicalURL)
            signedIn = false
            accountEmail = nil
            status = MobileError.sessionExpiredMessage
        } else {
            status = error.obsinkMessage
        }
    }

    private func rememberServer() {
        (UserDefaults(suiteName: SyncModel.appGroup) ?? .standard).set(canonicalURL, forKey: Self.lastServerKey)
    }

    private static var deviceName: String {
        "\(UIDevice.current.name) (iOS)"
    }

    private func sendCode() {
        busy = true; status = ""
        let url = canonicalURL, email = authEmail.trimmingCharacters(in: .whitespaces)
        Task.detached {
            do {
                let devCode = try authEmailStart(serverUrl: url, email: email)
                await MainActor.run {
                    codeSent = true
                    busy = false
                    if let devCode { authCode = devCode }
                }
            } catch {
                await MainActor.run { signInFailed(error) }
            }
        }
    }

    private func verifyCode() {
        busy = true; status = ""
        let url = canonicalURL, email = authEmail.trimmingCharacters(in: .whitespaces)
        let code = authCode.trimmingCharacters(in: .whitespaces), device = Self.deviceName
        let invite = inviteOrNil, appleToken = pendingAppleToken
        Task.detached {
            do {
                let session: MobileSession
                if let appleToken {
                    session = try authApple(serverUrl: url, identityToken: appleToken, deviceName: device, email: email, code: code, inviteCode: invite)
                } else {
                    session = try authEmailVerify(serverUrl: url, email: email, code: code, deviceName: device, inviteCode: invite)
                }
                KeychainStore.saveBearer(session.token, serverURL: url)
                await MainActor.run {
                    accountEmail = session.email
                    signedIn = true
                    codeSent = false
                    authCode = ""
                    pendingAppleToken = nil
                    busy = false
                    rememberServer()
                }
            } catch {
                await MainActor.run { signInFailed(error) }
            }
        }
    }

    private func handleApple(_ result: Result<ASAuthorization, Error>) {
        switch result {
        case .failure(let error):
            // User cancel is not an error worth showing.
            if (error as? ASAuthorizationError)?.code != .canceled {
                status = error.localizedDescription
            }
        case .success(let authorization):
            guard let credential = authorization.credential as? ASAuthorizationAppleIDCredential,
                  let tokenData = credential.identityToken,
                  let identityToken = String(data: tokenData, encoding: .utf8) else {
                status = "Apple did not return an identity token."
                return
            }
            busy = true; status = ""
            let url = canonicalURL, email = credential.email, device = Self.deviceName
            let invite = inviteOrNil
            Task.detached {
                do {
                    let session = try authApple(serverUrl: url, identityToken: identityToken, deviceName: device, email: email, code: nil, inviteCode: invite)
                    KeychainStore.saveBearer(session.token, serverURL: url)
                    await MainActor.run {
                        accountEmail = session.email
                        signedIn = true
                        busy = false
                        rememberServer()
                    }
                } catch {
                    // The token had no email claim: the server links the hint
                    // only once a one-time code for that address checks out.
                    if let email, (error as? MobileError)?.needsEmailVerification == true {
                        do {
                            let devCode = try authEmailStart(serverUrl: url, email: email)
                            await MainActor.run {
                                authEmail = email
                                pendingAppleToken = identityToken
                                codeSent = true
                                authCode = devCode ?? ""
                                busy = false
                            }
                            return
                        } catch {
                            await MainActor.run { signInFailed(error) }
                            return
                        }
                    }
                    await MainActor.run { signInFailed(error) }
                }
            }
        }
    }

    private func signOut() {
        let url = canonicalURL
        if let token = KeychainStore.loadBearer(serverURL: url) {
            Task.detached { try? authLogout(serverUrl: url, token: token) }
        }
        KeychainStore.deleteBearer(serverURL: url)
        signedIn = false
        accountEmail = nil
        available = []
        pickedVaultID = nil
    }

    // MARK: Vault create / connect

    private func fetchVaults() {
        guard let key = bearer else { return }
        busy = true
        status = ""
        let url = canonicalURL
        Task.detached {
            do {
                let vaults = try listVaults(serverUrl: url, apiKey: key)
                await MainActor.run {
                    available = vaults
                    pickedVaultID = nil
                    busy = false
                    if vaults.isEmpty { status = "No vaults found at this server." }
                }
            } catch {
                await MainActor.run { vaultCallFailed(error) }
            }
        }
    }

    private func submit() {
        guard let key = bearer else { return }
        busy = true
        status = ""
        let url = canonicalURL, name = self.name, pass = passphrase
        let mode = self.mode, picked = pickedVaultID
        let availableNames = available
        Task.detached {
            do {
                switch mode {
                case .create:
                    let summary = try createVault(serverUrl: url, apiKey: key, name: name)
                    let derived = try deriveMasterKey(passphrase: pass, vaultId: summary.id)
                    _ = KeychainStore.save(derived, account: summary.id)
                    await MainActor.run {
                        rememberServer()
                        onAdd(VaultEntry(serverURL: url, vaultID: summary.id, name: summary.name))
                        dismiss()
                    }
                case .connect:
                    guard let vid = picked else {
                        await MainActor.run { status = "Pick a vault first."; busy = false }
                        return
                    }
                    let derived = try deriveMasterKey(passphrase: pass, vaultId: vid)
                    // Prove the passphrase against a stored blob before keeping
                    // the key; a wrong one would fail on every sync afterwards.
                    let probe = MobileVaultConfig(serverUrl: url, apiKey: key, vaultId: vid, localPath: "")
                    guard try validateVaultKey(config: probe, key: derived) else {
                        await MainActor.run { status = "Passphrase does not match this vault."; busy = false }
                        return
                    }
                    _ = KeychainStore.save(derived, account: vid)
                    let vname = availableNames.first { $0.id == vid }?.name ?? vid
                    await MainActor.run {
                        rememberServer()
                        onAdd(VaultEntry(serverURL: url, vaultID: vid, name: vname))
                        dismiss()
                    }
                }
            } catch {
                await MainActor.run { vaultCallFailed(error) }
            }
        }
    }
}

#Preview {
    ContentView()
}
