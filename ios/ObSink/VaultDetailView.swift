import SwiftUI

/// `Manage vault`: what this device knows about one vault, and the two ways
/// to part with it. Remove keeps the vault on the server and drops the local
/// cache, item database, File Provider location, and key; Delete removes it
/// on the server for every device (typed vault name).
struct VaultDetailView: View {
    @ObservedObject var model: SyncModel
    let vaultID: String
    @Environment(\.dismiss) private var dismiss
    @State private var confirmingRemove = false
    @State private var confirmingDelete = false

    private var entry: VaultEntry? { model.entries.first { $0.vaultID == vaultID } }

    var body: some View {
        Form {
            if let entry {
                Section {
                    Text(entry.name).font(.headline)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Server").font(.caption).foregroundStyle(.secondary)
                        Text(entry.serverURL).font(.caption.monospaced()).textSelection(.enabled)
                    }
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Vault ID").font(.caption).foregroundStyle(.secondary)
                        Text(entry.vaultID).font(.caption.monospaced()).textSelection(.enabled)
                    }
                    if let usage = model.vaultUsageText(for: vaultID) {
                        Text(usage).font(.caption.monospaced()).foregroundStyle(.secondary)
                            .accessibilityIdentifier("vaultDetailUsageText")
                    }
                    if KeychainStore.load(account: vaultID) != nil {
                        Label("Key saved on this device", systemImage: "key.fill")
                            .font(.caption).foregroundStyle(.green)
                    }
                }
                Section {
                    Button("Remove from this device", role: .destructive) { confirmingRemove = true }
                        .disabled(model.busy)
                        .accessibilityIdentifier("removeVaultButton")
                    Button("Delete vault on server", role: .destructive) { confirmingDelete = true }
                        .disabled(model.busy || !model.hasBearer)
                        .accessibilityIdentifier("deleteVaultButton")
                } header: {
                    Label("Manage vault", systemImage: "externaldrive")
                }
            }
        }
        .navigationTitle(entry?.name ?? "Vault")
        .navigationBarTitleDisplayMode(.inline)
        .confirmationDialog(
            "Remove from this device",
            isPresented: $confirmingRemove,
            titleVisibility: .visible
        ) {
            Button("Remove from this device", role: .destructive) {
                Task { @MainActor in
                    await model.removeVaultLocally(vaultID)
                    dismiss()
                }
            }
        } message: {
            Text("The vault stays on the server. The copy on this device, its Files location, and the key are removed, so connecting again needs the passphrase.")
        }
        .sheet(isPresented: $confirmingDelete) {
            if let entry {
                TypedConfirmationSheet(
                    title: "Delete vault on server",
                    message: "This deletes \(entry.name) and all of its files on \(entry.serverURL) for every device.",
                    expected: entry.name,
                    confirmLabel: "Delete vault on server"
                ) {
                    model.deleteVaultOnServer(vaultID)
                    dismiss()
                }
            }
        }
    }
}
