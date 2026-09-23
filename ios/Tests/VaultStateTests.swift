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
        XCTAssertEqual(VaultState.statusLine(state { $0.hasStoredKey = false; $0.conflicts = 2 }), "Locked")
        XCTAssertEqual(VaultState.statusLine(state { $0.deletedOnServer = true; $0.hasStoredKey = false }), "Deleted on the server")
        XCTAssertEqual(VaultState.statusLine(state { $0.phase = .error("boom"); $0.deletedOnServer = true }), "Error: boom")
        XCTAssertEqual(VaultState.statusLine(state { $0.phase = .resolving; $0.phase = .syncing }), "Syncing…")
        XCTAssertEqual(VaultState.statusLine(state { $0.phase = .resolving }), "Resolving…")
    }

    func testCompletedOutcomeRecordsTheSyncAndClearsWhatItPulled() {
        var s = state { $0.staleDownloads = 4; $0.conflicts = 1; $0.phase = .syncing }
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        s.apply(outcome: SyncOutcome(uploaded: 1, downloaded: 4, conflicts: [], failures: [], completed: true, checkpointError: nil), now: now)
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
        s.apply(outcome: SyncOutcome(uploaded: 0, downloaded: 0, conflicts: [conflict], failures: [], completed: false, checkpointError: nil), now: Date())
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

    private func summary(_ id: String, _ name: String) -> MobileVaultSummary {
        MobileVaultSummary(id: id, name: name, created: 1, maxFileSize: 1, revision: 2, lastWrite: 0, bytes: 0,
                           wrappedKey: nil, devices: [])
    }

    // Spec §15.1: the vaults here first, then the account's others; the
    // server's name wins for a vault that is on both sides.
    func testRowsMergeTheServerListWithTheStoredEntries() {
        let entries = [VaultEntry(vaultID: "here", name: "Here (stale)"), VaultEntry(vaultID: "gone", name: "Gone")]
        let server = [summary("elsewhere", "Elsewhere"), summary("here", "Here")]
        let rows = VaultRow.merge(entries: entries, server: server)
        XCTAssertEqual(rows.map(\.id), ["here", "gone", "elsewhere"])
        XCTAssertEqual(rows.map(\.name), ["Here", "Gone", "Elsewhere"])
        XCTAssertEqual(rows.map(\.onDevice), [true, true, false])
        XCTAssertNotNil(rows[0].summary)
        XCTAssertNil(rows[1].summary, "the server no longer lists it")
    }

    func testRowsWithoutAServerAnswerAreTheStoredEntries() {
        let rows = VaultRow.merge(entries: [VaultEntry(vaultID: "a", name: "A")], server: nil)
        XCTAssertEqual(rows.map(\.id), ["a"])
        XCTAssertTrue(rows[0].onDevice)
    }

    // A pre-v3 entry carried a server URL; it reads and drops it.
    func testEntriesFromOlderBuildsDecode() throws {
        let json = #"[{"serverURL":"https://old.example","vaultID":"vault_a","name":"A"},{"workerURL":"https://w","vaultID":"vault_b","name":"B"}]"#
        let entries = try JSONDecoder().decode([VaultEntry].self, from: Data(json.utf8))
        XCTAssertEqual(entries.map(\.vaultID), ["vault_a", "vault_b"])
        let encoded = String(data: try JSONEncoder().encode(entries), encoding: .utf8)!
        XCTAssertFalse(encoded.contains("serverURL"))
    }

    func testPlatformNouns() {
        XCTAssertEqual(PlatformLabel.text("macos"), "Mac")
        XCTAssertEqual(PlatformLabel.text("ios"), "iPhone")
        XCTAssertEqual(PlatformLabel.text("browser"), "Browser")
        XCTAssertEqual(PlatformLabel.text("cli"), "CLI")
        XCTAssertEqual(PlatformLabel.text("unknown"), "Device")
    }

    func testPreviewTellsTextFromBinary() {
        XCTAssertEqual(SyncModel.previewText(Data("# hi".utf8)), "# hi")
        XCTAssertNil(SyncModel.previewText(Data([0xff, 0xfe, 0x00])))
        XCTAssertNil(SyncModel.previewText(Data("a\0b".utf8)))
        XCTAssertEqual(SyncModel.previewText(Data()), "")
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
