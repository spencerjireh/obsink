import XCTest

/// Coverage for the shared item database (Slice A — OBS-7/8/9/10/18).
///
/// `ItemStore` is compiled into this test target via `ios/DB/`, so these tests
/// exercise the real store logic against a temp SQLite file (no app host needed).
final class ItemStoreTests: XCTestCase {

    private func makeStore() throws -> (store: ItemStore, root: URL, dbURL: URL) {
        let id = UUID().uuidString
        let tmp = FileManager.default.temporaryDirectory
        let root = tmp.appendingPathComponent("obsink-test-\(id)/Vault")
        try? FileManager.default.removeItem(at: root)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let dbURL = tmp.appendingPathComponent("obsink-\(id).sqlite")
        let store = try ItemStore(databaseURL: dbURL)
        return (store, root, dbURL)
    }

    // OBS-8 schema + OBS-9 CRUD + OBS-18 UUID/parent assignment
    func testReconcileAssignsUUIDsAndParents() throws {
        let (store, root, _) = try makeStore()
        try Data("a".utf8).write(to: root.appendingPathComponent("a.md"))
        try FileManager.default.createDirectory(at: root.appendingPathComponent("notes"), withIntermediateDirectories: true)
        try Data("bb".utf8).write(to: root.appendingPathComponent("notes/b.md"))
        try store.reconcile(vaultRoot: root)

        let a = try store.item(path: "a.md")
        let notes = try store.item(path: "notes")
        let b = try store.item(path: "notes/b.md")
        XCTAssertNotNil(a)
        XCTAssertNotNil(notes); XCTAssertTrue(notes?.isDirectory == true)
        XCTAssertNotNil(b)
        XCTAssertEqual(a?.parentIdentifier, "")                // top-level → root container
        XCTAssertEqual(b?.parentIdentifier, notes?.identifier) // child → parent UUID (not parent path)
    }

    // OBS-18 + OBS-12: no spurious version bumps when nothing changed
    func testReconcileIsIdempotent() throws {
        let (store, root, _) = try makeStore()
        try Data("a".utf8).write(to: root.appendingPathComponent("a.md"))
        try store.reconcile(vaultRoot: root)
        let idBefore = try store.item(path: "a.md")?.identifier
        let anchorBefore = try store.currentAnchor()
        try store.reconcile(vaultRoot: root)
        XCTAssertEqual(try store.item(path: "a.md")?.identifier, idBefore)
        XCTAssertEqual(try store.currentAnchor(), anchorBefore)
    }

    // OBS-12 / OBS-17: a real content change is reported via rowVersion
    func testReconcileReportsContentChange() throws {
        let (store, root, _) = try makeStore()
        let f = root.appendingPathComponent("a.md")
        try Data("a".utf8).write(to: f)
        try store.reconcile(vaultRoot: root)
        let anchor = try store.currentAnchor()

        try Data("changed".utf8).write(to: f)
        try FileManager.default.setAttributes([.modificationDate: Date().addingTimeInterval(3600)], ofItemAtPath: f.path)
        try store.reconcile(vaultRoot: root)

        let changes = try store.changes(from: anchor)
        XCTAssertEqual(changes.count, 1)
        XCTAssertEqual(changes.first?.localPath, "a.md")
    }

    // OBS-18: a rename keeps the identifier (disk scan can't do this; FP modifyItem will)
    func testRenameKeepsIdentifier() throws {
        let (store, root, _) = try makeStore()
        try Data("a".utf8).write(to: root.appendingPathComponent("old.md"))
        try store.reconcile(vaultRoot: root)
        let id = try store.item(path: "old.md")?.identifier
        XCTAssertNotNil(id)

        let renamed = try store.rename(identifier: id!, toPath: "new.md", filename: "new.md", parentIdentifier: "")
        XCTAssertEqual(renamed?.identifier, id)
        XCTAssertEqual(renamed?.localPath, "new.md")
        XCTAssertNil(try store.item(path: "old.md"))
    }

    // OBS-10: both targets share the same DB file (two instances emulate app + extension)
    func testSharedDatabaseAcrossInstances() throws {
        let id = UUID().uuidString
        let dbURL = FileManager.default.temporaryDirectory.appendingPathComponent("obsink-share-\(id).sqlite")
        let writer = try ItemStore(databaseURL: dbURL)
        try writer.upsert(ItemRecord(identifier: "X", parentIdentifier: "", filename: "x.md",
                                     contentHash: nil, localPath: "x.md", isDirectory: false,
                                     size: 1, modified: 1))
        let reader = try ItemStore(databaseURL: dbURL)
        XCTAssertEqual(try reader.item(for: "X")?.filename, "x.md")
    }

