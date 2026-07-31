import SwiftUI

struct ContentView: View {
    @StateObject private var model = SyncModel()
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        NavigationStack {
            Form {
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
            }
            .navigationTitle("ObSink")
            .onChange(of: scenePhase) { _, phase in
                if phase == .active { model.refreshPending() }
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

#Preview {
    ContentView()
}
