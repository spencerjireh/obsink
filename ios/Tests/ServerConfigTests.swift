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
