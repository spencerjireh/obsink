import AuthenticationServices
import SwiftUI

struct ContentView: View {
    @StateObject private var model = SyncModel()
    @Environment(\.scenePhase) private var scenePhase
    @State private var showingAddVault = false

    var body: some View {
        NavigationStack {
            Form {
                Section("Vaults") {
                    if model.entries.isEmpty {
                        Text("No vault configured. Tap “Add Vault…”.")
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
                    }
                    Button("Add Vault…") { showingAddVault = true }
                        .accessibilityIdentifier("addVaultButton")
                }

                Section("Status") {
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
                        Label("\(model.pendingLocalChanges) local change\(model.pendingLocalChanges == 1 ? "" : "s") — Sync to push", systemImage: "arrow.up.circle")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                    Button(action: model.sync) {
                        HStack {
                            if model.busy { ProgressView() }
                            Text(model.busy ? "Working…" : "Sync Now")
                        }
                    }
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
                }

                Section("Account") {
                    if model.hasBearer {
                        Text(model.accountEmail.map { "Signed in as \($0)" } ?? "Signed in")
                            .font(.callout)
                            .accessibilityIdentifier("accountText")
                        Button("Sign out", role: .destructive) { model.signOut() }
                            .disabled(model.busy)
                            .accessibilityIdentifier("signOutButton")
                    } else {
                        Text("Signed out — add the vault again to sign in.")
                            .font(.caption).foregroundStyle(.orange)
                    }
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Server").font(.caption).foregroundStyle(.secondary)
                        Text(model.serverURL).font(.caption.monospaced()).textSelection(.enabled)
                    }
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Vault ID").font(.caption).foregroundStyle(.secondary)
                        Text(model.vaultID).font(.caption.monospaced()).textSelection(.enabled)
                    }
                    SecureField(model.hasStoredKey ? "Passphrase (saved — not needed)" : "Passphrase", text: $model.passphrase)
                        .accessibilityIdentifier("passphraseField")
                    if model.hasStoredKey {
                        Text("Key saved in Keychain for this vault.")
                            .font(.caption).foregroundStyle(.green)
                    }
                }

                if !model.conflicts.isEmpty {
                    Section("Conflicts") {
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
                        Button("Apply Resolutions", action: model.resolve)
                            .disabled(model.busy)
                            .accessibilityIdentifier("applyResolutionsButton")
                    }
                }

                if !model.failures.isEmpty {
                    Section("Failed this sync") {
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

private struct SecureLabeledField: View {
    let label: String
    @Binding var text: String

    init(_ label: String, text: Binding<String>) {
        self.label = label
        self._text = text
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label).font(.caption).foregroundStyle(.secondary)
            SecureField(label, text: $text)
                .accessibilityIdentifier(label)
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
            Picker("Resolution", selection: $choice) {
                Text("Keep local").tag(MobileChoice.keepLocal)
                Text("Keep remote").tag(MobileChoice.keepRemote)
                Text("Keep both").tag(MobileChoice.keepBoth)
            }
            .pickerStyle(.segmented)
        }
        .padding(.vertical, 4)
    }
}

/// Per-conflict detail screen (OBS-24/25): segmented winner + read-only preview
/// of both versions' decrypted content.
private struct ConflictDetailView: View {
    let path: String
    @ObservedObject var model: SyncModel
    @Binding var choice: MobileChoice

    var body: some View {
        Form {
            Section("Resolution") {
                Picker("Winner", selection: $choice) {
                    Text("Keep local").tag(MobileChoice.keepLocal)
                    Text("Keep remote").tag(MobileChoice.keepRemote)
                    Text("Keep both").tag(MobileChoice.keepBoth)
                }
                .pickerStyle(.segmented)
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
    var onAdd: (VaultEntry) -> Void
    @Environment(\.dismiss) private var dismiss

    private static let lastServerKey = "lastServerURL"

    @State private var mode: Mode = .create
    @State private var serverURL: String = {
        (UserDefaults(suiteName: SyncModel.appGroup) ?? .standard).string(forKey: AddVaultView.lastServerKey) ?? "https://"
    }()
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
    @State private var codeSent = false

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
                            refreshSignInState()
                        }
                    signInSection
                }
                Section {
                    Picker("", selection: $mode) {
                        ForEach(Mode.allCases) { Text($0.rawValue).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .accessibilityIdentifier("modePicker")
                }
                if mode == .create {
                    Section("New vault") {
                        LabeledField("Vault name", text: $name, identifier: "addVaultName")
                    }
                } else {
                    Section("Connect") {
                        Button("List vaults") { fetchVaults() }
                            .disabled(busy || bearer == nil)
                            .accessibilityIdentifier("listVaultsButton")
                        if available.isEmpty {
                            Text("Tap “List vaults”, then pick one.")
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
                Section {
                    SecureField("Passphrase", text: $passphrase)
                        .accessibilityIdentifier("addVaultPassphraseField")
                }
                if !status.isEmpty {
                    Text(status).font(.caption).foregroundStyle(.red)
                        .accessibilityIdentifier("addVaultStatusText")
                }
                Section {
                    Button(mode == .create ? "Create Vault" : "Connect Vault") { submit() }
                        .disabled(!canSubmit)
                        .accessibilityIdentifier("addVaultSubmitButton")
                }
            }
            .navigationTitle("Add Vault")
            .toolbar { Button("Cancel") { dismiss() } }
            .onAppear { refreshSignInState() }
        }
    }

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
        } else {
            SignInWithAppleButton(.signIn) { request in
                request.requestedScopes = [.email]
            } onCompletion: { result in
                handleApple(result)
            }
            .signInWithAppleButtonStyle(.black)
            .frame(height: 44)
            .accessibilityIdentifier("signInWithAppleButton")
            Text("or use your email").font(.caption).foregroundStyle(.secondary)
            LabeledField("Email", text: $authEmail, identifier: "addVaultEmail")
                .keyboardType(.emailAddress)
                .disabled(codeSent)
            if codeSent {
                LabeledField("6-digit code", text: $authCode, identifier: "addVaultCode")
                    .keyboardType(.numberPad)
                Button("Verify and sign in") { verifyCode() }
                    .disabled(busy || authCode.trimmingCharacters(in: .whitespaces).count != 6)
                    .accessibilityIdentifier("verifyCodeButton")
                Button("Change email") { codeSent = false; authCode = "" }.disabled(busy)
            } else {
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
            let email = (try? authMe(serverUrl: url, token: token))?.email
            await MainActor.run { if canonicalURL == url { accountEmail = email } }
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
                await MainActor.run { status = error.localizedDescription; busy = false }
            }
        }
    }

    private func verifyCode() {
        busy = true; status = ""
        let url = canonicalURL, email = authEmail.trimmingCharacters(in: .whitespaces)
        let code = authCode.trimmingCharacters(in: .whitespaces), device = Self.deviceName
        Task.detached {
            do {
                let session = try authEmailVerify(serverUrl: url, email: email, code: code, deviceName: device)
                KeychainStore.saveBearer(session.token, serverURL: url)
                await MainActor.run {
                    accountEmail = session.email
                    signedIn = true
                    codeSent = false
                    authCode = ""
                    busy = false
                    rememberServer()
                }
            } catch {
                await MainActor.run { status = error.localizedDescription; busy = false }
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
            Task.detached {
                do {
                    let session = try authApple(serverUrl: url, identityToken: identityToken, deviceName: device, email: email)
                    KeychainStore.saveBearer(session.token, serverURL: url)
                    await MainActor.run {
                        accountEmail = session.email
                        signedIn = true
                        busy = false
                        rememberServer()
                    }
                } catch {
                    await MainActor.run { status = error.localizedDescription; busy = false }
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
                await MainActor.run { status = error.localizedDescription; busy = false }
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
                    _ = KeychainStore.save(derived, account: vid)
                    let vname = availableNames.first { $0.id == vid }?.name ?? vid
                    await MainActor.run {
                        rememberServer()
                        onAdd(VaultEntry(serverURL: url, vaultID: vid, name: vname))
                        dismiss()
                    }
                }
            } catch {
                await MainActor.run { status = error.localizedDescription; busy = false }
            }
        }
    }
}

#Preview {
    ContentView()
}
