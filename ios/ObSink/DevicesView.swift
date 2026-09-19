import SwiftUI

/// Every session of the account (`GET /auth/me`). `Sign out` on another row
/// revokes that session on the server; the current device signs out from
/// the Account section.
struct DevicesView: View {
    @ObservedObject var model: SyncModel

    private static let formatter: DateFormatter = {
        let f = DateFormatter()
        f.dateStyle = .short
        f.timeStyle = .short
        return f
    }()

    private var devices: [MobileDevice] { model.account?.devices ?? [] }

    var body: some View {
        List {
            if devices.filter({ !$0.current }).isEmpty {
                Text("No other devices signed in.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            ForEach(devices, id: \.sessionId) { device in
                HStack(alignment: .center, spacing: 12) {
                    VStack(alignment: .leading, spacing: 2) {
                        HStack(spacing: 6) {
                            Text(device.deviceName)
                            if device.current {
                                Text("This device")
                                    .font(.caption2)
                                    .padding(.horizontal, 6)
                                    .padding(.vertical, 2)
                                    .background(Color.secondary.opacity(0.15), in: Capsule())
                                    .accessibilityIdentifier("deviceRowCurrentTag")
                            }
                        }
                        Text("since \(Self.formatter.string(from: Date(timeIntervalSince1970: TimeInterval(device.created))))")
                            .font(.caption.monospaced())
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    if !device.current {
                        Button("Sign out", role: .destructive) { model.revokeSession(device.sessionId) }
                            .buttonStyle(.bordered)
                            .disabled(model.busy)
                            .accessibilityIdentifier("deviceSignOutButton")
                    }
                }
                .padding(.vertical, 4)
                .accessibilityIdentifier("deviceRow")
            }
        }
        .navigationTitle("Devices")
        .refreshable { model.refreshAccount() }
    }
}
