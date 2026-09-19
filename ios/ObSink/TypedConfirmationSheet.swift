import SwiftUI

/// The confirmation pattern from DESIGN.md §4: a sheet, never an alert. The
/// destructive button stays disabled until the typed value matches
/// `expected` (case-insensitive for emails and the word `delete`, exact for
/// vault names); `expected == nil` asks for no typed value.
struct TypedConfirmationSheet: View {
    let title: String
    let message: String
    let expected: String?
    var caseInsensitive = false
    let confirmLabel: String
    let onConfirm: () -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var typed = ""
    @FocusState private var focused: Bool

    private var matches: Bool {
        guard let expected else { return true }
        let value = typed.trimmingCharacters(in: .whitespacesAndNewlines)
        return caseInsensitive ? value.lowercased() == expected.lowercased() : value == expected
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text(message)
                    if let expected {
                        VStack(alignment: .leading, spacing: 4) {
                            (Text("Type ") + Text(expected).font(.body.monospaced()) + Text(" to confirm."))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            TextField(expected, text: $typed)
                                .font(.body.monospaced())
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled()
                                .focused($focused)
                                .accessibilityIdentifier("confirmationField")
                        }
                    }
                }
                Section {
                    Button(role: .destructive) {
                        onConfirm()
                        dismiss()
                    } label: {
                        Text(confirmLabel)
                            .fontWeight(.semibold)
                            .frame(maxWidth: .infinity, minHeight: 44)
                    }
                    .disabled(!matches)
                    .accessibilityIdentifier("confirmDestructiveButton")
                }
            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                Button("Cancel") { dismiss() }
                    .accessibilityIdentifier("confirmCancelButton")
            }
            .onAppear { focused = expected != nil }
        }
        .presentationDetents([.medium])
    }
}
