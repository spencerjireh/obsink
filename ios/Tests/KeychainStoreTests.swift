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
}
