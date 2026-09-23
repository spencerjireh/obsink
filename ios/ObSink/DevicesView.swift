import SwiftUI

/// The Devices tab (spec §15.3): one row per device of the account with
/// its platform, name (`Rename` in place), `Last seen`, the `This device`
/// tag, the vaults it holds, and `Sign out`. Signing out this device is the
/// ordinary sign-out; another row revokes that device on the server after a
/// confirmation (its folders and keys stay).
struct DevicesView: View {
    @ObservedObject var model: SyncModel
    @State private var revoking: MobileDevice?
    @State private var renamingID: String?
    @State private var draftName = ""
    @State private var renameStatus = ""

    private var devices: [MobileDevice] { model.account?.devices ?? [] }

    var body: some View {
        NavigationStack {
            List {
                if !model.hasBearer {
                    Text("Sign in to see your devices.").font(.caption).foregroundStyle(.secondary)
                } else if model.locked {
                    Text("Unlock the account to see your devices.").font(.caption).foregroundStyle(.secondary)
                }
                ForEach(devices, id: \.id) { device in
                    deviceRow(device)
                }
                if !renameStatus.isEmpty {
                    Text(renameStatus).font(.caption).foregroundStyle(.red)
                }
                if let notice = model.accountNotice {
                    Text(notice).font(.caption).foregroundStyle(.secondary)
                        .accessibilityIdentifier("accountNoticeText")
                }
            }
            .navigationTitle("Devices")
            .refreshable { model.refreshAccount() }
            .sheet(item: $revoking) { device in
                TypedConfirmationSheet(
                    title: "Sign out \(device.name)",
                    message: "\(device.name) is signed out on its next request. Its folders stay.",
                    expected: nil,
                    confirmLabel: "Sign out"
                ) {
                    model.revokeDevice(device.id)
                }
            }
        }
    }

    @ViewBuilder
    private func deviceRow(_ device: MobileDevice) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .center, spacing: 12) {
                Image(systemName: PlatformLabel.symbol(device.platform)).foregroundStyle(.secondary)
                VStack(alignment: .leading, spacing: 2) {
                    if renamingID == device.id {
                        TextField("Device name", text: $draftName)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .accessibilityIdentifier("deviceRenameField")
                    } else {
                        HStack(spacing: 6) {
                            Text(device.name)
                            if device.current {
                                Text("This device")
                                    .font(.caption2)
                                    .padding(.horizontal, 6)
                                    .padding(.vertical, 2)
                                    .background(Color.secondary.opacity(0.15), in: Capsule())
                                    .accessibilityIdentifier("deviceRowCurrentTag")
                            }
                        }
                    }
                    Text("\(PlatformLabel.text(device.platform)) · Last seen \(RelativeTime.relative(Date(timeIntervalSince1970: TimeInterval(device.lastSeen))).lowercased())")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text(vaultsLine(device))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
            }
            HStack(spacing: 12) {
                if renamingID == device.id {
                    Button("Save") { saveName(device) }
                        .disabled(model.busy || draftName.trimmingCharacters(in: .whitespaces).isEmpty)
                        .accessibilityIdentifier("deviceRenameButton")
                    Button("Cancel") { renamingID = nil; renameStatus = "" }
                } else {
                    Button("Rename") { draftName = device.name; renamingID = device.id }
                        .disabled(model.busy)
                        .accessibilityIdentifier("deviceRenameButton")
                }
                Spacer()
                Button("Sign out", role: .destructive) {
                    if device.current { model.signOut() } else { revoking = device }
                }
                .disabled(model.busy)
                .accessibilityIdentifier("deviceSignOutButton")
            }
            .buttonStyle(.bordered)
            .font(.caption)
        }
        .padding(.vertical, 4)
        .accessibilityIdentifier("deviceRow")
    }

    private func vaultsLine(_ device: MobileDevice) -> String {
        if device.vaultIds.isEmpty { return "No vaults on this device." }
        let names = device.vaultIds.map { id in model.rows.first { $0.id == id }?.name ?? id }
        return names.joined(separator: ", ")
    }

    private func saveName(_ device: MobileDevice) {
        let name = draftName.trimmingCharacters(in: .whitespaces)
        Task { @MainActor in
            do {
                try await model.renameDevice(device.id, name: name)
                renamingID = nil
                renameStatus = ""
            } catch {
                renameStatus = error.obsinkMessage
            }
        }
    }
}

extension MobileDevice: Identifiable {}
