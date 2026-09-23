import XCTest
@testable import ObSink

/// OBS-107: the pure rules behind the foreground and background auto-sync.
final class AutoSyncPolicyTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_800_000_000)

    private func ready() -> VaultState {
        var state = VaultState()
        state.hasStoredKey = true
        state.lastSyncedAt = now.addingTimeInterval(-60)
        return state
    }

    func testARecentlySyncedIdleVaultIsLeftAlone() {
        XCTAssertFalse(AutoSyncPolicy.shouldSync(ready(), now: now))
    }

    func testPendingLocalWritesTriggerASync() {
        var state = ready()
        state.pendingLocal = 2
        XCTAssertTrue(AutoSyncPolicy.shouldSync(state, now: now))
    }

    func testAServerAheadTriggersASync() {
        var state = ready()
        state.staleDownloads = 1
        XCTAssertTrue(AutoSyncPolicy.shouldSync(state, now: now))
    }

    func testANeverSyncedVaultSyncs() {
        var state = ready()
        state.lastSyncedAt = nil
        XCTAssertTrue(AutoSyncPolicy.shouldSync(state, now: now))
    }

    func testStalenessBoundaryIsFifteenMinutes() {
        var state = ready()
        state.lastSyncedAt = now.addingTimeInterval(-AutoSyncPolicy.staleAfter)
        XCTAssertFalse(AutoSyncPolicy.shouldSync(state, now: now), "exactly 15 min is not stale")
        state.lastSyncedAt = now.addingTimeInterval(-AutoSyncPolicy.staleAfter - 1)
        XCTAssertTrue(AutoSyncPolicy.shouldSync(state, now: now))
    }

    func testVaultsThatNeedTheUserOrCannotSyncAreSkipped() {
        var syncing = ready()
        syncing.pendingLocal = 1
        syncing.phase = .syncing
        XCTAssertFalse(AutoSyncPolicy.shouldSync(syncing, now: now))

        var errored = ready()
        errored.pendingLocal = 1
        errored.phase = .error("boom")
        XCTAssertFalse(AutoSyncPolicy.shouldSync(errored, now: now), "an errored vault waits for the user")

        var deleted = ready()
        deleted.pendingLocal = 1
        deleted.deletedOnServer = true
        XCTAssertFalse(AutoSyncPolicy.shouldSync(deleted, now: now))

        var keyless = ready()
        keyless.pendingLocal = 1
        keyless.hasStoredKey = false
        XCTAssertFalse(AutoSyncPolicy.shouldSync(keyless, now: now))

        var conflicted = ready()
        conflicted.pendingLocal = 1
        conflicted.conflicts = 1
        XCTAssertFalse(AutoSyncPolicy.shouldSync(conflicted, now: now))
    }
}
