import XCTest

/// Simulator E2E driver for the Mac↔iOS verification checklist (OBS-29–34).
///
/// These tests only drive the app UI; the surrounding harness
/// (`scripts/verify-ios-sim-e2e.sh`) plays "device A" with the CLI against a
/// running server, stages files, and verifies on-disk state through
/// `simctl get_app_container`. Each test is one script-orchestrated phase, so
/// they are run individually with `-only-testing`, not as a suite.
///
/// Configuration arrives via `TEST_RUNNER_`-prefixed environment variables:
///   OBSINK_TEST_SERVER_URL / OBSINK_TEST_API_KEY  — server connection (operator bearer)
///   OBSINK_TEST_VAULT_ID / OBSINK_TEST_VAULT_NAME — target vault
///   OBSINK_TEST_PASSPHRASE                        — vault passphrase
///   OBSINK_TEST_CHOICE                            — conflict winner (resolve test)
///   OBSINK_TEST_EXPECT_FILE                       — filename (Files-app test)
final class SyncE2ETests: XCTestCase {
    private var env: [String: String] { ProcessInfo.processInfo.environment }

    override func setUp() {
        continueAfterFailure = false
    }

    /// Identifier lookup across all element types — SwiftUI Form controls
    /// surface as different XCUIElement types per style and OS version.
    private func anyElement(_ app: XCUIApplication, _ id: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: id).firstMatch
    }

    /// The harness's operator bearer, seeded into the Keychain the way a
    /// sign-in would (the CLI "device A" uses the same principal, so both
    /// devices see the same vault list).
    private func seedBearer(_ app: XCUIApplication) {
        app.launchEnvironment["OBSINK_UITEST_BEARER"] = env["OBSINK_TEST_API_KEY"]!
        app.launchEnvironment["OBSINK_UITEST_BEARER_URL"] = env["OBSINK_TEST_SERVER_URL"]!
    }

    /// Launch the app with the vault seeded into the app-group defaults so
    /// tests skip the Add Vault UI (the dedicated connect test drives it).
    private func launchSeeded(reset: Bool = false) -> XCUIApplication {
        let app = XCUIApplication()
        let seed = """
        [{"serverURL":"\(env["OBSINK_TEST_SERVER_URL"]!)",\
        "vaultID":"\(env["OBSINK_TEST_VAULT_ID"]!)",\
        "name":"\(env["OBSINK_TEST_VAULT_NAME"] ?? "e2e-sim")"}]
        """
        app.launchEnvironment["OBSINK_UITEST_SEED"] = seed
        seedBearer(app)
        if reset { app.launchEnvironment["OBSINK_UITEST_RESET"] = "1" }
        app.launch()
        return app
    }

    /// Type the passphrase into the root form if the vault has no stored key yet.
    private func enterPassphraseIfNeeded(_ app: XCUIApplication) {
        let sync = app.buttons["syncButton"]
        guard !sync.isEnabled else { return }
        let field = app.secureTextFields["passphraseField"]
        XCTAssertTrue(field.waitForExistence(timeout: 10), "passphrase field missing")
        field.tap()
        field.typeText(env["OBSINK_TEST_PASSPHRASE"]!)
        // Dismiss the keyboard so the Sync button is hittable.
        if app.keyboards.buttons["Return"].exists { app.keyboards.buttons["Return"].tap() }
    }

    /// Tap Sync Now and wait for the cycle to finish (Argon2 + network: generous).
    private func syncAndWait(_ app: XCUIApplication, expect prefix: String = "Synced ·") {
        let sync = app.buttons["syncButton"]
        XCTAssertTrue(sync.waitForExistence(timeout: 10))
        XCTAssertTrue(sync.isEnabled, "Sync button disabled — no key/passphrase?")
        sync.tap()
        waitForStatus(app, prefix: prefix)
    }

    private func waitForStatus(_ app: XCUIApplication, prefix: String, timeout: TimeInterval = 180) {
        let status = app.staticTexts["statusText"]
        let done = NSPredicate(format: "label BEGINSWITH %@", prefix)
        let result = XCTWaiter().wait(
            for: [XCTNSPredicateExpectation(predicate: done, object: status)],
            timeout: timeout
        )
        XCTAssertEqual(result, XCTWaiter.Result.completed, "status never reached '\(prefix)…' — last: \(status.label)")
    }

    // MARK: Phases

    /// Add Vault → Connect flow against the running server (vault setup UI).
    /// The bearer is pre-seeded, so typing the server URL should show the
    /// "signed in" row instead of the sign-in controls.
    func testConnectVaultFlow() throws {
        let app = XCUIApplication()
        app.launchEnvironment["OBSINK_UITEST_RESET"] = "1"
        seedBearer(app)
        app.launch()

        app.buttons["addVaultButton"].tap()

        let url = app.textFields["addVaultServerURL"]
        XCTAssertTrue(url.waitForExistence(timeout: 10))
        url.tap()
        // Clear the prefill, then type the full URL.
        url.press(forDuration: 1.2)
        if app.menuItems["Select All"].waitForExistence(timeout: 3) { app.menuItems["Select All"].tap() }
        url.typeText(env["OBSINK_TEST_SERVER_URL"]!)
        if app.keyboards.buttons["Return"].exists { app.keyboards.buttons["Return"].tap() }

        XCTAssertTrue(anyElement(app, "addVaultAccountText").waitForExistence(timeout: 10),
                      "seeded bearer not recognised for the typed server URL")

        app.buttons["Connect"].tap()
        app.buttons["listVaultsButton"].tap()

        let picker = anyElement(app, "vaultPicker")
        if !picker.waitForExistence(timeout: 60) {
            let err = anyElement(app, "addVaultStatusText")
            XCTFail("vault list never loaded" + (err.exists ? " — status: \(err.label)" : ""))
        }
        picker.tap()
        let vaultName = env["OBSINK_TEST_VAULT_NAME"] ?? "e2e-sim"
        // The menu picker pushes/pops a selection list; the option is a button
        // in most layouts, a static text in others.
        // Long vault lists push the newest entry off-screen (absent from the
        // accessibility tree); scroll until it shows up.
        var option = app.buttons[vaultName].firstMatch
        for _ in 0..<12 {
            if app.buttons[vaultName].firstMatch.waitForExistence(timeout: 2) {
                option = app.buttons[vaultName].firstMatch; break
            }
            if app.staticTexts[vaultName].firstMatch.exists {
                option = app.staticTexts[vaultName].firstMatch; break
            }
            app.swipeUp()
        }
        XCTAssertTrue(option.exists, "vault '\(vaultName)' not listed")
        option.tap()

        let pass = app.secureTextFields["addVaultPassphraseField"]
        pass.tap()
        pass.typeText(env["OBSINK_TEST_PASSPHRASE"]!)

        let submit = app.buttons["addVaultSubmitButton"]
        XCTAssertTrue(submit.isEnabled)
        submit.tap()

        // Sheet dismisses; the vault becomes active. Argon2 derive is slow.
        waitForStatus(app, prefix: "Added vault", timeout: 120)
    }

    /// Generic full sync cycle; the script stages state before and verifies after.
    func testSyncNow() throws {
        let app = launchSeeded()
        enterPassphraseIfNeeded(app)
        syncAndWait(app)
    }

    /// Stale-vault warning on open (OBS-33): server is ahead, banner appears
    /// without syncing.
    func testStaleBanner() throws {
        let app = launchSeeded()
        let banner = anyElement(app, "staleBanner")
        XCTAssertTrue(banner.waitForExistence(timeout: 120), "stale banner never appeared")
        // The Label's icon and text are separate children; check the text child.
        let text = app.staticTexts.matching(
            NSPredicate(format: "label CONTAINS 'changed on another device'")
        ).firstMatch
        XCTAssertTrue(text.waitForExistence(timeout: 10), "banner text missing")
    }

    /// Conflict resolution (OBS-31): sync surfaces the conflict, choose the
    /// winner in the detail screen, apply, and complete.
    func testResolveConflict() throws {
        let choice = env["OBSINK_TEST_CHOICE"] ?? "Keep remote"
        let app = launchSeeded()
        enterPassphraseIfNeeded(app)

        let sync = app.buttons["syncButton"]
        XCTAssertTrue(sync.waitForExistence(timeout: 10))
        sync.tap()
        waitForStatus(app, prefix: "1 conflict")

        // The inline row is a NavigationLink; resolve through the detail screen.
        // Tap the row's title text — the identifier on the NavigationLink
        // itself does not surface in the accessibility tree.
        // Off-screen Form rows are not in the accessibility tree — scroll
        // the Conflicts section into view first.
        let row = anyElement(app, "conflictRowTitle")
        var scrolls = 0
        while !row.exists && scrolls < 4 {
            app.swipeUp()
            scrolls += 1
        }
        if !row.waitForExistence(timeout: 15) {
            print("=== TREE: conflicts state ===")
            print(app.debugDescription)
            XCTFail("conflict row not shown")
        }
        row.tap()
        let winner = anyElement(app, "winnerPicker")
        XCTAssertTrue(winner.waitForExistence(timeout: 10))
        winner.buttons[choice].tap()
        app.navigationBars.buttons.firstMatch.tap() // back

        let apply = app.buttons["applyResolutionsButton"]
        var applyScrolls = 0
        while !apply.exists && applyScrolls < 4 {
            app.swipeUp()
            applyScrolls += 1
        }
        XCTAssertTrue(apply.waitForExistence(timeout: 10), "apply button not shown")
        apply.tap()
        waitForStatus(app, prefix: "Synced ·")
    }

    /// Files-app half of OBS-19/29: the ObSink File Provider location exists
    /// and shows a synced file.
    func testFilesAppShowsVault() throws {
        let expected = env["OBSINK_TEST_EXPECT_FILE"] ?? "hello.md"

        let files = XCUIApplication(bundleIdentifier: "com.apple.DocumentsApp")
        files.launch()

        // Browse tab → sidebar list. Each vault is its own File Provider
        // domain, listed under the vault's name (the provider itself is "ObSink").
        let browse = files.buttons["Browse"].firstMatch
        if browse.waitForExistence(timeout: 10) { browse.tap(); browse.tap() }

        let vaultName = env["OBSINK_TEST_VAULT_NAME"] ?? "ObSink"
        var location = files.staticTexts[vaultName].firstMatch
        if !location.waitForExistence(timeout: 30) {
            location = files.staticTexts["ObSink"].firstMatch
            if !location.waitForExistence(timeout: 10) {
                add(XCTAttachment(screenshot: files.screenshot()))
                XCTFail("\(vaultName) location not in Files sidebar")
            }
        }
        location.tap()

        // The Files app hides known extensions, so match on the stem too.
        let stem = (expected as NSString).deletingPathExtension
        let file = files.staticTexts.matching(
            NSPredicate(format: "label == %@ OR label BEGINSWITH %@", expected, stem)
        ).firstMatch
        if !file.waitForExistence(timeout: 45) {
            files.swipeDown() // pull-to-refresh, then retry once
            if !file.waitForExistence(timeout: 45) {
                print("=== TREE: files listing ===")
                print(files.debugDescription)
                XCTFail("'\(expected)' not visible in Files app")
            }
        }

        let shot = XCTAttachment(screenshot: files.screenshot())
        shot.lifetime = .keepAlways
        add(shot)
    }
}

