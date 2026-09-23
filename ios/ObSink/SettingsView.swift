import SwiftUI

/// The Settings tab (spec §15.4): the account on this build's server
/// (signed in as, usage, `Change passphrase`, invites, sign out, delete
/// account), sign-in when signed out, the unlock form when signed in but
/// locked, and the app version.
struct SettingsView: View {
    @ObservedObject var model: SyncModel
    @State private var showingSignIn = false
    @State private var confirmingDeleteAccount = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    if model.hasBearer {
                        Text(model.accountEmail.map { "Signed in as \($0)" } ?? "Signed in")
                            .font(.callout)
                            .accessibilityIdentifier("accountText")
                        if model.locked {
                            Label(model.hasServerKey ? "Locked. Enter the account passphrase." : "Set the account passphrase to start.",
                                  systemImage: "lock")
                                .font(.caption).foregroundStyle(.orange)
                            Button(model.hasServerKey ? "Unlock" : "Set passphrase") { showingSignIn = true }
                                .disabled(model.busy)
                                .accessibilityIdentifier(model.hasServerKey ? "unlockButton" : "setPassphraseButton")
                        }
                        if let usage = model.usageText {
                            Text(usage).font(.caption).foregroundStyle(.secondary)
                                .accessibilityIdentifier("usageText")
                        }
                        Button("Sign out", role: .destructive) { model.signOut() }
                            .disabled(model.busy)
                            .accessibilityIdentifier("signOutButton")
                    } else {
                        if model.sessionExpired {
                            Label(MobileError.sessionExpiredMessage, systemImage: "exclamationmark.triangle")
                                .font(.caption).foregroundStyle(.red)
                                .accessibilityIdentifier("sessionExpiredText")
                        } else {
                            Text("Signed out.").font(.caption).foregroundStyle(.secondary)
                        }
                        Button("Sign in") { showingSignIn = true }
                            .disabled(model.busy)
                            .accessibilityIdentifier("signInButton")
                    }
                    if let notice = model.accountNotice {
                        Text(notice).font(.caption).foregroundStyle(.secondary)
                            .accessibilityIdentifier("accountNoticeText")
                    }
                } header: {
                    Label("Account", systemImage: "person.crop.circle")
                } footer: {
                    Text(model.serverURL).font(.caption.monospaced())
                }

                if model.hasBearer && !model.locked {
                    ChangePassphraseSection(model: model)

                    Section {
                        NavigationLink {
                            InvitesView(model: model)
                        } label: {
                            Label("Invites", systemImage: "envelope")
                                .badge(model.invites.filter { $0.status == "active" }.count)
                        }
                        .accessibilityIdentifier("invitesLink")
                    }
                }

                if model.hasBearer {
                    Section {
                        Button("Delete account", role: .destructive) { confirmingDeleteAccount = true }
                            .disabled(model.busy)
                            .accessibilityIdentifier("deleteAccountButton")
                    } footer: {
                        Text("Deletes the account, every vault it owns on the server, and every signed-in device. The copies on this device are removed too.")
                    }
                }

                Section("About") {
                    LabeledContent("Version", value: Self.version)
                        .accessibilityIdentifier("appVersionText")
                    LabeledContent("Background refresh", value: Self.backgroundRefreshText)
                        .accessibilityIdentifier("backgroundRefreshText")
                }
            }
            .navigationTitle("Settings")
            .sheet(isPresented: $showingSignIn, onDismiss: { model.reloadBearerState() }) {
                SignInSheet(model: model)
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
        }
    }

    /// Whether iOS lets this app refresh in the background (Settings >
    /// General > Background App Refresh). Read-only: there is no toggle in
    /// the app.
    private static var backgroundRefreshText: String {
        switch UIApplication.shared.backgroundRefreshStatus {
        case .available: return "On"
        case .denied: return "Off in Settings"
        case .restricted: return "Restricted"
        @unknown default: return "Unknown"
        }
    }

    private static var version: String {
        let info = Bundle.main.infoDictionary
        let short = info?["CFBundleShortVersionString"] as? String ?? "?"
        let build = info?["CFBundleVersion"] as? String ?? "?"
        return "\(short) (\(build))"
    }
}

/// DESIGN.md §5 `Change passphrase`: current, new twice. The account key
/// stays the same; only its wrapping changes (spec §6.1).
struct ChangePassphraseSection: View {
    @ObservedObject var model: SyncModel
    @State private var current = ""
    @State private var next = ""
    @State private var again = ""
    @State private var status = ""
    @State private var busy = false

    private var tooShort: Bool { !next.isEmpty && next.count < SyncModel.minPassphraseChars }
    private var valid: Bool { !current.isEmpty && next.count >= SyncModel.minPassphraseChars && !again.isEmpty }

    var body: some View {
        Section {
            SecureField("Current passphrase", text: $current)
                .accessibilityIdentifier("currentPassphraseField")
            SecureField("New passphrase", text: $next)
                .accessibilityIdentifier("newPassphraseField")
            SecureField("Again", text: $again)
                .accessibilityIdentifier("newPassphraseConfirmField")
            Text("At least \(SyncModel.minPassphraseChars) characters.")
                .font(.caption)
                .foregroundStyle(tooShort ? Color.orange : Color.secondary)
            if !status.isEmpty {
                Text(status).font(.caption)
                    .foregroundStyle(status == "Passphrase changed." ? Color.secondary : Color.red)
            }
            Button(busy ? "Working…" : "Change passphrase") { submit() }
                .disabled(busy || !valid)
                .accessibilityIdentifier("changePassphraseButton")
        } header: {
            Label("Passphrase", systemImage: "key")
        } footer: {
            Text("Changes it for every device. There is no recovery if it is lost.")
        }
    }

    private func submit() {
        guard valid, !busy else { return }
        if next != again {
            status = "The passphrases do not match."
            return
        }
        busy = true
        status = ""
        let currentValue = current, nextValue = next
        Task { @MainActor in
            do {
                try await model.changePassphrase(current: currentValue, next: nextValue)
                current = ""; next = ""; again = ""
                status = "Passphrase changed."
            } catch {
                status = error.obsinkMessage
            }
            busy = false
        }
    }
}
