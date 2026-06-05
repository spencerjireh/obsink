import XCTest
@testable import ObSink

/// Exercises the Rust core through the UniFFI bindings, running inside the iOS
/// simulator. Proves the FFI bridge, Argon2 key derivation, and (when live env
/// is provided) the full encrypted sync over the network all work on iOS.
final class ObSinkMobileTests: XCTestCase {
    func testDeriveKeyIsDeterministic32Bytes() throws {
        let a = try deriveMasterKey(passphrase: "hunter2", vaultId: "vault_test")
        let b = try deriveMasterKey(passphrase: "hunter2", vaultId: "vault_test")
        XCTAssertEqual(a.count, 32)
        XCTAssertEqual(a, b)
        XCTAssertNotEqual(a, try deriveMasterKey(passphrase: "other", vaultId: "vault_test"))
    }

    func testLiveSyncDownloadsSeededFile() throws {
        let env = ProcessInfo.processInfo.environment
        guard let url = env["OBSINK_TEST_WORKER_URL"],
              let apiKey = env["OBSINK_TEST_API_KEY"],
              let vaultID = env["OBSINK_TEST_VAULT_ID"],
              let passphrase = env["OBSINK_TEST_PASSPHRASE"]
        else {
            throw XCTSkip("live worker env not set")
        }

        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }

        let config = MobileVaultConfig(workerUrl: url, apiKey: apiKey, vaultId: vaultID, localPath: dir.path)
        let key = try deriveMasterKey(passphrase: passphrase, vaultId: vaultID)
        let client = try VaultClient(config: config, key: key)

        let outcome = try client.sync()
        XCTAssertTrue(outcome.completed, "sync should complete without conflicts")
        XCTAssertGreaterThanOrEqual(outcome.downloaded, 1)

        let downloaded = try String(contentsOf: dir.appendingPathComponent("ios-test.md"), encoding: .utf8)
        XCTAssertTrue(downloaded.contains("hello from CLI"), "decrypted content should match the seeded file")
    }
}
