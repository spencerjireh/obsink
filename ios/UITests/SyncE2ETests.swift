import XCTest

/// Simulator E2E driver for the Mac↔iOS verification checklist (OBS-29–34,
/// wire format v3).
///
/// These tests only drive the app UI; the surrounding harness
/// (`scripts/verify-ios-sim-e2e.sh`) plays "device A" with the CLI against a
/// running server, stages files, and verifies on-disk state through
/// `simctl get_app_container`. Each test is one script-orchestrated phase, so
/// they are run individually with `-only-testing`, not as a suite.
///
/// Configuration arrives via `TEST_RUNNER_`-prefixed environment variables:
///   OBSINK_TEST_SERVER_URL                 — the server; also reaches the app as
///     OBSINK_UITEST_SERVER_URL, overriding the one baked into the build
///   OBSINK_TEST_EMAIL / OBSINK_TEST_PASSPHRASE — the harness account (sign-in phase)
///   OBSINK_TEST_BEARER / OBSINK_TEST_USER_ID   — device A's session, seeded for
///     the phases that skip the sign-in UI when OBSINK_TEST_SEED_ACCOUNT=1 (the
///     sign-in phase failed); otherwise the phone keeps its own session
///   OBSINK_TEST_ACCOUNT_KEY / OBSINK_TEST_ACCOUNT_KEY_ID — the unlocked account key
///     (hex) with its server key id, seeded the same way
///   OBSINK_TEST_VAULT_ID / OBSINK_TEST_VAULT_NAME / OBSINK_TEST_VAULT_KEY — the
///     target vault and its key (hex), seeded so the phases skip the download
///   OBSINK_TEST_CHOICE                     — conflict winner (resolve test)
///   OBSINK_TEST_EXPECT_FILE                — filename (Files-app test)
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

    private func serverEnv(_ app: XCUIApplication) {
        app.launchEnvironment["OBSINK_UITEST_SERVER_URL"] = env["OBSINK_TEST_SERVER_URL"]!
        app.launchEnvironment["OBSINK_UITEST_DEVICE_ID"] = "e2e-sim"
    }

    /// The harness account's session and unlocked account key, seeded into
    /// the Keychain the way a sign-in plus unlock would (the CLI "device A"
    /// is another device of the same account).
    private func seedAccount(_ app: XCUIApplication) {
        serverEnv(app)
        app.launchEnvironment["OBSINK_UITEST_BEARER"] = env["OBSINK_TEST_BEARER"]!
        app.launchEnvironment["OBSINK_UITEST_BEARER_URL"] = env["OBSINK_TEST_SERVER_URL"]!
        app.launchEnvironment["OBSINK_UITEST_USER_ID"] = env["OBSINK_TEST_USER_ID"]!
        app.launchEnvironment["OBSINK_UITEST_ACCOUNT_KEY"] = env["OBSINK_TEST_ACCOUNT_KEY"]!
        app.launchEnvironment["OBSINK_UITEST_ACCOUNT_KEY_ID"] = env["OBSINK_TEST_ACCOUNT_KEY_ID"]!
    }

    /// Launch the app with the account and the vault (entry + key) seeded so
    /// tests skip the sign-in and download UI (the dedicated phases drive them).
    private func launchSeeded(reset: Bool = false) -> XCUIApplication {
        let app = XCUIApplication()
        let seed = """
        [{"vaultID":"\(env["OBSINK_TEST_VAULT_ID"]!)",\
        "name":"\(env["OBSINK_TEST_VAULT_NAME"] ?? "e2e-sim")"}]
        """
        app.launchEnvironment["OBSINK_UITEST_SEED"] = seed
        app.launchEnvironment["OBSINK_UITEST_VAULT_KEY"] = env["OBSINK_TEST_VAULT_KEY"]!
        // The phone's own session (from the sign-in phase) survives in the
        // Keychain between launches; device A's is seeded only when the
        // harness says so (the sign-in phase failed).
        if env["OBSINK_TEST_SEED_ACCOUNT"] == "1" { seedAccount(app) } else { serverEnv(app) }
        if reset { app.launchEnvironment["OBSINK_UITEST_RESET"] = "1" }
        app.launch()
        return app
    }

    /// Wait for a launch auto-sync (OBS-107) to finish: the Sync button is
    /// disabled while a cycle runs and enabled once the model is idle.
    private func settle(_ app: XCUIApplication, timeout: TimeInterval = 180) {
        let sync = app.buttons["syncButton"]
        let idle = NSPredicate { _, _ in sync.isEnabled }
        let result = XCTWaiter().wait(
            for: [XCTNSPredicateExpectation(predicate: idle, object: nil)],
            timeout: timeout
        )
        XCTAssertEqual(result, XCTWaiter.Result.completed, "the app never went idle")
    }

    /// Tap Sync Now and wait for the cycle to finish (network: generous).
    private func syncAndWait(_ app: XCUIApplication, expect prefix: String = "Synced ·") {
        let sync = app.buttons["syncButton"]
        XCTAssertTrue(sync.waitForExistence(timeout: 30))
        settle(app)
        XCTAssertTrue(sync.isEnabled, "Sync button disabled — no key?")
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

    private func type(_ app: XCUIApplication, _ field: XCUIElement, _ text: String) {
        XCTAssertTrue(field.waitForExistence(timeout: 10), "field missing")
        field.tap()
        field.typeText(text)
        if app.keyboards.buttons["Return"].exists { app.keyboards.buttons["Return"].tap() }
    }

    // MARK: Phases

    /// Spec §12.1 through the UI: sign in with the email code (the dev server
    /// returns it inline, so the field fills itself), enter the account
    /// passphrase (`Unlock`: device A set it), then `Download` the vault from
    /// its `Not on this device` card and wait for the first cycle.
    func testSignInUnlockAndDownload() throws {
        let app = XCUIApplication()
        app.launchEnvironment["OBSINK_UITEST_RESET"] = "1"
        serverEnv(app)
        app.launch()

        let signIn = app.buttons["signInButton"].firstMatch
        XCTAssertTrue(signIn.waitForExistence(timeout: 30), "Sign in button missing on the Vaults tab")
        signIn.tap()

        type(app, anyElement(app, "emailField"), env["OBSINK_TEST_EMAIL"]!)
        let send = app.buttons["sendCodeButton"]
        XCTAssertTrue(send.waitForExistence(timeout: 10))
        send.tap()
        let verify = app.buttons["verifyCodeButton"]
        XCTAssertTrue(verify.waitForExistence(timeout: 60), "the code step never appeared")
        // The dev server filled the code in; an established server shows the
        // invite field, which an existing account leaves empty.
        let enabled = NSPredicate(format: "isEnabled == true")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: enabled, object: verify)], timeout: 30),
            XCTWaiter.Result.completed, "the code was not filled in (is AUTH_DEV_RETURN_CODE set?)"
        )
        verify.tap()

        let unlockField = app.secureTextFields["unlockField"]
        XCTAssertTrue(unlockField.waitForExistence(timeout: 60), "the passphrase step never appeared")
        XCTAssertFalse(app.secureTextFields["unlockConfirmField"].exists, "device A set the passphrase; this is Unlock")
        type(app, unlockField, env["OBSINK_TEST_PASSPHRASE"]!)
        let unlock = app.buttons["unlockButton"]
        XCTAssertTrue(unlock.waitForExistence(timeout: 10))
        unlock.tap()
        let done = app.buttons["addVaultDoneButton"]
        XCTAssertTrue(done.waitForExistence(timeout: 60))
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: enabled, object: done)], timeout: 60),
            XCTWaiter.Result.completed, "Done never enabled (unlock failed?)"
        )
        done.tap()

        // The account's vault is listed as not on this device; Download it.
        let state = anyElement(app, "vaultStateText")
        let elsewhere = NSPredicate(format: "label == 'Not on this device'")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: elsewhere, object: state)], timeout: 60),
            XCTWaiter.Result.completed, "the vault card did not read Not on this device — last: \(state.label)"
        )
        let download = app.buttons["downloadVaultButton"].firstMatch
        XCTAssertTrue(download.waitForExistence(timeout: 10))
        download.tap()
        // `Downloaded <name>` is shown only until the first cycle starts, which
        // is at once; the cycle's `Synced ·` is the observable end state.
        waitForStatus(app, prefix: "Synced ·", timeout: 300)
    }

    /// Generic full sync cycle; the script stages state before and verifies after.
    func testSyncNow() throws {
        let app = launchSeeded()
        syncAndWait(app)
    }

    /// Remove from this device (OBS-100): the vault leaves this phone but
    /// stays on the account, so its card turns into `Not on this device`; the
    /// harness then checks that its cache directory and item database are
    /// gone from the app-group container.
    func testRemoveVaultFromDevice() throws {
        let app = launchSeeded()
        settle(app)
        let manage = anyElement(app, "manageVaultButton")
        XCTAssertTrue(manage.waitForExistence(timeout: 10), "Manage vault link missing")
        manage.tap()
        // Manage vault sits below Devices, Activity and History; scroll to it.
        let remove = app.buttons["removeVaultButton"]
        var scrolls = 0
        while !remove.exists && scrolls < 6 {
            app.swipeUp()
            scrolls += 1
        }
        XCTAssertTrue(remove.waitForExistence(timeout: 10), "Remove button missing")
        // SwiftUI Form buttons with a role report themselves non-hittable to
        // XCUITest on iOS 26 even when visible; a coordinate tap lands anyway.
        sleep(1)
        remove.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        // The confirmation dialog (an action sheet on iPhone) carries a
        // destructive button with the same label.
        let confirm = app.sheets.buttons["Remove from this device"].firstMatch
        XCTAssertTrue(confirm.waitForExistence(timeout: 10), "confirmation dialog never appeared")
        confirm.tap()
        let state = anyElement(app, "vaultStateText")
        let elsewhere = NSPredicate(format: "label == 'Not on this device'")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: elsewhere, object: state)], timeout: 30),
            XCTWaiter.Result.completed, "vault still on this device after removal — last: \(state.label)"
        )
        XCTAssertTrue(app.buttons["downloadVaultButton"].firstMatch.waitForExistence(timeout: 10))
    }

    /// Server ahead on open (OBS-33 → OBS-107): the launch auto-sync pulls
    /// the new file without a tap; the harness then checks it landed in the
    /// container. The stale banner still exists for the window before the
    /// sync starts and for vaults that hold conflicts.
    func testAutoSyncPullsRemote() throws {
        let app = launchSeeded()
        waitForStatus(app, prefix: "Synced ·")
        let state = anyElement(app, "vaultStateText")
        let upToDate = NSPredicate(format: "label == 'Up to date'")
        let result = XCTWaiter().wait(
            for: [XCTNSPredicateExpectation(predicate: upToDate, object: state)],
            timeout: 30
        )
        XCTAssertEqual(result, XCTWaiter.Result.completed, "card did not settle on Up to date — last: \(state.label)")
    }

    /// Conflict resolution (OBS-31): sync surfaces the conflict, choose the
    /// winner in the detail screen, apply, and complete.
    func testResolveConflict() throws {
        let choice = env["OBSINK_TEST_CHOICE"] ?? "Keep remote"
        let app = launchSeeded()

        let sync = app.buttons["syncButton"]
        XCTAssertTrue(sync.waitForExistence(timeout: 30))
        settle(app)
        sync.tap()
        waitForStatus(app, prefix: "1 conflict")

        // The card links to the Conflicts screen; the rows live there.
        let resolve = anyElement(app, "resolveConflictsLink")
        XCTAssertTrue(resolve.waitForExistence(timeout: 10), "Resolve link missing")
        resolve.tap()

        // Each row is a NavigationLink; resolve through the detail screen.
        // Tap the row's title text — the identifier on the NavigationLink
        // itself does not surface in the accessibility tree.
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

    /// Spec §15.3 on the Devices tab: device A (the CLI) is listed next to
    /// this phone, which carries the `This device` tag.
    func testDevicesTabListsBothDevices() throws {
        let app = launchSeeded()
        settle(app)
        app.tabBars.buttons["Devices"].tap()
        let rows = app.descendants(matching: .any).matching(identifier: "deviceRow")
        let two = NSPredicate { _, _ in rows.count >= 2 }
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: two, object: nil)], timeout: 60),
            XCTWaiter.Result.completed, "expected two device rows, got \(rows.count)"
        )
        XCTAssertTrue(anyElement(app, "deviceRowCurrentTag").waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS 'CLI'")).firstMatch.exists,
                      "device A (the CLI) is not listed")
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
