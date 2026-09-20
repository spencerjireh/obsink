import SwiftUI

/// The Home tab: one card per vault, the one-time Obsidian hint, and
/// `Add vault`. Status lives on the cards; the account lives in Settings.
struct HomeView: View {
    @ObservedObject var model: SyncModel
    @State private var showingAddVault = false

    /// The vault the hint names: the active one, else the first this build
    /// can sync.
    private var guidanceVaultName: String {
        let candidates = model.entries.filter { model.isOnDefaultServer($0) }
        let entry = candidates.first { $0.vaultID == model.activeVaultID } ?? candidates.first ?? model.entries.first
        return entry.map { $0.name.isEmpty ? "your vault" : $0.name } ?? "your vault"
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                LazyVStack(spacing: 12) {
                    if model.entries.isEmpty {
                        VStack(alignment: .leading, spacing: 10) {
                            Text("No vault yet. Tap Add vault.")
                                .foregroundStyle(.secondary)
                            if model.status != "Not synced" {
                                Text(model.status)
                                    .font(.footnote)
                                    .foregroundStyle(model.status.hasPrefix("Error") ? .red : .secondary)
                                    .accessibilityIdentifier("statusText")
                            }
                            Button("Add vault") { showingAddVault = true }
                                .buttonStyle(.borderedProminent)
                                .tint(Color("Amber"))
                                .foregroundStyle(Color("Ink"))
                                .accessibilityIdentifier("addVaultButton")
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(16)
                        .background(Color(.secondarySystemGroupedBackground))
                        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                    }
                    ForEach(model.entries) { entry in
                        VaultCard(model: model, entry: entry)
                    }
                    if !model.entries.isEmpty && !model.guidanceDismissed {
                        GuidanceCard(vaultName: guidanceVaultName) {
                            model.dismissGuidance()
                        }
                    }
                }
                .padding(16)
            }
            .background(Color(.systemGroupedBackground))
            .navigationTitle("ObSink")
            .toolbar {
                if !model.entries.isEmpty {
                    ToolbarItem(placement: .primaryAction) {
                        Button {
                            showingAddVault = true
                        } label: {
                            Label("Add vault", systemImage: "plus")
                        }
                        .accessibilityIdentifier("addVaultButton")
                    }
                }
            }
            .sheet(isPresented: $showingAddVault) {
                AddVaultFlow { model.addVault($0) }
            }
        }
    }
}
