import AuthenticationServices
import SwiftUI

/// Sign in to this build's server: Sign in with Apple or an emailed code,
/// with the invite field only when the server needs one (`GET /`) or just
/// refused a sign-up without one. The bearer goes to the Keychain under the
/// server URL. Shared by the Add vault flow and the sign-in sheet.
struct SignInPane: View {
    /// Called once a bearer is stored.
    var onSignedIn: (String?) -> Void = { _ in }

    @State private var capabilities: MobileCapabilities?
    @State private var inviteForced = false
    @FocusState private var inviteFocused: Bool
    @State private var authEmail = ""
    @State private var authCode = ""
    @State private var inviteCode = ""
    @State private var codeSent = false
    @State private var status = ""
    @State private var busy = false
    /// Sign in with Apple gave a token without an email claim plus an email
    /// hint; the server wants a one-time code for that address before it
    /// links the two. Held until the code is verified.
    @State private var pendingAppleToken: String? = nil

    private var serverURL: String { ServerConfig.defaultURL }
    private var offersApple: Bool { capabilities?.apple ?? true }
    private var offersEmail: Bool { capabilities?.email ?? true }
    private var showInviteField: Bool { (capabilities?.inviteRequired ?? true) || inviteForced }
    private var inviteOrNil: String? {
        let trimmed = inviteCode.trimmingCharacters(in: .whitespaces)
        return trimmed.isEmpty ? nil : trimmed
    }

