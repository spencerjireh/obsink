import SwiftUI

struct ContentView: View {
    @StateObject private var model = SyncModel()
    @Environment(\.scenePhase) private var scenePhase
    @State private var showingAddVault = false

    var body: some View {
        NavigationStack {
            Form {
                Section("Vaults") {
                    if model.entries.isEmpty {
                        Text("No vault configured. Tap “Add Vault…”.")
                            .font(.caption).foregroundStyle(.secondary)
                    } else {
                        Picker("Active", selection: Binding(
                            get: { model.activeVaultID },
                            set: { model.selectVault($0) }
                        )) {
                            ForEach(model.entries) { entry in
                                Text(entry.name).tag(entry.vaultID)
                            }
                        }
                    }
                    Button("Add Vault…") { showingAddVault = true }
                }

                Section("Status") {
                    Text(model.status)
                        .font(.callout)
                        .foregroundStyle(model.status.hasPrefix("Error") ? .red : .primary)
                    if model.pendingLocalChanges > 0 {
                        Label("\(model.pendingLocalChanges) local change\(model.pendingLocalChanges == 1 ? "" : "s") — Sync to push", systemImage: "arrow.up.circle")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                    Button(action: model.sync) {
                        HStack {
                            if model.busy { ProgressView() }
                            Text(model.busy ? "Working…" : "Sync Now")
                        }
                    }
                    .disabled(model.busy || model.vaultID.isEmpty || (model.passphrase.isEmpty && !model.hasStoredKey))

                    if model.busy, let p = model.progress {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("\(p.phase)\(p.path.map { " · \($0)" } ?? "")")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            if p.total > 0 {
                                ProgressView(value: Double(p.current), total: Double(p.total))
                            }
                        }
                    }
                }

                Section("Vault") {
                    LabeledField("Worker URL", text: $model.workerURL)
                    LabeledField("API key", text: $model.apiKey)
                    LabeledField("Vault ID", text: $model.vaultID)
                    SecureField(model.hasStoredKey ? "Passphrase (saved — not needed)" : "Passphrase", text: $model.passphrase)
                    if model.hasStoredKey {
                        Text("Key saved in Keychain for this vault.")
                            .font(.caption).foregroundStyle(.green)
                    }
                }

                if !model.conflicts.isEmpty {
                    Section("Conflicts") {
                        ForEach(model.conflicts, id: \.path) { conflict in
                            NavigationLink {
                                ConflictDetailView(
                                    path: conflict.path,
                                    model: model,
                                    choice: Binding(
                                        get: { model.choices[conflict.path] ?? .keepLocal },
                                        set: { model.choices[conflict.path] = $0 }
                                    )
                                )
                            } label: {
                                ConflictRow(conflict: conflict, choice: Binding(
                                    get: { model.choices[conflict.path] ?? .keepLocal },
                                    set: { model.choices[conflict.path] = $0 }
                                ))
                            }
                        }
                        Button("Apply Resolutions", action: model.resolve)
                            .disabled(model.busy)
                    }
                }

                if !model.failures.isEmpty {
                    Section("Failed this sync") {
                        ForEach(model.failures, id: \.self) { failure in
                            VStack(alignment: .leading, spacing: 2) {
                                HStack(spacing: 6) {
                                    Text(failure.fatal ? "FATAL" : "skipped")
                                        .font(.caption2.weight(.bold))
                                        .foregroundStyle(failure.fatal ? .red : .orange)
                                    Text(failure.path).font(.caption)
                                }
                                Text(failure.error)
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            }
            .navigationTitle("ObSink")
            .onChange(of: scenePhase) { _, phase in
                if phase == .active { model.refreshPending() }
            }
            .sheet(isPresented: $showingAddVault) {
                AddVaultView { model.addVault($0) }
            }
        }
    }
}

private struct LabeledField: View {
    let label: String
    @Binding var text: String

    init(_ label: String, text: Binding<String>) {
        self.label = label
        self._text = text
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label).font(.caption).foregroundStyle(.secondary)
            TextField(label, text: $text)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
        }
    }
}

private struct ConflictRow: View {
    let conflict: MobileConflict
    @Binding var choice: MobileChoice

    private static let formatter: DateFormatter = {
        let f = DateFormatter()
        f.dateStyle = .short
        f.timeStyle = .short
        return f
    }()

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(conflict.path).font(.subheadline.weight(.semibold))
            VStack(alignment: .leading, spacing: 1) {
                Text("This device · \(conflict.localSize)B · \(Self.formatter.string(from: Date(timeIntervalSince1970: TimeInterval(conflict.localModified))))")
                Text("Other device · \(conflict.remoteSize)B · \(Self.formatter.string(from: Date(timeIntervalSince1970: TimeInterval(conflict.remoteModified))))")
            }
            .font(.caption).foregroundStyle(.secondary)
            Picker("Resolution", selection: $choice) {
                Text("Keep local").tag(MobileChoice.keepLocal)
                Text("Keep remote").tag(MobileChoice.keepRemote)
                Text("Keep both").tag(MobileChoice.keepBoth)
            }
            .pickerStyle(.segmented)
        }
        .padding(.vertical, 4)
    }
}

/// Per-conflict detail screen (OBS-24/25): segmented winner + read-only preview
/// of both versions' decrypted content.
private struct ConflictDetailView: View {
    let path: String
    @ObservedObject var model: SyncModel
    @Binding var choice: MobileChoice

