import SwiftUI

/// The Vaults tab (spec §15.1): one card per vault of the account, the
/// vaults on this device first, then `Not on this device` cards with
/// `Download`; the one-time Obsidian hint; `Create vault` in the toolbar.
struct VaultsView: View {
    @ObservedObject var model: SyncModel
    @State private var showingCreate = false
    @State private var showingSignIn = false

    /// The vault the hint names: the detail one, else the first here.
    private var guidanceVaultName: String {
        let entry = model.entries.first { $0.vaultID == model.detailVaultID } ?? model.entries.first
        return entry.map { $0.name.isEmpty ? "your vault" : $0.name } ?? "your vault"
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                LazyVStack(spacing: 12) {
                    if !model.hasBearer {
                        signedOutCard
                    } else if model.locked {
                        lockedCard
                    } else if model.rows.isEmpty {
                        emptyCard
                    }
                    if model.hasBearer && !model.locked {
                        ForEach(model.rows) { row in
                            VaultCard(model: model, row: row)
                        }
                        if !model.entries.isEmpty && !model.guidanceDismissed {
                            GuidanceCard(vaultName: guidanceVaultName) {
                                model.dismissGuidance()
                            }
                        }
                    }
                }
                .padding(16)
            }
            .background(Color(.systemGroupedBackground))
            .navigationTitle("Vaults")
            .toolbar {
                if model.hasBearer && !model.locked {
                    ToolbarItem(placement: .primaryAction) {
                        Button {
                            showingCreate = true
                        } label: {
                            Label("Create vault", systemImage: "plus")
                        }
                        .accessibilityIdentifier("createVaultButton")
                    }
                }
            }
            .refreshable { await model.refreshAll() }
            .sheet(isPresented: $showingCreate) {
                CreateVaultSheet(model: model)
            }
            .sheet(isPresented: $showingSignIn, onDismiss: { model.reloadBearerState() }) {
                SignInSheet(model: model)
            }
        }
    }

    private var emptyCard: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("No vault yet. Tap Create vault.")
                .foregroundStyle(.secondary)
            if model.status != "Not synced" {
                Text(model.status)
                    .font(.footnote)
                    .foregroundStyle(model.status.hasPrefix("Error") ? .red : .secondary)
                    .accessibilityIdentifier("statusText")
            }
            Button("Create vault") { showingCreate = true }
                .buttonStyle(.borderedProminent)
                .tint(Color("Amber"))
                .foregroundStyle(Color("Ink"))
                .accessibilityIdentifier("createVaultButton")
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(16)
        .background(Color(.secondarySystemGroupedBackground))
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
    }

    private var signedOutCard: some View {
        VStack(alignment: .leading, spacing: 10) {
            if model.sessionExpired {
                Label(MobileError.sessionExpiredMessage, systemImage: "exclamationmark.triangle")
                    .font(.footnote).foregroundStyle(.red)
                    .accessibilityIdentifier("sessionExpiredText")
            } else {
                Text("Sign in to see your vaults.").foregroundStyle(.secondary)
            }
            Button("Sign in") { showingSignIn = true }
                .buttonStyle(.borderedProminent)
                .tint(Color("Amber"))
                .foregroundStyle(Color("Ink"))
                .accessibilityIdentifier("signInButton")
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(16)
        .background(Color(.secondarySystemGroupedBackground))
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
    }

    /// Spec §12.1: signed in without the account key at hand.
    private var lockedCard: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(model.hasServerKey
                 ? "Enter the account passphrase to see your vaults."
                 : "Set the account passphrase to start syncing.")
                .foregroundStyle(.secondary)
            Button(model.hasServerKey ? "Unlock" : "Set passphrase") { showingSignIn = true }
                .buttonStyle(.borderedProminent)
                .tint(Color("Amber"))
                .foregroundStyle(Color("Ink"))
                .accessibilityIdentifier(model.hasServerKey ? "unlockPromptButton" : "setPassphrasePromptButton")
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(16)
        .background(Color(.secondarySystemGroupedBackground))
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
    }
}

/// `Create vault`: a name; the folder is the app's own (spec §15.6).
struct CreateVaultSheet: View {
    @ObservedObject var model: SyncModel
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var status = ""
    @State private var busy = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    LabeledField("Vault name", text: $name, identifier: "createVaultNameField")
                } footer: {
                    Text("A random key, wrapped for your account. Every device that unlocks the account can download it.")
                }
                if !status.isEmpty {
                    Text(status).font(.caption).foregroundStyle(.red)
                        .accessibilityIdentifier("addVaultStatusText")
                }
                Section {
                    Button {
                        submit()
                    } label: {
                        Text(busy ? "Working…" : "Create vault")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Color("Amber"))
                    .foregroundStyle(Color("Ink"))
                    .disabled(busy || name.trimmingCharacters(in: .whitespaces).isEmpty)
                    .accessibilityIdentifier("createVaultSubmitButton")
                }
            }
            .navigationTitle("Create vault")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }.disabled(busy)
                }
            }
        }
    }

    private func submit() {
        busy = true
        status = ""
        let vaultName = name.trimmingCharacters(in: .whitespaces)
        Task { @MainActor in
            do {
                try await model.createVault(name: vaultName)
                dismiss()
            } catch {
                status = error.obsinkMessage
                busy = false
            }
        }
    }
}
