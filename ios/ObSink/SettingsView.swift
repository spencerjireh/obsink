import SwiftUI

/// The Settings tab: the account on this build's server (devices, invites,
/// sign out, delete account), sign-in when signed out, and the app version.
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

                if model.hasBearer && model.account != nil {
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
                }
            }
            .navigationTitle("Settings")
            .sheet(isPresented: $showingSignIn, onDismiss: { model.reloadBearerState() }) {
                SignInSheet()
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

    private static var version: String {
        let info = Bundle.main.infoDictionary
        let short = info?["CFBundleShortVersionString"] as? String ?? "?"
        let build = info?["CFBundleVersion"] as? String ?? "?"
        return "\(short) (\(build))"
    }
}