    var body: some View {
        Form {
            Section("Resolution") {
                Picker("Winner", selection: $choice) {
                    Text("Keep local").tag(MobileChoice.keepLocal)
                    Text("Keep remote").tag(MobileChoice.keepRemote)
                    Text("Keep both").tag(MobileChoice.keepBoth)
                }
                .pickerStyle(.segmented)
            }
            Section("This device") {
                previewText(model.previews[path]?.localText, deleted: model.previews[path]?.localDeleted ?? false)
            }
            Section("Other device") {
                previewText(model.previews[path]?.remoteText, deleted: model.previews[path]?.remoteDeleted ?? false)
            }
        }
        .navigationTitle(path)
    }

    @ViewBuilder
    private func previewText(_ text: String?, deleted: Bool) -> some View {
        if deleted {
            Text("(deleted on this side)").font(.caption).foregroundStyle(.secondary)
        } else if let text {
            Text(text.isEmpty ? "(empty)" : text)
                .font(.system(.body, design: .monospaced))
                .textSelection(.enabled)
        } else {
            Text("Loading…").foregroundStyle(.secondary)
        }
    }
}

/// Add a vault: create a new one or connect to an existing one on the Worker
/// (spec §12.1/§12.2). Uses the `create_vault` / `list_vaults` facade methods and
/// stores the derived key in the Keychain so later syncs need no passphrase.
struct AddVaultView: View {
    var onAdd: (VaultEntry) -> Void
    @Environment(\.dismiss) private var dismiss

    @State private var mode: Mode = .create
    @State private var workerURL = "https://"
    @State private var apiKey = ""
    @State private var name = ""
    @State private var passphrase = ""
    @State private var available: [MobileVaultSummary] = []
    @State private var pickedVaultID: String?
    @State private var status = ""
    @State private var busy = false

    private enum Mode: String, CaseIterable, Identifiable {
        case create = "Create", connect = "Connect"
        var id: String { rawValue }
    }

    var body: some View {
        NavigationStack {
            Form {
                Section("Server") {
                    LabeledField("Worker URL", text: $workerURL)
                    LabeledField("API key", text: $apiKey)
                }
                Section {
                    Picker("", selection: $mode) {
                        ForEach(Mode.allCases) { Text($0.rawValue).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                }
                if mode == .create {
                    Section("New vault") {
                        LabeledField("Vault name", text: $name)
                    }
                } else {
                    Section("Connect") {
                        Button("List vaults") { fetchVaults() }
                            .disabled(busy || apiKey.isEmpty)
                        if available.isEmpty {
                            Text("Tap “List vaults”, then pick one.")
                                .font(.caption).foregroundStyle(.secondary)
                        } else {
                            Picker("Vault", selection: $pickedVaultID) {
                                Text("—").tag(String?.none)
                                ForEach(available, id: \.id) { v in
                                    Text(v.name).tag(Optional(v.id))
                                }
                            }
                        }
                    }
                }
                Section {
                    SecureField("Passphrase", text: $passphrase)
                }
                if !status.isEmpty {
                    Text(status).font(.caption).foregroundStyle(.red)
                }
                Section {
                    Button(mode == .create ? "Create Vault" : "Connect Vault") { submit() }
                        .disabled(busy || workerURL.isEmpty || apiKey.isEmpty || passphrase.isEmpty
                                  || (mode == .create ? name.isEmpty : pickedVaultID == nil))
                }
            }
            .navigationTitle("Add Vault")
            .toolbar { Button("Cancel") { dismiss() } }
        }
    }

    private func fetchVaults() {
        busy = true
        status = ""
        let url = workerURL, key = apiKey
        Task.detached {
            do {
                let vaults = try listVaults(workerUrl: url, apiKey: key)
                await MainActor.run {
                    available = vaults
                    pickedVaultID = nil
                    busy = false
                    if vaults.isEmpty { status = "No vaults found at this Worker." }
                }
            } catch {
                await MainActor.run { status = error.localizedDescription; busy = false }
            }
        }
    }

    private func submit() {
        busy = true
        status = ""
        let url = workerURL, key = apiKey, name = self.name, pass = passphrase
        let mode = self.mode, picked = pickedVaultID
        let availableNames = available
        Task.detached {
            do {
                switch mode {
                case .create:
                    let summary = try createVault(workerUrl: url, apiKey: key, name: name)
                    let derived = try deriveMasterKey(passphrase: pass, vaultId: summary.id)
                    _ = KeychainStore.save(derived, account: summary.id)
                    await MainActor.run {
                        onAdd(VaultEntry(workerURL: url, apiKey: key, vaultID: summary.id, name: summary.name))
                        dismiss()
                    }
                case .connect:
                    guard let vid = picked else {
                        await MainActor.run { status = "Pick a vault first."; busy = false }
                        return
                    }
                    let derived = try deriveMasterKey(passphrase: pass, vaultId: vid)
                    _ = KeychainStore.save(derived, account: vid)
                    let vname = availableNames.first { $0.id == vid }?.name ?? vid
                    await MainActor.run {
                        onAdd(VaultEntry(workerURL: url, apiKey: key, vaultID: vid, name: vname))
                        dismiss()
                    }
                }
            } catch {
                await MainActor.run { status = error.localizedDescription; busy = false }
            }
        }
    }
}

#Preview {
    ContentView()
}
