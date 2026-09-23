import SwiftUI

/// Three tabs (spec §15.6): Vaults, Devices, Settings, over one `SyncModel`.
/// Every activation runs the auto-sync routine: pending File Provider
/// writes, a server that is ahead, or a stale vault sync without a tap. A
/// server that speaks another wire format replaces everything with the
/// `Update ObSink` page (spec §15.5).
struct ContentView: View {
    @StateObject private var model = SyncModel()
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        Group {
            if model.protocolMismatch {
                UpdateRequiredView(serverProtocol: model.serverProtocol)
            } else {
                TabView {
                    VaultsView(model: model)
                        .tabItem { Label("Vaults", systemImage: "externaldrive") }
                    DevicesView(model: model)
                        .tabItem { Label("Devices", systemImage: "iphone") }
                    SettingsView(model: model)
                        .tabItem { Label("Settings", systemImage: "gearshape") }
                }
            }
        }
        .task { await model.autoSync(reason: .launch) }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active {
                Task { await model.autoSync(reason: .foreground) }
            }
        }
        .alert(item: $model.alert) { alert in
            Alert(title: Text(alert.title), message: Text(alert.message), dismissButton: .default(Text("OK")))
        }
    }
}

/// Spec §15.5: one page, nothing else runs.
struct UpdateRequiredView: View {
    let serverProtocol: UInt32?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Update ObSink").font(.title2.weight(.semibold))
            Text("This app is too old for the server. Download the current version.")
                .accessibilityIdentifier("updateRequiredText")
            if let serverProtocol {
                Text("Server wire format \(serverProtocol), this app \(mobileProtocolVersion()).")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Link("Get the current version", destination: URL(string: "https://obsink.spencerjireh.com")!)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .padding(24)
        .background(Color(.systemGroupedBackground))
    }
}

#Preview {
    ContentView()
}
