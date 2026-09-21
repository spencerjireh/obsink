import SwiftUI

/// One vault on the Home tab: its state line, last sync time, usage, the
/// passphrase field until the key is stored, `Sync now`, and links to the
/// conflicts and vault screens. The active vault's card also carries the
/// last result (`statusText`), the stale banner and the failed files.
struct VaultCard: View {
    @ObservedObject var model: SyncModel
    let entry: VaultEntry
    @State private var passphrase = ""

    private var state: VaultState { model.vaultStates[entry.vaultID] ?? VaultState() }
    private var isActive: Bool { model.activeVaultID == entry.vaultID }
    private var syncable: Bool { model.canSync(entry.vaultID) }
    private var needsPassphrase: Bool { !state.isForeign && !state.hasStoredKey }
    private var canSync: Bool {
        !model.busy && syncable && (!needsPassphrase || !passphrase.isEmpty)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                Text(entry.name.isEmpty ? "Vault" : entry.name)
                    .font(.headline)
                Spacer()
                NavigationLink {
                    VaultDetailView(model: model, vaultID: entry.vaultID)
                } label: {
                    Text("Manage")
                }
                .accessibilityIdentifier("manageVaultButton")
            }

            HStack(spacing: 6) {
                Circle().fill(dotColor).frame(width: 8, height: 8)
                Text(VaultState.statusLine(state))
                    .accessibilityIdentifier("vaultStateText")
                Text("·").foregroundStyle(.secondary)
                Text(RelativeTime.lastSynced(state.lastSyncedAt))
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("lastSyncedText")
            }
            .font(.subheadline)

            if let usage = model.vaultUsageText(for: entry.vaultID) {
                Text(usage)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("vaultUsageText")
            }

            if state.isForeign {
                Text("Configured for \(entry.serverURL), not this build's server. Remove it from this device, then connect it again.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if isActive {
                // The last result; "Not synced" says nothing the state line
                // does not, but the identifier stays for the harness.
                Text(model.status)
                    .font(.footnote)
                    .foregroundStyle(model.status.hasPrefix("Error") ? .red : .secondary)
                    .opacity(model.status == "Not synced" ? 0 : 1)
                    .frame(height: model.status == "Not synced" ? 0 : nil)
                    .accessibilityIdentifier("statusText")
                if state.staleDownloads > 0 && model.conflicts.isEmpty {
                    Label("\(state.staleDownloads) file(s) changed on another device. Sync before editing.",
                          systemImage: "exclamationmark.triangle")
                        .font(.footnote)
                        .foregroundStyle(.orange)
                        .accessibilityIdentifier("staleBanner")
                }
                if !model.conflicts.isEmpty {
                    NavigationLink {
                        ConflictsView(model: model)
                    } label: {
                        Label("Resolve \(model.conflicts.count == 1 ? "1 conflict" : "\(model.conflicts.count) conflicts")",
                              systemImage: "arrow.triangle.branch")
                    }
                    .accessibilityIdentifier("resolveConflictsLink")
                }
                if model.busy, let progress = model.progress {
                    VStack(alignment: .leading, spacing: 2) {
                        if progress.total > 0 {
                            ProgressView(value: Double(progress.current), total: Double(progress.total))
                        } else {
                            ProgressView()
                        }
                        Text(progressLine(progress)).font(.caption).foregroundStyle(.secondary)
                    }
                }
            } else if state.staleDownloads > 0 {
                Label("\(state.staleDownloads) file(s) changed on another device. Sync before editing.",
                      systemImage: "exclamationmark.triangle")
                    .font(.footnote)
                    .foregroundStyle(.orange)
            }

            if needsPassphrase {
                SecureField("Passphrase", text: $passphrase)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("passphraseField")
            }

            if !state.isForeign {
                Button {
                    model.sync(vaultID: entry.vaultID, passphrase: passphrase)
                } label: {
                    Text(model.busy && isActive ? "Working…" : "Sync now")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(Color("Amber"))
                .foregroundStyle(Color("Ink"))
                .disabled(!canSync)
                .accessibilityIdentifier(isActive || model.entries.count == 1 ? "syncButton" : "syncButton-\(entry.vaultID)")
            }

            if isActive && (!model.failures.isEmpty || model.checkpointError != nil) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Failed this sync").font(.subheadline.weight(.semibold))
                    if let error = model.checkpointError {
                        HStack(alignment: .top, spacing: 6) {
                            Text("FATAL")
                                .font(.caption2.weight(.bold))
                                .foregroundStyle(.red)
                            VStack(alignment: .leading) {
                                Text("Checkpoint").font(.caption.monospaced())
                                Text(error).font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                    ForEach(model.failures, id: \.path) { failure in
                        HStack(alignment: .top, spacing: 6) {
                            Text(failure.fatal ? "FATAL" : "skipped")
                                .font(.caption2.weight(.bold))
                                .foregroundStyle(failure.fatal ? .red : .orange)
                            VStack(alignment: .leading) {
                                Text(failure.path).font(.caption.monospaced())
                                Text(failure.error).font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            }
        }
        .padding(16)
        .background(Color(.secondarySystemGroupedBackground))
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("vaultCard")
        .onChange(of: state.hasStoredKey) { _, stored in
            if stored { passphrase = "" }
        }
    }

    private var dotColor: Color {
        switch state.phase {
        case .error: return .red
        case .syncing, .resolving: return .orange
        case .idle: break
        }
        if state.isForeign { return .secondary }
        if state.conflicts > 0 { return .red }
        if !state.hasStoredKey || state.staleDownloads > 0 || state.pendingLocal > 0 { return .orange }
        return .green
    }

    private func progressLine(_ progress: SyncProgressInfo) -> String {
        var line = progress.phase
        if let path = progress.path { line += " · \(path)" }
        if progress.total > 0 { line += " (\(progress.current)/\(progress.total))" }
        return line
    }
}

/// One-time hint after the first vault: how Obsidian opens it.
struct GuidanceCard: View {
    let vaultName: String
    let onDismiss: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Open in Obsidian", systemImage: "folder")
                .font(.headline)
            Text("In Obsidian, choose Open folder as vault, then Browse, then ObSink > \(vaultName).")
                .font(.subheadline)
                .foregroundStyle(.secondary)
            Button("Got it", action: onDismiss)
                .accessibilityIdentifier("guidanceDismissButton")
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(16)
        .background(Color(.secondarySystemGroupedBackground))
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("guidanceCard")
    }
}
