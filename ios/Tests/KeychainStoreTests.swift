import XCTest

/// Validates OBS-27: the derived key round-trips through the iOS Keychain.
///
/// On an unsigned simulator run (CODE_SIGNING_ALLOWED=NO) the keychain is often
/// not writable (errSecMissingEntitlement); these tests skip in that case and
/// exercise the round-trip on a signed device/CI where the keychain is available.
final class KeychainStoreTests: XCTestCase {

    private func skipIfKeychainUnavailable() throws {
        let probe = "obsink-probe-\(UUID().uuidString)"
        let ok = KeychainStore.save(Data(repeating: 0, count: 1), account: probe)
        KeychainStore.delete(account: probe)
        try XCTSkipUnless(ok, "Keychain unavailable in this (unsigned simulator) environment")
    }

    func testSaveLoadDeleteRoundTrip() throws {
        try skipIfKeychainUnavailable()
        let account = "obsink-test-\(UUID().uuidString)"
        let key = Data(repeating: 0xAB, count: 32)

        XCTAssertNil(KeychainStore.load(account: account))
        XCTAssertTrue(KeychainStore.save(key, account: account))
        XCTAssertEqual(KeychainStore.load(account: account), key)
        XCTAssertTrue(KeychainStore.delete(account: account))
        XCTAssertNil(KeychainStore.load(account: account))
    }

    func testSaveReplacesExisting() throws {
        try skipIfKeychainUnavailable()
        let account = "obsink-test-\(UUID().uuidString)"
        XCTAssertTrue(KeychainStore.save(Data(repeating: 1, count: 32), account: account))
        XCTAssertTrue(KeychainStore.save(Data(repeating: 2, count: 32), account: account))
        XCTAssertEqual(KeychainStore.load(account: account), Data(repeating: 2, count: 32))
        XCTAssertTrue(KeychainStore.delete(account: account))
    }

    // OBS-107: the background refresh reads keys while the device is locked.
    func testItemsAreSavedAfterFirstUnlock() throws {
        try skipIfKeychainUnavailable()
        let account = "obsink-test-\(UUID().uuidString)"
        XCTAssertTrue(KeychainStore.save(Data(repeating: 3, count: 32), account: account))
        XCTAssertEqual(KeychainStore.accessibility(of: account), kSecAttrAccessibleAfterFirstUnlock as String)
        XCTAssertTrue(KeychainStore.resave(account: account))
        XCTAssertEqual(KeychainStore.load(account: account), Data(repeating: 3, count: 32))
        XCTAssertTrue(KeychainStore.delete(account: account))
        XCTAssertTrue(KeychainStore.resave(account: account), "a missing item is fine")
    }

    // Spec §6.3: the account key is kept with the server's `key_id`, the
    // user id per server, and one device id per server (minted once).
    func testAccountKeyUserIDAndDeviceIDRoundTrip() throws {
        try skipIfKeychainUnavailable()
        let user = "usr_\(UUID().uuidString)"
        let key = Data(repeating: 0xCD, count: 32)
        XCTAssertNil(KeychainStore.loadAccountKey(userID: user))
        XCTAssertTrue(KeychainStore.saveAccountKey(key, keyID: "key_1", userID: user))
        let stored = try XCTUnwrap(KeychainStore.loadAccountKey(userID: user))
        XCTAssertEqual(stored.key, key)
        XCTAssertEqual(stored.keyID, "key_1")
        XCTAssertTrue(KeychainStore.deleteAccountKey(userID: user))
        XCTAssertNil(KeychainStore.loadAccountKey(userID: user))

        let server = "https://keychain-test-\(UUID().uuidString).example/"
        XCTAssertNil(KeychainStore.loadUserID(serverURL: server))
        XCTAssertTrue(KeychainStore.saveUserID(user, serverURL: server))
        XCTAssertEqual(KeychainStore.loadUserID(serverURL: server), user)
        KeychainStore.delete(account: KeychainStore.userAccount(for: server))

        var mints = 0
        let first = KeychainStore.loadOrCreateDeviceID(serverURL: server) { mints += 1; return "dev_test_\(mints)" }
        let second = KeychainStore.loadOrCreateDeviceID(serverURL: server) { mints += 1; return "dev_test_\(mints)" }
        XCTAssertEqual(first, "dev_test_1")
        XCTAssertEqual(second, first, "the id is minted once")
        KeychainStore.delete(account: KeychainStore.deviceAccount(for: server))
    }

    func testHexDecoding() {
        XCTAssertEqual(Data(hexString: "00ff10"), Data([0x00, 0xff, 0x10]))
        XCTAssertNil(Data(hexString: "abc"))
        XCTAssertNil(Data(hexString: "zz"))
        XCTAssertEqual(Data(hexString: ""), Data())
    }
}
