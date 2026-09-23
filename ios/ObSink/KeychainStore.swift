import Foundation
import Security

/// The iOS Keychain as the app's secret store (spec §6.3), service `obsink`:
/// each vault's key under `account = vaultID`, the server bearer under
/// `bearer:<server url>`, the signed-in user id under `user:<server url>`,
/// the account key (with the server's `key_id`) under `account:<user id>`,
/// and this phone's device id under `device:<server url>`. The File Provider
/// extension serves already-decrypted files and needs none of it.
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

    private static func loadString(account: String) -> String? {
        load(account: account).flatMap { String(data: $0, encoding: .utf8) }
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
        if let token = loadString(account: "bearer:" + canonical) {
            return token
        }
        for legacy in legacyServerURLs(of: canonical) {
            let account = "bearer:" + legacy
            guard let token = loadString(account: account) else { continue }
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

    // MARK: Signed-in user, account key, device id (spec §6.3)

    static func userAccount(for serverURL: String) -> String {
        "user:" + canonicalServerURL(serverURL)
    }

    @discardableResult
    static func saveUserID(_ userID: String, serverURL: String) -> Bool {
        save(Data(userID.utf8), account: userAccount(for: serverURL))
    }

    static func loadUserID(serverURL: String) -> String? {
        loadString(account: userAccount(for: serverURL))
    }

    static func accountKeyAccount(for userID: String) -> String {
        "account:" + userID
    }

    /// The unlocked account key with the server's `key_id`, so a later
    /// `GET /auth/keys` can tell whether this entry is still the account's.
    @discardableResult
    static func saveAccountKey(_ key: Data, keyID: String, userID: String) -> Bool {
        let value = key.map { String(format: "%02x", $0) }.joined() + ":" + keyID
        return save(Data(value.utf8), account: accountKeyAccount(for: userID))
    }

    static func loadAccountKey(userID: String) -> (key: Data, keyID: String)? {
        guard let value = loadString(account: accountKeyAccount(for: userID)),
              let colon = value.firstIndex(of: ":") else { return nil }
        let hex = value[..<colon]
        let keyID = String(value[value.index(after: colon)...])
        guard let key = Data(hexString: String(hex)), key.count == 32, !keyID.isEmpty else { return nil }
        return (key, keyID)
    }

    @discardableResult
    static func deleteAccountKey(userID: String) -> Bool {
        delete(account: accountKeyAccount(for: userID))
    }

    static func deviceAccount(for serverURL: String) -> String {
        "device:" + canonicalServerURL(serverURL)
    }

    /// This phone's device id for a server (spec §4.1): read from the
    /// Keychain, minted on first use. `OBSINK_UITEST_DEVICE_ID` fixes it for
    /// the simulator harness.
    static func loadOrCreateDeviceID(serverURL: String, mint: () -> String) -> String {
        if let fixed = ProcessInfo.processInfo.environment["OBSINK_UITEST_DEVICE_ID"], !fixed.isEmpty {
            return fixed
        }
        if let stored = loadString(account: deviceAccount(for: serverURL)), !stored.isEmpty {
            return stored
        }
        let id = mint()
        save(Data(id.utf8), account: deviceAccount(for: serverURL))
        return id
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

extension Data {
    /// Bytes from an even-length hex string; nil on any other input.
    init?(hexString: String) {
        let chars = Array(hexString)
        guard chars.count % 2 == 0 else { return nil }
        var bytes: [UInt8] = []
        bytes.reserveCapacity(chars.count / 2)
        var index = 0
        while index < chars.count {
            guard let byte = UInt8(String(chars[index...index + 1]), radix: 16) else { return nil }
            bytes.append(byte)
            index += 2
        }
        self.init(bytes)
    }
}
