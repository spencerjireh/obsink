import XCTest
@testable import ObSink

/// Exercises the Rust core through the UniFFI bindings, running inside the iOS
/// simulator. Proves the FFI bridge, the v3 key wrapping, and (when live env
/// is provided) the full encrypted sync over the network all work on iOS.
final class ObSinkMobileTests: XCTestCase {
    /// Minimal `ProgressListener` that discards events for tests.
    final class NoopListener: ProgressListener {
        func onProgress(event: MobileProgressEvent) {}
    }

    func testThisBuildSpeaksWireFormatV3() {
        XCTAssertEqual(mobileProtocolVersion(), 3)
        XCTAssertTrue(newDeviceId().hasPrefix("dev_"))
        XCTAssertEqual(newDeviceId().count, 36)
    }

    // Spec §6.1: a vault key wraps under the account key with the vault id
    // as the AAD, so another vault's id (or another account) does not open it.
    func testVaultKeysWrapUnderTheAccountKey() throws {
        let account = newVaultKey()
        let vault = newVaultKey()
        XCTAssertEqual(account.count, 32)
        XCTAssertNotEqual(account, vault)
        let wrapped = try wrapVaultKeyFor(accountKey: account, vaultKey: vault, vaultId: "vault_a")
        XCTAssertEqual(try unwrapVaultKey(accountKey: account, wrapped: wrapped, vaultId: "vault_a"), vault)
        XCTAssertThrowsError(try unwrapVaultKey(accountKey: account, wrapped: wrapped, vaultId: "vault_b"))
        XCTAssertThrowsError(try unwrapVaultKey(accountKey: newVaultKey(), wrapped: wrapped, vaultId: "vault_a"))
        XCTAssertThrowsError(try wrapVaultKeyFor(accountKey: Data([1, 2, 3]), vaultKey: vault, vaultId: "vault_a"))
    }

    func testLiveSyncDownloadsSeededFile() throws {
        let env = ProcessInfo.processInfo.environment
        guard let url = env["OBSINK_TEST_SERVER_URL"],
              let bearer = env["OBSINK_TEST_BEARER"],
              let vaultID = env["OBSINK_TEST_VAULT_ID"],
              let keyHex = env["OBSINK_TEST_VAULT_KEY"],
              let key = Data(hexString: keyHex)
        else {
            throw XCTSkip("live server env not set")
        }

        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }

        let config = MobileVaultConfig(serverUrl: url, bearer: bearer, vaultId: vaultID, localPath: dir.path, deviceId: nil)
        let client = try VaultClient(config: config, key: key)

        let outcome = try client.sync(listener: NoopListener())
        XCTAssertTrue(outcome.completed, "sync should complete without conflicts")
        XCTAssertGreaterThanOrEqual(outcome.downloaded, 1)
        XCTAssertTrue(outcome.failures.isEmpty, "seeded-file sync should have no failures")

        let downloaded = try String(contentsOf: dir.appendingPathComponent("ios-test.md"), encoding: .utf8)
        XCTAssertTrue(downloaded.contains("hello from CLI"), "decrypted content should match the seeded file")
    }
}
