import Foundation

/// What the UI says for each `MobileError` variant. The variant decides the
/// wording and the follow-up (sign in again, retry, fix the invite code); the
/// server's text is shown only for `Server` errors, sentence-cased.
extension MobileError {
    /// Shared label (DESIGN.md §5).
    static let sessionExpiredMessage = "Session expired. Sign in again."

    var displayMessage: String {
        switch self {
        case .Unauthorized:
            return Self.sessionExpiredMessage
        case .Network:
            return "Could not reach the server. Check the URL and your connection."
        case .Server(_, let message):
            return Self.sentence(message)
        case .Sync(let message):
            return Self.sentence(message)
        case .InvalidKey:
            return "The stored key is damaged. Remove the vault and connect again."
        case .NoPendingSync:
            return "Nothing to resolve. Sync first."
        }
    }

    var isUnauthorized: Bool {
        if case .Unauthorized = self { return true }
        return false
    }

    /// The server has no machine-readable code for these two 403s; both of
    /// its invite messages contain the words.
    var isInviteRequired: Bool {
        if case .Server(403, let message) = self { return message.contains("invite code") }
        return false
    }

    /// Sign in with Apple gave an email hint without a code (`POST /auth/apple`).
    var needsEmailVerification: Bool {
        if case .Server(403, let message) = self { return message.contains("email verification required") }
        return false
    }

    private static func sentence(_ text: String) -> String {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let first = trimmed.first else { return trimmed }
        let capitalised = first.uppercased() + trimmed.dropFirst()
        return capitalised.hasSuffix(".") ? capitalised : capitalised + "."
    }
}

extension Error {
    /// `displayMessage` for a `MobileError`; the localized description for
    /// anything else (Foundation errors from file operations).
    var obsinkMessage: String {
        (self as? MobileError)?.displayMessage ?? localizedDescription
    }

    var isUnauthorized: Bool {
        (self as? MobileError)?.isUnauthorized ?? false
    }
}