    // OBS-9 CRUD + Slice D foundation: pending flags + ordered children
    func testChildrenOrdering() throws {
        let (store, _, _) = try makeStore()
        let parent = "notes-id"
        try store.upsert(ItemRecord(identifier: parent, parentIdentifier: "", filename: "notes",
                                    contentHash: nil, localPath: "notes", isDirectory: true, size: nil, modified: 1))
        try store.upsert(ItemRecord(identifier: "c2", parentIdentifier: parent, filename: "c2.md",
                                    contentHash: nil, localPath: "notes/c2.md", isDirectory: false, size: 1, modified: 1))
        try store.upsert(ItemRecord(identifier: "c1", parentIdentifier: parent, filename: "c1.md",
                                    contentHash: nil, localPath: "notes/c1.md", isDirectory: false, size: 1, modified: 1))
        let kids = try store.children(of: parent)
        XCTAssertEqual(kids.map(\.filename), ["c1.md", "c2.md"])
    }

    // OBS-20: reconcile is gated on a completed sync (no rewrite on conflict-pause).
    func testReconcileAfterSyncGatedOnCompleted() throws {
        let (store, root, _) = try makeStore()
        try Data("hi".utf8).write(to: root.appendingPathComponent("a.md"))
        try store.reconcileAfterSync(completed: false, vaultRoot: root)
        XCTAssertEqual(try store.children(of: "").count, 0)
        try store.reconcileAfterSync(completed: true, vaultRoot: root)
        XCTAssertEqual(try store.children(of: "").count, 1)
    }

    func testPendingFlags() throws {
        let (store, _, _) = try makeStore()
        try store.upsert(ItemRecord(identifier: "P", parentIdentifier: "", filename: "p.md",
                                    contentHash: nil, localPath: "p.md", isDirectory: false, size: 1, modified: 1))
        XCTAssertEqual(try store.pendingCount(), 0)
        try store.setPending(identifier: "P", upload: true)
        XCTAssertEqual(try store.pendingCount(), 1)
        try store.clearPending(identifier: "P")
        XCTAssertEqual(try store.pendingCount(), 0)
    }

    // OBS-12: a file that vanished from disk is tombstoned (not hard-deleted) and
    // surfaces in changes() for the enumerator to report as a delete.
    func testReconcileTombstonesVanishedFiles() throws {
        let (store, root, _) = try makeStore()
        let f = root.appendingPathComponent("gone.md")
        try Data("x".utf8).write(to: f)
        try store.reconcile(vaultRoot: root)
        let anchor = try store.currentAnchor()

        try FileManager.default.removeItem(at: f)
        try store.reconcile(vaultRoot: root)

        // Hidden from normal reads (so the FP doesn't list it)...
        XCTAssertNil(try store.item(path: "gone.md"))
        // ...but present in changes() as a tombstone.
        let deltas = try store.changes(from: anchor)
        XCTAssertEqual(deltas.count, 1)
        XCTAssertEqual(deltas.first?.localPath, "gone.md")
        XCTAssertTrue(deltas.first?.isDeleted == true)
    }

    // OBS-18: a path that reappears after tombstoning resurrects (reuses its UUID).
    func testReconcileResurrectsReappearingPath() throws {
        let (store, root, _) = try makeStore()
        let f = root.appendingPathComponent("ghost.md")
        try Data("first".utf8).write(to: f)
        try store.reconcile(vaultRoot: root)
        let id = try store.item(path: "ghost.md")?.identifier

        try FileManager.default.removeItem(at: f)
        try store.reconcile(vaultRoot: root) // tombstone
        try Data("back".utf8).write(to: f)
        try store.reconcile(vaultRoot: root) // resurrect

        let rec = try store.item(path: "ghost.md")
        XCTAssertEqual(rec?.identifier, id)     // UUID preserved
        XCTAssertTrue(rec?.isDeleted == false)
    }

    // OBS-16: a queued deletion tombstones immediately (drops out of enumeration).
    func testPendingDeletionTombstonesImmediately() throws {
        let (store, _, _) = try makeStore()
        try store.upsert(ItemRecord(identifier: "D", parentIdentifier: "", filename: "d.md",
                                    contentHash: nil, localPath: "d.md", isDirectory: false, size: 1, modified: 1))
        XCTAssertEqual(try store.children(of: "").count, 1)
        try store.setPending(identifier: "D", deletion: true)
        XCTAssertEqual(try store.children(of: "").count, 0) // excluded from enumeration
        XCTAssertNil(try store.item(for: "D"))
    }

