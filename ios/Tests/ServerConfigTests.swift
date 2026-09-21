import XCTest
@testable import ObSink

final class ServerConfigTests: XCTestCase {
    func testFallsBackToTheBuiltInServer() {
        XCTAssertEqual(ServerConfig.resolve(env: [:], infoValue: nil), ServerConfig.fallbackURL)
    }

    func testBlankBakedValueCountsAsAbsent() {
        XCTAssertEqual(ServerConfig.resolve(env: [:], infoValue: ""), ServerConfig.fallbackURL)
        XCTAssertEqual(ServerConfig.resolve(env: [:], infoValue: "  \n"), ServerConfig.fallbackURL)
    }

    func testBakedValueIsCanonicalised() {
        XCTAssertEqual(
            ServerConfig.resolve(env: [:], infoValue: "HTTPS://Notes.Example/"),
            "https://notes.example"
        )
    }

    func testTheOldPublicHostIsAnAliasOfTheCurrentOne() {
        XCTAssertEqual(
            ServerConfig.resolve(env: [:], infoValue: "https://obsink.spencerjireh.com/"),
            "https://obsink-api.spencerjireh.com"
        )
        XCTAssertEqual(
            KeychainStore.canonicalServerURL("HTTPS://OBSINK.spencerjireh.com/vaults"),
            "https://obsink-api.spencerjireh.com/vaults"
        )
        XCTAssertEqual(
            KeychainStore.canonicalServerURL("http://obsink.spencerjireh.com"),
            "http://obsink.spencerjireh.com"
        )
        XCTAssertEqual(
            KeychainStore.legacyServerURLs(of: "https://obsink-api.spencerjireh.com"),
            ["https://obsink.spencerjireh.com"]
        )
    }

    func testLaunchOverrideWinsOverTheBakedValue() {
        let env = [ServerConfig.overrideEnv: "http://localhost:8080/"]
        XCTAssertEqual(
            ServerConfig.resolve(env: env, infoValue: "https://notes.example"),
            "http://localhost:8080"
        )
    }

    func testBlankOverrideFallsThrough() {
        let env = [ServerConfig.overrideEnv: ""]
        XCTAssertEqual(
            ServerConfig.resolve(env: env, infoValue: "https://notes.example"),
            "https://notes.example"
        )
    }
}
