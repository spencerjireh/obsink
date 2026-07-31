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
}