    // OBS-22/23: a completed sync drains both pending kinds; a paused sync doesn't.
    func testDrainPendingAfterSync() throws {
        let (store, _, _) = try makeStore()
        try store.upsert(ItemRecord(identifier: "U", parentIdentifier: "", filename: "u.md",
                                    contentHash: nil, localPath: "u.md", isDirectory: false, size: 1, modified: 1,
                                    pendingUpload: true))
        try store.upsert(ItemRecord(identifier: "D", parentIdentifier: "", filename: "d.md",
                                    contentHash: nil, localPath: "d.md", isDirectory: false, size: 1, modified: 1,
                                    pendingDeletion: true, isDeleted: true))
        XCTAssertEqual(try store.pendingCount(), 2)

        try store.drainPendingAfterSync(completed: false) // conflict-paused: no-op
        XCTAssertEqual(try store.pendingCount(), 2)

        try store.drainPendingAfterSync(completed: true)
        XCTAssertEqual(try store.pendingCount(), 0)
        XCTAssertFalse(try store.item(for: "U")?.pendingUpload ?? true) // upload flag cleared
        XCTAssertNil(try store.item(for: "D"))                          // deletion row removed
    }

    // OBS-93: a tombstone and a live row for the same path (deleted in the FP,
    // then re-created) must not both survive a reconcile.
    func testReconcileDropsStaleTombstoneBesideLiveRow() throws {
        let (store, root, _) = try makeStore()
        let file = root.appendingPathComponent("a.md")
        try Data("a".utf8).write(to: file)
        try store.reconcile(vaultRoot: root)
        let firstID = try XCTUnwrap(try store.item(path: "a.md")?.identifier)

        // Deleted on disk: reconcile tombstones the row.
        try FileManager.default.removeItem(at: file)
        try store.reconcile(vaultRoot: root)
        XCTAssertNil(try store.item(path: "a.md"))

        // Re-created through the FP with a fresh identifier (createItem path).
        try Data("again".utf8).write(to: file)
        try store.upsert(ItemRecord(identifier: "fresh", parentIdentifier: "", filename: "a.md",
                                    contentHash: nil, localPath: "a.md", isDirectory: false,
                                    size: 5, modified: 1, pendingUpload: true))
        try store.drainPendingAfterSync(completed: true)
        try store.reconcile(vaultRoot: root)

        let rows = try store.changes(from: 0).filter { $0.localPath == "a.md" }
        XCTAssertEqual(rows.count, 1, "one row per path after reconcile")
        XCTAssertEqual(rows.first?.identifier, "fresh")
        XCTAssertFalse(rows.first?.isDeleted ?? true)
        XCTAssertNotEqual(rows.first?.identifier, firstID)
    }

    // OBS-93: per-vault stores are separate databases.
    func testStoresForDifferentVaultsDoNotShareRows() throws {
        XCTAssertNotEqual(
            ItemStore.defaultDatabaseURL(vaultID: "vault_a"),
            ItemStore.defaultDatabaseURL(vaultID: "vault_b")
        )
        XCTAssertTrue(ItemStore.defaultDatabaseURL(vaultID: "vault_a").lastPathComponent.contains("vault_a"))
    }

    // OBS-93: fetchContents hands the system a copy, never the vault file.
    func testStagedCopyLeavesSourceInPlace() throws {
        let (_, root, _) = try makeStore()
        let source = root.appendingPathComponent("note.md")
        try Data("body".utf8).write(to: source)
        let staging = root.deletingLastPathComponent().appendingPathComponent("staging")

        let copy = try FileProviderPaths.stagedCopy(of: source, in: staging)

        XCTAssertTrue(FileManager.default.fileExists(atPath: source.path))
        XCTAssertNotEqual(copy, source)
        XCTAssertEqual(try Data(contentsOf: copy), Data("body".utf8))
        try FileManager.default.removeItem(at: copy)
        XCTAssertTrue(FileManager.default.fileExists(atPath: source.path))
    }

    // OBS-100: removing a vault closes its cached store; a later `store(for:)`
    // opens a fresh one instead of reusing a closed queue.
    func testForgetClosesAndReopens() throws {
        let id = "vault_forget_\(UUID().uuidString)"
        let first = try ItemStore.store(for: id)
        XCTAssertEqual(try first.pendingCount(), 0)

        ItemStore.forget(vaultID: id)
        XCTAssertThrowsError(try first.pendingCount(), "the closed queue rejects work")

        let second = try ItemStore.store(for: id)
        XCTAssertEqual(try second.pendingCount(), 0)
        ItemStore.forget(vaultID: id)
        for suffix in ["", "-wal", "-shm"] {
            try? FileManager.default.removeItem(atPath: ItemStore.defaultDatabaseURL(vaultID: id).path + suffix)
        }
    }

    // MARK: OBS-103: File Provider fixes

    private func record(_ id: String, parent: String, path: String, directory: Bool = false) -> ItemRecord {
        ItemRecord(identifier: id, parentIdentifier: parent, filename: (path as NSString).lastPathComponent,
                   contentHash: nil, localPath: path, isDirectory: directory, size: directory ? nil : 1, modified: 1)
    }

