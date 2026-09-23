import SwiftUI

/// One vault on the Vaults tab. A vault this device holds shows its state
/// line, last sync time, usage, `Sync now`, and links to the conflicts and
/// vault screens; the detail vault's card also carries the last result
/// (`statusText`), the stale banner and the failed files. A vault the
/// account owns elsewhere shows `Not on this device` with `Download`.
struct VaultCard: View {
    @ObservedObject var model: SyncModel
    let row: VaultRow

    private var state: VaultState { model.vaultStates[row.id] ?? VaultState() }
    private var isDetail: Bool { model.detailVaultID == row.id }
    private var canSync: Bool { !model.busy && model.canSync(row.id) }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                Text(row.name.isEmpty ? "Vault" : row.name)
                    .font(.headline)
                Spacer()
                NavigationLink {
                    VaultDetailView(model: model, vaultID: row.id)
                } label: {
                    Text("Manage")
                }
                .accessibilityIdentifier("manageVaultButton")
            }

            if let entry = row.entry {
                onDeviceBody(entry)
            } else {
                elsewhereBody
            }
        }
        .padding(16)
        .background(Color(.secondarySystemGroupedBackground))
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("vaultCard")
    }

    @ViewBuilder
    private func onDeviceBody(_ entry: VaultEntry) -> some View {
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

        if let usage = model.vaultUsageText(for: row.id) {
            Text(usage)
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("vaultUsageText")
        }

        if state.deletedOnServer {
            Text("This vault was deleted on the server. The copy on this device stays until you remove it.")
                .font(.caption).foregroundStyle(.secondary)
        } else if !state.hasStoredKey {
            Text("The key for this vault is not on this device. Download it again.")
                .font(.caption).foregroundStyle(.secondary)
        }

        if isDetail {
            // The last result; "Not synced" says nothing the state line
            // does not, but the identifier stays for the harness.
            Text(model.status)
                .font(.footnote)
                .foregroundStyle(model.status.hasPrefix("Error") ? .red : .secondary)
                .opacity(model.status == "Not synced" ? 0 : 1)
                .frame(height: model.status == "Not synced" ? 0 : nil)
                .accessibilityIdentifier("statusText")
            if state.staleDownloads > 0 && model.conflicts.isEmpty {
                staleBanner.accessibilityIdentifier("staleBanner")
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
            staleBanner
        }

        if !state.deletedOnServer && state.hasStoredKey {
            Button {
                model.sync(vaultID: entry.vaultID)
            } label: {
                Text(model.busy && isDetail ? "Working…" : "Sync now")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .tint(Color("Amber"))
            .foregroundStyle(Color("Ink"))
            .disabled(!canSync)
            .accessibilityIdentifier(isDetail || model.entries.count == 1 ? "syncButton" : "syncButton-\(entry.vaultID)")
        }

        if isDetail && (!model.failures.isEmpty || model.checkpointError != nil) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Failed this sync").font(.subheadline.weight(.semibold))
                if let error = model.checkpointError {
                    failureRow(tag: "FATAL", tagColor: .red, title: "Checkpoint", detail: error)
                }
                ForEach(model.failures, id: \.path) { failure in
                    failureRow(tag: failure.fatal ? "FATAL" : "skipped",
                               tagColor: failure.fatal ? .red : .orange,
                               title: failure.path, detail: failure.error)
                }
            }
        }
    }

    /// Spec §15.1: the account owns it; this device does not hold it.
    @ViewBuilder
    private var elsewhereBody: some View {
        HStack(spacing: 6) {
            Circle().fill(Color.secondary).frame(width: 8, height: 8)
            Text("Not on this device")
                .accessibilityIdentifier("vaultStateText")
            if let devices = row.summary?.devices, !devices.isEmpty {
                Text("·").foregroundStyle(.secondary)
                Text(devices.count == 1 ? "on 1 device" : "on \(devices.count) devices")
                    .foregroundStyle(.secondary)
            }
        }
        .font(.subheadline)
        if let usage = model.vaultUsageText(for: row.id) {
            Text(usage)
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("vaultUsageText")
        }
        if isDetail && model.status != "Not synced" {
            Text(model.status)
                .font(.footnote)
                .foregroundStyle(model.status.hasPrefix("Error") ? .red : .secondary)
                .accessibilityIdentifier("statusText")
        }
        Button {
            model.download(vaultID: row.id)
        } label: {
            Text(model.busy && isDetail ? "Working…" : "Download")
                .frame(maxWidth: .infinity)
        }
        .buttonStyle(.borderedProminent)
        .tint(Color("Amber"))
        .foregroundStyle(Color("Ink"))
        .disabled(model.busy || model.locked)
        .accessibilityIdentifier("downloadVaultButton")
    }

    private var staleBanner: some View {
        Label("\(state.staleDownloads) file(s) changed on another device. Sync before editing.",
              systemImage: "exclamationmark.triangle")
            .font(.footnote)
            .foregroundStyle(.orange)
    }

    private func failureRow(tag: String, tagColor: Color, title: String, detail: String) -> some View {
        HStack(alignment: .top, spacing: 6) {
            Text(tag)
                .font(.caption2.weight(.bold))
                .foregroundStyle(tagColor)
            VStack(alignment: .leading) {
                Text(title).font(.caption.monospaced())
                Text(detail).font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    private var dotColor: Color {
        switch state.phase {
        case .error: return .red
        case .syncing, .resolving: return .orange
        case .idle: break
        }
        if state.deletedOnServer { return .secondary }
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
