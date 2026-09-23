import AuthenticationServices
import SwiftUI

/// Sign in to this build's server (spec §12.1): Sign in with Apple or an
/// emailed code, with the invite field only when the server needs one
/// (`GET /`) or just refused a sign-up without one; then the passphrase
/// step: `Set passphrase` (a new account, twice, 12 characters at least) or
/// `Unlock` (an existing one). The bearer, the user id and the account key
/// go to the Keychain.
struct SignInPane: View {
    @ObservedObject var model: SyncModel
    /// Called once the account is unlocked.
    var onSignedIn: (String?) -> Void = { _ in }

    private enum Step { case signIn, passphrase }

    @State private var step: Step = .signIn
    @State private var capabilities: MobileCapabilities?
    @State private var inviteForced = false
    @FocusState private var inviteFocused: Bool
    @State private var authEmail = ""
    @State private var authCode = ""
    @State private var inviteCode = ""
    @State private var codeSent = false
    @State private var status = ""
    @State private var busy = false
    @State private var signedInEmail: String?
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
            switch step {
            case .signIn:
                signInBody
            case .passphrase:
                PassphraseStep(model: model, status: $status) {
                    onSignedIn(signedInEmail)
                }
            }
            if !status.isEmpty {
                Text(status).font(.caption).foregroundStyle(.red)
                    .accessibilityIdentifier("addVaultStatusText")
            }
        }
        .onAppear {
            fetchCapabilities()
            // Already signed in (the sheet opened for the unlock): straight
            // to the passphrase step.
            if model.hasBearer { step = .passphrase }
        }
    }

    @ViewBuilder
    private var signInBody: some View {
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
                LabeledField("Email", text: $authEmail, identifier: "emailField")
                    .keyboardType(.emailAddress)
                    .disabled(codeSent)
            }
            if showInviteField {
                LabeledField("Invite code", text: $inviteCode, identifier: "inviteField")
                    .textInputAutocapitalization(.characters)
                    .autocorrectionDisabled()
                    .focused($inviteFocused)
            }
            if offersEmail && codeSent {
                if pendingAppleToken != nil {
                    Text("Enter the code sent to \(authEmail) to link it to your Apple ID.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                LabeledField("6-digit code", text: $authCode, identifier: "codeField")
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
        let code = authCode.trimmingCharacters(in: .whitespaces)
        let device = model.thisDevice()
        let invite = inviteOrNil, appleToken = pendingAppleToken
        Task.detached {
            do {
                let session: MobileSession
                if let appleToken {
                    session = try authApple(serverUrl: url, identityToken: appleToken, device: device, email: email, code: code, inviteCode: invite)
                } else {
                    session = try authEmailVerify(serverUrl: url, email: email, code: code, device: device, inviteCode: invite)
                }
                await MainActor.run {
                    codeSent = false
                    authCode = ""
                    pendingAppleToken = nil
                    busy = false
                    signedIn(session)
                }
            } catch {
                await MainActor.run { signInFailed(error) }
            }
        }
    }

    private func signedIn(_ session: MobileSession) {
        signedInEmail = session.email
        model.signedIn(session: session)
        step = .passphrase
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
            let url = serverURL, email = credential.email, device = model.thisDevice()
            let invite = inviteOrNil
            Task.detached {
                do {
                    let session = try authApple(serverUrl: url, identityToken: identityToken, device: device, email: email, code: nil, inviteCode: invite)
                    await MainActor.run {
                        busy = false
                        signedIn(session)
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

/// Spec §12.1, the step after sign-in: `Set passphrase` on a new account
/// (twice), `Unlock` on an existing one or after a lost first-set race. The
/// unlocked account key goes to the Keychain.
struct PassphraseStep: View {
    @ObservedObject var model: SyncModel
    @Binding var status: String
    var onUnlocked: () -> Void

    @State private var passphrase = ""
    @State private var again = ""
    @State private var busy = false
    @State private var raceNotice = ""
    @State private var checked = false

    private var setting: Bool { !model.hasServerKey }
    private var tooShort: Bool { !passphrase.isEmpty && passphrase.count < SyncModel.minPassphraseChars }
    private var valid: Bool {
        setting ? passphrase.count >= SyncModel.minPassphraseChars && !again.isEmpty : !passphrase.isEmpty
    }

    var body: some View {
        Group {
            if !checked {
                HStack(spacing: 8) {
                    ProgressView()
                    Text("Checking the account…").font(.caption).foregroundStyle(.secondary)
                }
            } else {
                Text(setting ? "Set passphrase" : "Unlock").font(.headline)
                Text(setting
                     ? "It unlocks every vault on every device. There is no recovery if it is lost."
                     : "The account passphrase, set on your first device.")
                    .font(.caption).foregroundStyle(.secondary)
                if !raceNotice.isEmpty {
                    Text(raceNotice).font(.caption).foregroundStyle(.orange)
                }
                SecureField("Passphrase", text: $passphrase)
                    .accessibilityIdentifier("unlockField")
                if setting {
                    SecureField("Again", text: $again)
                        .accessibilityIdentifier("unlockConfirmField")
                    Text("At least \(SyncModel.minPassphraseChars) characters.")
                        .font(.caption)
                        .foregroundStyle(tooShort ? Color.orange : Color.secondary)
                }
                Button(busy ? "Working…" : (setting ? "Set passphrase" : "Unlock")) { submit() }
                    .disabled(busy || !valid)
                    .accessibilityIdentifier(setting ? "setPassphraseButton" : "unlockButton")
            }
        }
        .task {
            await model.refreshKeyState()
            if !model.locked {
                // The key was already on this phone (a sign-in on a device
                // that had the account before).
                onUnlocked()
            }
            checked = true
        }
    }

    private func submit() {
        guard valid, !busy else { return }
        if setting && passphrase != again {
            status = "The passphrases do not match."
            return
        }
        busy = true
        status = ""
        let entered = passphrase
        Task { @MainActor in
            do {
                if setting {
                    let outcome = try await model.setPassphrase(entered)
                    if outcome == .mismatch {
                        // Spec §12.1: another device set it first; the form
                        // turns into Unlock.
                        raceNotice = "A passphrase was already set on another device. Enter it."
                        passphrase = ""
                        again = ""
                        busy = false
                        return
                    }
                } else {
                    try await model.unlock(entered)
                }
                busy = false
                onUnlocked()
            } catch {
                status = error.obsinkMessage
                busy = false
            }
        }
    }
}

/// The sign-in sheet: the pane plus `Done` once the account is unlocked.
struct SignInSheet: View {
    @ObservedObject var model: SyncModel
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
                        SignInPane(model: model) { email in
                            self.email = email ?? model.accountEmail
                            signedIn = true
                        }
                    }
                } header: {
                    Text(ServerConfig.defaultURL).font(.caption.monospaced())
                }
            }
            .navigationTitle(signedIn ? "Signed in" : "Sign in")
            .toolbar {
                Button("Done") { dismiss() }
                    .disabled(!signedIn)
                    .accessibilityIdentifier("addVaultDoneButton")
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
