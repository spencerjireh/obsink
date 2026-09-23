import SwiftUI

/// Spec §8.2 and §9.3 on iOS: `File history` (pick a file, its versions,
/// preview, restore) and `Recently deleted` (tombstones with real paths,
/// preview, restore). A restore writes into the vault's cache; the next sync
/// uploads it through the ordinary conflict-gated path.
struct HistoryView: View {
    @ObservedObject var model: SyncModel
    let vaultID: String

    @State private var files: [String] = []
    @State private var file = ""
    @State private var versions: [MobileVersion]?
    @State private var trash: [MobileTrashEntry]?
    @State private var preview: (key: String, text: String?)?
    @State private var status = ""
    @State private var working = false

    private var disabled: Bool { working || model.busy }

    var body: some View {
        Form {
            Section {
                Picker("File", selection: $file) {
                    Text("Pick a file").tag("")
                    ForEach(files, id: \.self) { path in
                        Text(path).tag(path)
                    }
                }
                .accessibilityIdentifier("historyFilePicker")
                .disabled(disabled)
                if file.isEmpty {
                    Text("Pick a file to see its versions.").font(.caption).foregroundStyle(.secondary)
                } else if let versions {
                    if versions.isEmpty {
                        Text("No earlier versions kept.").font(.caption).foregroundStyle(.secondary)
                    }
                    ForEach(versions, id: \.name) { version in
                        historyRow(version)
                    }
                } else {
                    ProgressView()
                }
            } header: {
                Text("File history")
            }

            Section {
                if let trash {
                    if trash.isEmpty {
                        Text("Nothing deleted in the last 30 days.").font(.caption).foregroundStyle(.secondary)
                    }
                    ForEach(trash, id: \.path) { entry in
                        trashRow(entry)
                    }
                } else {
                    ProgressView()
                }
            } header: {
                Text("Recently deleted")
            }

            if !status.isEmpty {
                Section {
                    Text(status).font(.caption)
                        .foregroundStyle(status.hasPrefix("Restored") ? Color.secondary : Color.red)
                        .accessibilityIdentifier("statusText")
                }
            }
        }
        .navigationTitle("History")
        .navigationBarTitleDisplayMode(.inline)
        .task { await load() }
        .onChange(of: file) { _, path in
            preview = nil
            versions = nil
            guard !path.isEmpty else { return }
            Task { @MainActor in
                do { versions = try await model.versions(for: vaultID, path: path) } catch { status = error.obsinkMessage }
            }
        }
    }

    private func load() async {
        files = model.listFiles(for: vaultID)
        do {
            trash = try await model.trash(for: vaultID)
        } catch {
            trash = []
            status = error.obsinkMessage
        }
    }

    @ViewBuilder
    private func historyRow(_ version: MobileVersion) -> some View {
        let key = "version:\(version.name)"
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("\(RelativeTime.relative(Date(timeIntervalSince1970: TimeInterval(version.ts)))) · \(SyncModel.formatBytes(version.size))")
                    .font(.subheadline)
                Spacer()
                Button("Preview") {
                    togglePreview(key) { try await model.versionText(for: vaultID, path: file, name: version.name) }
                }
                .disabled(disabled)
                .accessibilityIdentifier("previewButton")
                Button("Restore") {
                    run {
                        try await model.restoreVersion(vaultID: vaultID, path: file, name: version.name)
                        status = "Restored \(file). Sync to upload it."
                    }
                }
                .disabled(disabled)
                .accessibilityIdentifier("restoreButton")
            }
            .buttonStyle(.bordered)
            .font(.caption)
            if let preview, preview.key == key {
                previewText(preview.text)
            }
        }
        .accessibilityIdentifier("historyRow")
    }

    @ViewBuilder
    private func trashRow(_ entry: MobileTrashEntry) -> some View {
        let key = "trash:\(entry.path)"
        VStack(alignment: .leading, spacing: 6) {
            Text(entry.path).font(.subheadline.monospaced())
            HStack {
                Text("deleted \(RelativeTime.relative(Date(timeIntervalSince1970: TimeInterval(entry.deletedAt))).lowercased())")
                    .font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("Preview") {
                    togglePreview(key) { try await model.trashText(for: vaultID, path: entry.path) }
                }
                .disabled(disabled)
                .accessibilityIdentifier("previewButton")
                Button("Restore") {
                    run {
                        try await model.restoreTrash(vaultID: vaultID, path: entry.path)
                        status = "Restored \(entry.path). Sync to upload it."
                    }
                }
                .disabled(disabled)
                .accessibilityIdentifier("restoreButton")
            }
            .buttonStyle(.bordered)
            .font(.caption)
            if let preview, preview.key == key {
                previewText(preview.text)
            }
        }
        .accessibilityIdentifier("trashRow")
    }

    @ViewBuilder
    private func previewText(_ text: String?) -> some View {
        if let text {
            Text(text.isEmpty ? "Empty file." : text)
                .font(.system(.caption, design: .monospaced))
                .textSelection(.enabled)
        } else {
            Text("No preview for this file.").font(.caption).foregroundStyle(.secondary)
        }
    }

    private func togglePreview(_ key: String, load: @escaping () async throws -> String?) {
        if preview?.key == key {
            preview = nil
            return
        }
        run {
            let text = try await load()
            preview = (key, text)
        }
    }

    private func run(_ action: @escaping () async throws -> Void) {
        working = true
        status = ""
        Task { @MainActor in
            do { try await action() } catch { status = error.obsinkMessage }
            working = false
        }
    }
}
