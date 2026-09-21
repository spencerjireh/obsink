import Foundation
import Security

/// Stores the derived vault encryption key in the iOS Keychain so the user
/// doesn't re-enter the passphrase every launch (spec §6.3). Keys are scoped per
/// vault (account = vault ID), service `obsink`. The File Provider extension does
/// not need the key (it serves already-decrypted cache), so this stays app-side.
enum KeychainStore {
    private static let service = "obsink"

    /// Items are readable once the device has been unlocked since boot, so
    /// the background refresh can sync while it is locked again. Still
    /// encrypted at rest until that first unlock.
    static let accessibility = kSecAttrAccessibleAfterFirstUnlock

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
        add[kSecAttrAccessible as String] = accessibility
        return SecItemAdd(add as CFDictionary, nil) == errSecSuccess
    }

    /// Re-save an existing item under the current accessibility class; a
    /// missing item is fine.
    @discardableResult
    static func resave(account: String) -> Bool {
        guard let data = load(account: account) else { return true }
        return save(data, account: account)
    }

    /// The accessibility class an item was saved with (tests).
    static func accessibility(of account: String) -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnAttributes as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
              let attributes = item as? [String: Any] else { return nil }
        return attributes[kSecAttrAccessible as String] as? String
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

    /// Hosts the server moved away from, mapped to where it lives now (core
    /// `LEGACY_SERVER_ALIASES`): an entry written by an older build against
    /// the old host still matches the server this build talks to.
    static let legacyServerAliases: [String: String] = [
        "https://obsink.spencerjireh.com": "https://obsink-api.spencerjireh.com"
    ]

    static func canonicalServerURL(_ url: String) -> String {
        var trimmed = url.trimmingCharacters(in: .whitespacesAndNewlines)
        while trimmed.hasSuffix("/") { trimmed.removeLast() }
        guard let range = trimmed.range(of: "://") else { return trimmed }
        let scheme = trimmed[..<range.lowerBound].lowercased()
        let rest = trimmed[range.upperBound...]
        let host: String
        let path: Substring
        if let slash = rest.firstIndex(of: "/") {
            host = rest[..<slash].lowercased()
            path = rest[slash...]
        } else {
            host = rest.lowercased()
            path = ""
        }
        let origin = legacyServerAliases[scheme + "://" + host] ?? (scheme + "://" + host)
        return origin + path
    }

    /// The legacy spellings that canonicalise to `canonical`: where an older
    /// build may have stored the bearer for this server.
    static func legacyServerURLs(of canonical: String) -> [String] {
        legacyServerAliases.filter { $0.value == canonical }.map(\.key)
    }

    @discardableResult
    static func saveBearer(_ token: String, serverURL: String) -> Bool {
        save(Data(token.utf8), account: bearerAccount(for: serverURL))
    }

    /// The bearer for a server. A bearer an older build stored under a legacy
    /// host is moved to the canonical entry the first time it is read.
    static func loadBearer(serverURL: String) -> String? {
        let canonical = canonicalServerURL(serverURL)
        if let token = load(account: "bearer:" + canonical).flatMap({ String(data: $0, encoding: .utf8) }) {
            return token
        }
        for legacy in legacyServerURLs(of: canonical) {
            let account = "bearer:" + legacy
            guard let token = load(account: account).flatMap({ String(data: $0, encoding: .utf8) }) else { continue }
            saveBearer(token, serverURL: canonical)
            delete(account: account)
            return token
        }
        return nil
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
