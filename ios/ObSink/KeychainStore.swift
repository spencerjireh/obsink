import Foundation
import Security

/// Stores the derived vault encryption key in the iOS Keychain so the user
/// doesn't re-enter the passphrase every launch (spec §6.3). Keys are scoped per
/// vault (account = vault ID), service `obsink`. The File Provider extension does
/// not need the key (it serves already-decrypted cache), so this stays app-side.
enum KeychainStore {
    private static let service = "obsink"

    @discardableResult
    static func save(_ key: Data, account: String) -> Bool {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account
        ]
        SecItemDelete(query as CFDictionary)
        var add = query
        add[kSecValueData as String] = key
        return SecItemAdd(add as CFDictionary, nil) == errSecSuccess
    }

    static func load(account: String) -> Data? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess else { return nil }
        return item as? Data
    }

    // MARK: Server bearer (session token)

    /// Keychain account for a server's bearer. Canonicalised so the same
    /// server always maps to one entry (mirrors core `normalize_server_url`).
    static func bearerAccount(for serverURL: String) -> String {
        "bearer:" + canonicalServerURL(serverURL)
    }

    static func canonicalServerURL(_ url: String) -> String {
        var trimmed = url.trimmingCharacters(in: .whitespacesAndNewlines)
        while trimmed.hasSuffix("/") { trimmed.removeLast() }
        guard let range = trimmed.range(of: "://") else { return trimmed }
        let scheme = trimmed[..<range.lowerBound].lowercased()
        let rest = trimmed[range.upperBound...]
        if let slash = rest.firstIndex(of: "/") {
            return scheme + "://" + rest[..<slash].lowercased() + rest[slash...]
        }
        return scheme + "://" + rest.lowercased()
    }

    @discardableResult
    static func saveBearer(_ token: String, serverURL: String) -> Bool {
        save(Data(token.utf8), account: bearerAccount(for: serverURL))
    }

    static func loadBearer(serverURL: String) -> String? {
        load(account: bearerAccount(for: serverURL)).flatMap { String(data: $0, encoding: .utf8) }
    }

    @discardableResult
    static func deleteBearer(serverURL: String) -> Bool {
        delete(account: bearerAccount(for: serverURL))
    }

    @discardableResult
    static func delete(account: String) -> Bool {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account
        ]
        return SecItemDelete(query as CFDictionary) == errSecSuccess
    }
}
