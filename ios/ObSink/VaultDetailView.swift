import SwiftUI

/// The vault page (spec §15.2): the name with `Rename` in place, the
/// devices that hold the vault, this vault's activity, `History` (file
/// history and recently deleted), and `Manage vault` with the two ways to
/// part with it. Remove keeps the vault on the server and drops the local
/// cache, item database, File Provider location and key; Delete removes it
/// on the server for every device (typed vault name).
struct VaultDetailView: View {
    @ObservedObject var model: SyncModel
    let vaultID: String
    @Environment(\.dismiss) private var dismiss
    @State private var confirmingRemove = false
    @State private var confirmingDelete = false
    @State private var renaming = false
    @State private var draftName = ""
    @State private var renameStatus = ""

    private var row: VaultRow? { model.rows.first { $0.id == vaultID } }
    private var entry: VaultEntry? { row?.entry }
    private var state: VaultState { model.vaultStates[vaultID] ?? VaultState() }

    var body: some View {
        Form {
            if let row {
                Section {
                    nameRow(row)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Vault ID").font(.caption).foregroundStyle(.secondary)
                        Text(row.id).font(.caption.monospaced()).textSelection(.enabled)
                    }
                    if let usage = model.vaultUsageText(for: vaultID) {
                        Text(usage).font(.caption.monospaced()).foregroundStyle(.secondary)
                            .accessibilityIdentifier("vaultDetailUsageText")
                    }
                    if row.onDevice {
                        if state.hasStoredKey {
                            Label("Key saved on this device", systemImage: "key.fill")
                                .font(.caption).foregroundStyle(.green)
                        }
                        if state.deletedOnServer {
                            Label("Deleted on the server", systemImage: "exclamationmark.triangle")
                                .font(.caption).foregroundStyle(.orange)
                        }
                    } else {
                        Text("Not on this device").font(.caption).foregroundStyle(.secondary)
                        Button("Download") { model.download(vaultID: vaultID) }
                            .disabled(model.busy || model.locked)
                            .accessibilityIdentifier("downloadVaultButton")
                    }
                }

                Section {
                    let devices = row.summary?.devices ?? []
                    if devices.isEmpty {
                        Text("No device holds this vault yet.").font(.caption).foregroundStyle(.secondary)
                    }
                    ForEach(devices, id: \.id) { device in
                        VaultDeviceRow(device: device, revision: row.summary?.revision ?? 0,
                                       current: device.id == model.deviceID)
                    }
                } header: {
                    Label("Devices", systemImage: "iphone")
                }

                if row.onDevice {
                    Section {
                        let events = model.activity(for: vaultID)
                        if events.isEmpty {
                            Text("Run a sync to see uploads and downloads.").font(.caption).foregroundStyle(.secondary)
                        }
                        ForEach(events.prefix(20)) { event in
                            HStack(alignment: .top, spacing: 8) {
                                Text(RelativeTime.relative(event.at))
                                    .font(.caption).foregroundStyle(.secondary)
                                    .frame(width: 92, alignment: .leading)
                                Text(event.line)
                                    .font(.caption.monospaced())
                                    .foregroundStyle(event.kind == .error ? Color.red : Color.primary)
                            }
                            .accessibilityIdentifier("activityRow")
                        }
                    } header: {
                        Label("Activity", systemImage: "clock")
                    }

                    if state.hasStoredKey && !state.deletedOnServer {
                        Section {
                            NavigationLink {
                                HistoryView(model: model, vaultID: vaultID)
                            } label: {
                                Label("History", systemImage: "clock.arrow.circlepath")
                            }
                            .accessibilityIdentifier("historyLink")
                        }
                    }

                    Section {
                        Button("Remove from this device", role: .destructive) { confirmingRemove = true }
                            .disabled(model.busy)
                            .accessibilityIdentifier("removeVaultButton")
                        if !state.deletedOnServer {
                            Button("Delete vault on server", role: .destructive) { confirmingDelete = true }
                                .disabled(model.busy || !model.hasBearer)
                                .accessibilityIdentifier("deleteVaultButton")
                        }
                    } header: {
                        Label("Manage vault", systemImage: "externaldrive")
                    }
                } else {
                    Section {
                        Button("Delete vault on server", role: .destructive) { confirmingDelete = true }
                            .disabled(model.busy || !model.hasBearer)
                            .accessibilityIdentifier("deleteVaultButton")
                    } header: {
                        Label("Manage vault", systemImage: "externaldrive")
                    }
                }
            }
        }
        .navigationTitle(row?.name ?? "Vault")
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
            Text("The vault stays on the server and can be downloaded again. The copy on this device and its Files location are removed.")
        }
        .sheet(isPresented: $confirmingDelete) {
            if let row {
                TypedConfirmationSheet(
                    title: "Delete vault on server",
                    message: "This deletes \(row.name) and all of its files on \(model.serverURL) for every device. The copy on this device is removed too.",
                    expected: row.name,
                    confirmLabel: "Delete vault on server"
                ) {
                    model.deleteVaultOnServer(vaultID)
                    dismiss()
                }
            }
        }
    }

    /// The name with `Rename` in place (DESIGN.md §5): one button reads
    /// `Rename`, then `Save`.
    @ViewBuilder
    private func nameRow(_ row: VaultRow) -> some View {
        if renaming {
            VStack(alignment: .leading, spacing: 6) {
                TextField("Vault name", text: $draftName)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .accessibilityIdentifier("renameVaultField")
                HStack {
                    Button("Save") { saveName() }
                        .disabled(model.busy || draftName.trimmingCharacters(in: .whitespaces).isEmpty
                                  || draftName.trimmingCharacters(in: .whitespaces) == row.name)
                        .accessibilityIdentifier("renameVaultButton")
                    Button("Cancel") { renaming = false; renameStatus = "" }
                }
                .font(.subheadline)
                if !renameStatus.isEmpty {
                    Text(renameStatus).font(.caption).foregroundStyle(.red)
                }
            }
        } else {
            HStack {
                Text(row.name).font(.headline)
                Spacer()
                if model.hasBearer && !model.locked {
                    Button("Rename") { draftName = row.name; renaming = true }
                        .font(.subheadline)
                        .disabled(model.busy)
                        .accessibilityIdentifier("renameVaultButton")
                }
            }
        }
    }

    private func saveName() {
        let name = draftName.trimmingCharacters(in: .whitespaces)
        Task { @MainActor in
            do {
                try await model.renameVault(vaultID: vaultID, name: name)
                renaming = false
                renameStatus = ""
            } catch {
                renameStatus = error.obsinkMessage
            }
        }
    }
}

/// One device that holds a vault (spec §15.2): platform, name, `Last
/// synced`, and how far behind the server it is.
struct VaultDeviceRow: View {
    let device: MobileVaultDevice
    let revision: UInt64
    let current: Bool

    private var behind: String {
        guard let last = device.lastRevision else { return "" }
        let gap = revision > last ? revision - last : 0
        if gap == 0 { return " · Up to date" }
        return " · \(gap) revision\(gap == 1 ? "" : "s") behind"
    }

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: PlatformLabel.symbol(device.platform)).foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(device.name)
                    if current {
                        Text("This device")
                            .font(.caption2)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(Color.secondary.opacity(0.15), in: Capsule())
                    }
                }
                Text("\(PlatformLabel.text(device.platform)) · \(device.lastSynced.map { "Last synced " + RelativeTime.relative(Date(timeIntervalSince1970: TimeInterval($0))).lowercased() } ?? "Never synced")\(behind)")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 2)
        .accessibilityIdentifier("vaultDeviceRow")
    }
}