    func testInsertAssignsRowVersionVisibleInChanges() throws {
        let (store, _, _) = try makeStore()
        let stored = try store.insert(record("A", parent: "", path: "a.md"))
        XCTAssertEqual(stored.rowVersion, 1)
        let again = try store.insert(record("B", parent: "", path: "b.md"))
        XCTAssertEqual(again.rowVersion, 2)
        XCTAssertEqual(try store.changes(from: 0).map(\.identifier), ["A", "B"])
        XCTAssertEqual(try store.changes(from: 1).map(\.identifier), ["B"])
    }

    func testUpdateContentBumpsRowVersionAndStoresBytesMetadata() throws {
        let (store, _, _) = try makeStore()
        let stored = try store.insert(record("A", parent: "", path: "a.md"))
        let updated = try store.updateContent(identifier: "A", size: 42, modified: 99)
        XCTAssertEqual(updated?.size, 42)
        XCTAssertEqual(updated?.modified, 99)
        XCTAssertEqual(updated?.rowVersion, stored.rowVersion + 1)
        XCTAssertNil(try store.updateContent(identifier: "missing", size: 1, modified: 1))
    }

    func testRenamingADirectoryRewritesDescendantPathsAndKeepsIdentifiers() throws {
        let (store, _, _) = try makeStore()
        try store.insert(record("D", parent: "", path: "notes", directory: true))
        try store.insert(record("S", parent: "D", path: "notes/sub", directory: true))
        try store.insert(record("F", parent: "D", path: "notes/a.md"))
        try store.insert(record("G", parent: "S", path: "notes/sub/b.md"))
        try store.insert(record("X", parent: "", path: "notes_other.md"))   // LIKE `_` must not match
        let before = try store.currentAnchor()

        let moved = try store.rename(identifier: "D", toPath: "archive/notes", filename: "notes", parentIdentifier: "R")
        XCTAssertEqual(moved?.localPath, "archive/notes")
        XCTAssertEqual(moved?.rowVersion, before + 1)
        XCTAssertEqual(try store.item(for: "F")?.localPath, "archive/notes/a.md")
        XCTAssertEqual(try store.item(for: "S")?.localPath, "archive/notes/sub")
        XCTAssertEqual(try store.item(for: "G")?.localPath, "archive/notes/sub/b.md")
        XCTAssertEqual(try store.item(for: "F")?.parentIdentifier, "D")
        XCTAssertEqual(try store.item(for: "X")?.localPath, "notes_other.md")
        // Descendants keep their row versions: only the moved folder is a change.
        XCTAssertEqual(try store.changes(from: before).map(\.identifier), ["D"])
    }

    func testDeletingADirectoryTombstonesDescendantsAndDrainRemovesThem() throws {
        let (store, _, _) = try makeStore()
        try store.insert(record("D", parent: "", path: "notes", directory: true))
        try store.insert(record("F", parent: "D", path: "notes/a.md"))
        try store.insert(record("G", parent: "D", path: "notes/b.md"))
        try store.insert(record("K", parent: "", path: "keep.md"))
        let before = try store.currentAnchor()

        try store.setPending(identifier: "D", deletion: true)
        let changed = try store.changes(from: before)
        XCTAssertEqual(Set(changed.map(\.identifier)), ["D", "F", "G"])
        XCTAssertTrue(changed.allSatisfy { $0.isDeleted && $0.pendingDeletion })
        XCTAssertEqual(Set(changed.map(\.rowVersion)).count, 3, "each tombstone has its own version")
        XCTAssertNil(try store.item(for: "F"))
        XCTAssertNotNil(try store.item(for: "K"))
        XCTAssertEqual(try store.pendingCount(), 3)

        try store.drainPendingAfterSync(completed: true)
        XCTAssertEqual(try store.pendingCount(), 0)
        XCTAssertEqual(try store.changes(from: 0).map(\.identifier), ["K"])
    }

    func testPagedReadsCoverEveryRowOnce() throws {
        let (store, _, _) = try makeStore()
        try store.insert(record("D", parent: "", path: "d", directory: true))
        for i in 0..<7 {
            try store.insert(record("F\(i)", parent: "D", path: "d/f\(i).md"))
        }
        let page1 = try store.children(of: "D", limit: 3, offset: 0)
        let page2 = try store.children(of: "D", limit: 3, offset: 3)
        let page3 = try store.children(of: "D", limit: 3, offset: 6)
        XCTAssertEqual((page1 + page2 + page3).map(\.filename), (0..<7).map { "f\($0).md" })
        XCTAssertEqual(try store.allItems(limit: nil, offset: 0).count, 8)
        XCTAssertEqual(try store.allItems(limit: 5, offset: 5).count, 3)
        XCTAssertEqual(try store.changes(from: 0, limit: 2).map(\.identifier), ["D", "F0"])
    }
}
