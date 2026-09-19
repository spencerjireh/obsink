import XCTest
@testable import ObSink

/// The UI decides what to say from the `MobileError` variant (OBS-100).
final class MobileErrorPresentationTests: XCTestCase {
    func testUnauthorizedIsTheSessionMessage() {
        let error = MobileError.Unauthorized(message: "unauthorized: sign in again")
        XCTAssertEqual(error.displayMessage, "Session expired. Sign in again.")
        XCTAssertTrue(error.isUnauthorized)
        XCTAssertTrue((error as Error).isUnauthorized)
    }

    func testNetworkHasFixedCopy() {
        let error = MobileError.Network(message: "error sending request")
        XCTAssertEqual(error.displayMessage, "Could not reach the server. Check the URL and your connection.")
        XCTAssertFalse(error.isUnauthorized)
    }

    func testServerMessagesAreSentenceCased() {
        let error = MobileError.Server(status: 507, message: "vault storage limit reached")
        XCTAssertEqual(error.displayMessage, "Vault storage limit reached.")
        XCTAssertFalse(error.isInviteRequired)
    }

    func testThe403sAreRecognised() {
        XCTAssertTrue(MobileError.Server(status: 403, message: "an invite code is required to create an account").isInviteRequired)
        XCTAssertTrue(MobileError.Server(status: 403, message: "invite code is invalid, used, or expired").isInviteRequired)
        XCTAssertFalse(MobileError.Server(status: 401, message: "invite code is invalid").isInviteRequired)
        XCTAssertTrue(MobileError.Server(status: 403, message: "email verification required: request a code").needsEmailVerification)
        XCTAssertFalse(MobileError.Server(status: 403, message: "email verification required").isInviteRequired)
    }

    func testFoundationErrorsKeepTheirDescription() {
        let error = NSError(domain: "obsink", code: 1, userInfo: [NSLocalizedDescriptionKey: "Enter a passphrase."])
        XCTAssertEqual(error.obsinkMessage, "Enter a passphrase.")
        XCTAssertFalse(error.isUnauthorized)
    }
}

final class ByteFormattingTests: XCTestCase {
    func testBinaryUnitsMatchTheDesktop() {
        XCTAssertEqual(SyncModel.formatBytes(0), "0 B")
        XCTAssertEqual(SyncModel.formatBytes(36), "36 B")
        XCTAssertEqual(SyncModel.formatBytes(1536), "1.5 KiB")
        XCTAssertEqual(SyncModel.formatBytes(412 * 1024 * 1024), "412 MiB")
        XCTAssertEqual(SyncModel.formatBytes(1024 * 1024 * 1024), "1.0 GiB")
    }
}
