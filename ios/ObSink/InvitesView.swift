import SwiftUI

/// Invites this account minted (`GET /auth/invites`) and the button to mint
/// one more. `Copy` is offered on active codes only.
struct InvitesView: View {
    @ObservedObject var model: SyncModel

    private static let formatter: DateFormatter = {
        let f = DateFormatter()
        f.dateStyle = .short
        f.timeStyle = .short
        return f
    }()

    var body: some View {
        List {
            Section {
                Button("Invite someone") { model.createInvite() }
                    .disabled(model.busy)
                    .accessibilityIdentifier("inviteButton")
                if let invite = model.issuedInvite {
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Invite code").font(.caption).foregroundStyle(.secondary)
                            Text(invite.code).font(.body.monospaced()).textSelection(.enabled)
                                .accessibilityIdentifier("inviteCodeText")
                        }
                        Spacer()
                        Button("Copy") { UIPasteboard.general.string = invite.code }
                    }
                }
            }
            Section {
                if model.invites.isEmpty {
                    Text("No invites yet.").font(.caption).foregroundStyle(.secondary)
                }
                ForEach(model.invites, id: \.code) { invite in
                    HStack(spacing: 12) {
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Text(invite.code).font(.body.monospaced())
                                statusTag(invite.status)
                            }
                            Text(Self.dateLine(invite))
                                .font(.caption.monospaced())
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        if invite.status == "active" {
                            Button("Copy") { UIPasteboard.general.string = invite.code }
                                .buttonStyle(.bordered)
                        }
                    }
                    .padding(.vertical, 4)
                    .accessibilityIdentifier("inviteRow")
                }
            } header: {
                Text("Invites")
            }
        }
        .navigationTitle("Invites")
        .task { model.refreshInvites() }
        .refreshable { model.refreshInvites() }
    }

    private static func dateLine(_ invite: MobileInvite) -> String {
        if invite.status == "used", let usedAt = invite.usedAt {
            return "used " + formatter.string(from: Date(timeIntervalSince1970: TimeInterval(usedAt)))
        }
        return "expires " + formatter.string(from: Date(timeIntervalSince1970: TimeInterval(invite.expires)))
    }

    private func statusTag(_ status: String) -> some View {
        let label: String
        let color: Color
        switch status {
        case "active": label = "Active"; color = .green
        case "expired": label = "Expired"; color = .orange
        default: label = "Used"; color = .secondary
        }
        return Text(label)
            .font(.caption2)
            .foregroundStyle(color)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(color.opacity(0.15), in: Capsule())
    }
}
