import XCTest
@testable import ObSink

final class VaultStateTests: XCTestCase {
    private func state(_ configure: (inout VaultState) -> Void) -> VaultState {
        var s = VaultState()
        s.hasStoredKey = true
        configure(&s)
        return s
    }

    func testStatusLinePriority() {
        XCTAssertEqual(VaultState.statusLine(state { _ in }), "Up to date")
        XCTAssertEqual(VaultState.statusLine(state { $0.pendingLocal = 2 }), "2 to upload")
        XCTAssertEqual(VaultState.statusLine(state { $0.staleDownloads = 3 }), "3 to download")
        XCTAssertEqual(
            VaultState.statusLine(state { $0.pendingLocal = 2; $0.staleDownloads = 3 }),
            "2 to upload · 3 to download"
        )
        XCTAssertEqual(VaultState.statusLine(state { $0.conflicts = 1; $0.staleDownloads = 3 }), "1 conflict")
        XCTAssertEqual(VaultState.statusLine(state { $0.conflicts = 2 }), "2 conflicts")
        XCTAssertEqual(VaultState.statusLine(state { $0.hasStoredKey = false; $0.conflicts = 2 }), "Needs passphrase")
        XCTAssertEqual(VaultState.statusLine(state { $0.isForeign = true; $0.hasStoredKey = false }), "On another server")
        XCTAssertEqual(VaultState.statusLine(state { $0.phase = .error("boom"); $0.isForeign = true }), "Error: boom")
        XCTAssertEqual(VaultState.statusLine(state { $0.phase = .resolving; $0.phase = .syncing }), "Syncing…")
        XCTAssertEqual(VaultState.statusLine(state { $0.phase = .resolving }), "Resolving…")
    }

    func testCompletedOutcomeRecordsTheSyncAndClearsWhatItPulled() {
        var s = state { $0.staleDownloads = 4; $0.conflicts = 1; $0.phase = .syncing }
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        s.apply(outcome: SyncOutcome(uploaded: 1, downloaded: 4, conflicts: [], failures: [], completed: true), now: now)
        XCTAssertEqual(s.phase, .idle)
        XCTAssertEqual(s.lastSyncedAt, now)
        XCTAssertEqual(s.staleDownloads, 0)
        XCTAssertEqual(s.conflicts, 0)
        XCTAssertEqual(s.failures, 0)
    }

    func testConflictedOutcomeCountsConflictsWithoutASyncTime() {
        var s = state { $0.phase = .syncing }
        let conflict = MobileConflict(
            path: "a.md", localModified: 1, remoteModified: 2, localSize: 3, remoteSize: 4,
            localDeleted: false, remoteDeleted: false
        )
        s.apply(outcome: SyncOutcome(uploaded: 0, downloaded: 0, conflicts: [conflict], failures: [], completed: false), now: Date())
        XCTAssertEqual(s.conflicts, 1)
        XCTAssertNil(s.lastSyncedAt)
        XCTAssertEqual(s.phase, .idle)
    }

    func testStaleStatusFillsDownloadsAndConflicts() {
        var s = state { _ in }
        s.apply(status: MobileVaultStatus(pendingUploads: 0, pendingDownloads: 3, pendingConflicts: 1))
        XCTAssertEqual(s.staleDownloads, 3)
        XCTAssertEqual(s.conflicts, 1)
    }

    func testForeignComparesCanonicalForms() {
        XCTAssertFalse(VaultState.isForeign(entryURL: "https://X.example/", defaultURL: "https://x.example"))
        XCTAssertTrue(VaultState.isForeign(entryURL: "https://y.example", defaultURL: "https://x.example"))
    }
}

final class RelativeTimeTests: XCTestCase {
    private let locale = Locale(identifier: "en_US")

    func testNilIsNeverSynced() {
        XCTAssertEqual(RelativeTime.lastSynced(nil, locale: locale), "Never synced")
    }

    func testUnderAMinuteIsJustNow() {
        let now = Date()
        XCTAssertEqual(RelativeTime.lastSynced(now.addingTimeInterval(-30), now: now, locale: locale), "Just now")
    }

    func testMinutesAreSpelledOutAndCapitalised() {
        let now = Date()
        XCTAssertEqual(RelativeTime.lastSynced(now.addingTimeInterval(-300), now: now, locale: locale), "5 minutes ago")
    }
}