    var body: some View {
        Group {
            if !offersApple && !offersEmail {
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
                    Button("Change email") { codeSent = false; authCode = ""; pendingAppleToken = nil }
                        .disabled(busy)
                } else if offersEmail {
                    Button("Send sign-in code") { sendCode() }
                        .disabled(busy || !authEmail.contains("@"))
                        .accessibilityIdentifier("sendCodeButton")
                }
            }
            if !status.isEmpty {
                Text(status).font(.caption).foregroundStyle(.red)
                    .accessibilityIdentifier("addVaultStatusText")
            }
        }
        .onAppear { fetchCapabilities() }
    }

    private func fetchCapabilities() {
        let url = serverURL
        Task.detached {
            let caps = try? authCapabilities(serverUrl: url)
            await MainActor.run { capabilities = caps }
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

    private static var deviceName: String {
        "\(UIDevice.current.name) (iOS)"
    }

    private func sendCode() {
        busy = true; status = ""
        let url = serverURL, email = authEmail.trimmingCharacters(in: .whitespaces)
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
        let url = serverURL, email = authEmail.trimmingCharacters(in: .whitespaces)
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
                    codeSent = false
                    authCode = ""
                    pendingAppleToken = nil
                    busy = false
                    onSignedIn(session.email)
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
            let url = serverURL, email = credential.email, device = Self.deviceName
            let invite = inviteOrNil
            Task.detached {
                do {
                    let session = try authApple(serverUrl: url, identityToken: identityToken, deviceName: device, email: email, code: nil, inviteCode: invite)
                    KeychainStore.saveBearer(session.token, serverURL: url)
                    await MainActor.run {
                        busy = false
                        onSignedIn(session.email)
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
}

/// Sign in only, after a session expired: the pane plus `Done`.
struct SignInSheet: View {
    @Environment(\.dismiss) private var dismiss
    @State private var signedIn = false
    @State private var email: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    if signedIn {
                        Text(email.map { "Signed in as \($0)" } ?? "Signed in")
                            .accessibilityIdentifier("addVaultAccountText")
                    } else {
                        SignInPane { email in
                            self.email = email
                            signedIn = true
                        }
                    }
                } header: {
                    Text(ServerConfig.defaultURL).font(.caption.monospaced())
                }
            }
            .navigationTitle("Sign in")
            .toolbar {
                Button("Done") { dismiss() }
                    .disabled(!signedIn)
                    .accessibilityIdentifier("addVaultDoneButton")
            }
        }
    }
}

/// Add a vault, one question at a time: sign in (only when signed out), pick
/// an existing vault or name a new one, then the passphrase (spec §12.1/§12.2).
/// The derived vault key goes to the Keychain under the vault ID.
struct AddVaultFlow: View {
    var onAdd: (VaultEntry) -> Void = { _ in }
    @Environment(\.dismiss) private var dismiss

    private enum Step { case signIn, pick, passphrase }
    private enum Mode: String, CaseIterable, Identifiable {
        case create = "Create", connect = "Connect"
        var id: String { rawValue }
    }

    @State private var step: Step
    @State private var accountEmail: String?
    @State private var mode: Mode = .connect
    @State private var name = ""
    @State private var passphrase = ""
    @State private var available: [MobileVaultSummary] = []
    @State private var listed = false
    @State private var pickedVaultID: String?
    @State private var status = ""
    @State private var busy = false

    init(onAdd: @escaping (VaultEntry) -> Void = { _ in }) {
        self.onAdd = onAdd
        _step = State(initialValue: KeychainStore.loadBearer(serverURL: ServerConfig.defaultURL) == nil ? .signIn : .pick)
    }

    private var serverURL: String { ServerConfig.defaultURL }
    private var bearer: String? { KeychainStore.loadBearer(serverURL: serverURL) }
    private var pickValid: Bool {
        mode == .create ? !name.trimmingCharacters(in: .whitespaces).isEmpty : pickedVaultID != nil
    }
    private var canSubmit: Bool { !busy && bearer != nil && !passphrase.isEmpty && pickValid }

    var body: some View {
        NavigationStack {
            Form {
                switch step {
                case .signIn:
                    Section {
                        SignInPane { email in
                            accountEmail = email
                            step = .pick
                        }
                    } header: {
                        Text("Sign in")
                    } footer: {
                        Text(serverURL).font(.caption.monospaced())
                    }
                case .pick:
                    pickSection
                case .passphrase:
                    passphraseSection
                }
                if !status.isEmpty && step != .signIn {
                    Text(status).font(.caption).foregroundStyle(.red)
                        .accessibilityIdentifier("addVaultStatusText")
                }
            }
            .navigationTitle("Add vault")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    if step == .passphrase {
                        Button("Back") { step = .pick }.disabled(busy)
                    } else {
                        Button("Cancel") { dismiss() }
                    }
                }
            }
            .onAppear { refreshSignInState() }
        }
    }

    @ViewBuilder
    private var pickSection: some View {
        Section {
            Text(accountEmail.map { "Signed in as \($0)" } ?? "Signed in")
                .font(.callout)
                .accessibilityIdentifier("addVaultAccountText")
            Picker("", selection: $mode) {
                ForEach(Mode.allCases) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .accessibilityIdentifier("modePicker")
        } header: {
            Text("Choose vault")
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
                    Text(listed ? "No vaults on this server yet. Create one." : "Loading vaults…")
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
            Button("Next") { status = ""; step = .passphrase }
                .disabled(busy || !pickValid)
                .accessibilityIdentifier("addVaultNextButton")
        }
    }

    @ViewBuilder
    private var passphraseSection: some View {
        Section {
            SecureField("Passphrase", text: $passphrase)
                .accessibilityIdentifier("addVaultPassphraseField")
        } header: {
            Text("Passphrase")
        } footer: {
            Text(mode == .create
                 ? "Encrypts the vault. There is no recovery if it is lost."
                 : "The passphrase this vault was created with.")
        }
        Section {
            Button(action: submit) {
                Text(busy ? "Working…" : (mode == .create ? "Create vault" : "Connect vault"))
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .tint(Color("Amber"))
            .foregroundStyle(Color("Ink"))
            .disabled(!canSubmit)
            .accessibilityIdentifier("addVaultSubmitButton")
        }
    }

    /// The stored bearer decides whether sign-in is a step; a dead one is
    /// dropped so the flow starts with sign-in.
    private func refreshSignInState() {
        guard let token = bearer else {
            step = .signIn
            return
        }
        let url = serverURL
        Task.detached {
            // The operator bearer has no account and answers with `Sync`; only
            // a 401 means the stored bearer is dead.
            let result = Result { try authMe(serverUrl: url, token: token) }
            await MainActor.run {
                switch result {
                case .success(let account):
                    accountEmail = account.email
                    if step == .pick && !listed { fetchVaults() }
                case .failure(let error) where error.isUnauthorized:
                    KeychainStore.deleteBearer(serverURL: url)
                    step = .signIn
                    status = MobileError.sessionExpiredMessage
                case .failure:
                    if step == .pick && !listed { fetchVaults() }
                }
            }
        }
    }

    /// A vault call failed after sign-in: a 401 drops the bearer and goes
    /// back to the sign-in step.
    private func vaultCallFailed(_ error: Error) {
        busy = false
        if error.isUnauthorized {
            KeychainStore.deleteBearer(serverURL: serverURL)
            step = .signIn
            status = MobileError.sessionExpiredMessage
        } else {
            status = error.obsinkMessage
        }
    }

    private func fetchVaults() {
        guard let key = bearer else { return }
        busy = true
        status = ""
        let url = serverURL
        Task.detached {
            do {
                let vaults = try listVaults(serverUrl: url, apiKey: key)
                await MainActor.run {
                    available = vaults
                    listed = true
                    busy = false
                    if vaults.isEmpty {
                        mode = .create
                    } else if pickedVaultID == nil || !vaults.contains(where: { $0.id == pickedVaultID }) {
                        pickedVaultID = vaults.first?.id
                    }
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
        let url = serverURL, name = self.name.trimmingCharacters(in: .whitespaces), pass = passphrase
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

/// A captioned text field. `identifier` defaults to the label.
struct LabeledField: View {
    let label: String
    let identifier: String
    @Binding var text: String

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
