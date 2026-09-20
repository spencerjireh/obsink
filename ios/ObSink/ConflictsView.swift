import SwiftUI

/// The active vault's pending conflicts (OBS-24/25): one row per file with
/// the winner picker, a detail screen with both versions, and
/// `Apply resolutions`. Pops itself once nothing is left to decide so the
/// card's status line is visible again.
struct ConflictsView: View {
    @ObservedObject var model: SyncModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        Form {
            Section {
                if model.conflicts.isEmpty {
                    Text("No conflicts. Sync pauses here when both devices changed the same file.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                ForEach(model.conflicts, id: \.path) { conflict in
                    NavigationLink {
                        ConflictDetailView(
                            path: conflict.path,
                            model: model,
                            choice: choiceBinding(conflict.path)
                        )
                    } label: {
                        ConflictRow(conflict: conflict, choice: choiceBinding(conflict.path))
                    }
                    .accessibilityIdentifier("conflictRow")
                }
            } header: {
                Label("Conflicts", systemImage: "arrow.triangle.branch")
            }
            if !model.conflicts.isEmpty {
                Section {
                    Button(action: model.resolve) {
                        Text(model.busy ? "Working…" : "Apply resolutions")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Color("Amber"))
                    .foregroundStyle(Color("Ink"))
                    .disabled(model.busy)
                    .accessibilityIdentifier("applyResolutionsButton")
                }
            }
        }
        .navigationTitle("Conflicts")
        .navigationBarTitleDisplayMode(.inline)
        .onChange(of: model.busy) { _, busy in
            if !busy && model.conflicts.isEmpty { dismiss() }
        }
    }

    private func choiceBinding(_ path: String) -> Binding<MobileChoice> {
        Binding(
            get: { model.choices[path] ?? .keepLocal },
            set: { model.choices[path] = $0 }
        )
    }
}

struct ConflictRow: View {
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
                .accessibilityIdentifier("conflictRowTitle")
            VStack(alignment: .leading, spacing: 1) {
                Text("This device · \(conflict.localSize)B · \(Self.formatter.string(from: Date(timeIntervalSince1970: TimeInterval(conflict.localModified))))")
                Text("Other device · \(conflict.remoteSize)B · \(Self.formatter.string(from: Date(timeIntervalSince1970: TimeInterval(conflict.remoteModified))))")
            }
            .font(.caption).foregroundStyle(.secondary)
            ResolutionPicker(
                title: "Resolution",
                localDeleted: conflict.localDeleted,
                remoteDeleted: conflict.remoteDeleted,
                choice: $choice
            )
        }
        .padding(.vertical, 4)
    }
}

/// The winner picker for one conflict. "Keep both" needs two live versions;
/// with a deletion on one side the only question is which side wins, and the
/// labels say what that means.
struct ResolutionPicker: View {
    let title: String
    let localDeleted: Bool
    let remoteDeleted: Bool
    @Binding var choice: MobileChoice

    var body: some View {
        Picker(title, selection: $choice) {
            Text(localDeleted ? "Delete on server" : "Keep local").tag(MobileChoice.keepLocal)
            Text(remoteDeleted ? "Delete here" : "Keep remote").tag(MobileChoice.keepRemote)
            if !localDeleted && !remoteDeleted {
                Text("Keep both").tag(MobileChoice.keepBoth)
            }
        }
        .pickerStyle(.segmented)
        .onAppear {
            if choice == .keepBoth && (localDeleted || remoteDeleted) { choice = .keepLocal }
        }
    }
}

/// Per-conflict detail screen (OBS-24/25): segmented winner + read-only preview
/// of both versions' decrypted content.
struct ConflictDetailView: View {
    let path: String
    @ObservedObject var model: SyncModel
    @Binding var choice: MobileChoice

    private var conflict: MobileConflict? { model.conflicts.first { $0.path == path } }

    var body: some View {
        Form {
            Section("Resolution") {
                ResolutionPicker(
                    title: "Winner",
                    localDeleted: conflict?.localDeleted ?? false,
                    remoteDeleted: conflict?.remoteDeleted ?? false,
                    choice: $choice
                )
                .accessibilityIdentifier("winnerPicker")
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
