import SwiftUI

/// Spec §12.3 from the Devices tab: the fingerprint shown on the waiting
/// device, typed here. `Approve` wraps the account key to that device only
/// when the typed value matches (nothing is sent otherwise) and closes the
/// sheet; a mismatch or an expired request keeps it open with its message
/// (DESIGN.md §5 approval prompt). The same shape as `TypedConfirmationSheet`.
struct ApproveDeviceSheet: View {
    @ObservedObject var model: SyncModel
    let device: MobileDevice
    @Environment(\.dismiss) private var dismiss
    @State private var typed = ""
    @State private var status = ""
    @State private var busy = false
    @FocusState private var focused: Bool

    /// The 8 symbols the other device shows.
    static let fingerprintLength = 8

    private var ready: Bool {
        !busy && SyncModel.normalizeFingerprint(typed).count == Self.fingerprintLength
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text("Type the \(Self.fingerprintLength) characters shown on \(device.name).")
                    TextField("Fingerprint", text: $typed)
                        .font(.body.monospaced())
                        .textInputAutocapitalization(.characters)
                        .autocorrectionDisabled()
                        .focused($focused)
                        .submitLabel(.done)
                        .accessibilityIdentifier("approveFingerprintField")
                    if !status.isEmpty {
                        Text(status).font(.caption).foregroundStyle(.red)
                            .accessibilityIdentifier("approveStatusText")
                    }
                }
                Section {
                    Button { submit() } label: {
                        Text(busy ? "Working…" : "Approve")
                            .fontWeight(.semibold)
                            .frame(maxWidth: .infinity, minHeight: 44)
                    }
                    .disabled(!ready)
                    .accessibilityIdentifier("approveSubmitButton")
                }
            }
            .navigationTitle("Approve \(device.name)")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                Button("Cancel") { dismiss() }
                    .disabled(busy)
                    .accessibilityIdentifier("approveCancelButton")
            }
            .onAppear { focused = true }
        }
        .presentationDetents([.medium])
    }

    private func submit() {
        guard ready else { return }
        busy = true
        status = ""
        let entered = typed
        Task { @MainActor in
            do {
                try await model.approveDevice(device, fingerprint: entered)
                busy = false
                dismiss()
            } catch {
                status = error.obsinkMessage
                busy = false
            }
        }
    }
}
