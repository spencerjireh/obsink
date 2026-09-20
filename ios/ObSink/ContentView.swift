import SwiftUI

/// Two tabs: Home (a card per vault) and Settings (the account). One
/// `SyncModel` behind both.
struct ContentView: View {
    @StateObject private var model = SyncModel()
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        TabView {
            HomeView(model: model)
                .tabItem { Label("Home", systemImage: "house") }
            SettingsView(model: model)
                .tabItem { Label("Settings", systemImage: "gearshape") }
        }
        .task { model.checkStale() }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active {
                model.refreshAllPending()
                model.checkStale()
            }
        }
        .alert(item: $model.alert) { alert in
            Alert(title: Text(alert.title), message: Text(alert.message), dismissButton: .default(Text("OK")))
        }
    }
}

#Preview {
    ContentView()
}
