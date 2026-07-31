import Foundation
import GRDB

/// Shared SQLite item store (spec §11.5) backing the File Provider. One row per
/// vault item, keyed by a stable UUID. Both the ObSink app and the File Provider
/// extension open the same database file in the App Group container.
final class ItemStore {
    static let appGroup = "group.com.obsink.shared"

    /// Singleton used by the app and extension. Points at the App Group database.
    /// Failing to open the backing store is fatal: the FP can't operate without it.
    static let shared: ItemStore = {
        try! ItemStore(databaseURL: ItemStore.defaultDatabaseURL())
    }()

    private let dbQueue: DatabaseQueue

    /// Open (or create) the store at `databaseURL`. Tests pass a temp URL.
    init(databaseURL: URL) throws {
        var config = Configuration()
        config.label = "obsink.itemstore"
        // WAL lets the app and the extension read concurrently with a single writer.
        config.prepareDatabase { db in
            try db.execute(sql: "PRAGMA journal_mode=WAL")
        }
        self.dbQueue = try DatabaseQueue(path: databaseURL.path, configuration: config)
        try Self.migrator.migrate(dbQueue)
    }

    private static var migrator: DatabaseMigrator {
        var m = DatabaseMigrator()
        m.registerMigration("v1_items") { db in
            try db.create(table: "items") { t in
                t.column("identifier", .text).notNull().primaryKey()
                t.column("parentIdentifier", .text).notNull()
                t.column("filename", .text).notNull()
                t.column("contentHash", .text)
                t.column("localPath", .text).notNull()
                t.column("isDirectory", .boolean).notNull().defaults(to: false)
                t.column("size", .integer)
                t.column("modified", .integer).notNull().defaults(to: 0)
                t.column("pendingUpload", .boolean).notNull().defaults(to: false)
                t.column("pendingDeletion", .boolean).notNull().defaults(to: false)
                t.column("rowVersion", .integer).notNull().defaults(to: 0)
            }
            try db.create(index: "items_parent", on: "items", columns: ["parentIdentifier"])
            try db.create(index: "items_rowVersion", on: "items", columns: ["rowVersion"])
            try db.create(table: "meta") { t in
                t.column("key", .text).notNull().primaryKey()
                t.column("value", .text).notNull()
            }
        }
        m.registerMigration("v2_tombstones") { db in
            // Tombstones let enumerateChanges report deletes: reconcile marks a
            // vanished file isDeleted (bumping rowVersion) instead of hard-deleting.
            try db.alter(table: "items") { t in
                t.add(column: "isDeleted", .boolean).notNull().defaults(to: false)
            }
            try db.create(index: "items_isDeleted", on: "items", columns: ["isDeleted"])
        }
        return m
    }

    static func defaultDatabaseURL() -> URL {
        let base = FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: appGroup)
            ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        return base.appendingPathComponent("obsink.sqlite")
    }

    // MARK: Reads

    func item(for identifier: String) throws -> ItemRecord? {
        try dbQueue.read { db in
            try ItemRecord
                .filter(Column("identifier") == identifier)
                .filter(Column("isDeleted") == false)
                .fetchOne(db)
        }
    }

    func item(path: String) throws -> ItemRecord? {
        try dbQueue.read { db in
            try ItemRecord
                .filter(Column("localPath") == path)
                .filter(Column("isDeleted") == false)
                .fetchOne(db)
        }
    }

    func children(of parentIdentifier: String) throws -> [ItemRecord] {
        try dbQueue.read { db in
            try ItemRecord
                .filter(Column("parentIdentifier") == parentIdentifier)
                .filter(Column("isDeleted") == false)
                .order(Column("filename"))
                .fetchAll(db)
        }
    }

    /// Items changed since `anchor` (rowVersion strictly greater), INCLUDING
    /// tombstones (`isDeleted`). Slice B's enumerator maps tombstones to deletes.
    func changes(from anchor: Int64) throws -> [ItemRecord] {
        try dbQueue.read { db in
            try ItemRecord
                .filter(Column("rowVersion") > anchor)
                .order(Column("rowVersion"))
                .fetchAll(db)
        }
    }

    func currentAnchor() throws -> Int64 {
        try dbQueue.read { db in try Self.maxRowVersion(db) }
    }

    func pendingCount() throws -> Int {
        try dbQueue.read { db in
            try ItemRecord
                .filter(Column("pendingUpload") || Column("pendingDeletion"))
                .fetchCount(db)
        }
    }

    // MARK: Writes

    /// Insert/replace a record (caller assigns `rowVersion`).
    func upsert(_ record: ItemRecord) throws {
        try dbQueue.write { db in
            var rec = record
            try rec.insert(db, onConflict: .replace)
        }
    }

    /// Rename/move an item, keeping its identifier stable (spec §11.6). Used by
    /// the File Provider's `modifyItem` (Slice D) — a disk scan cannot detect
    /// renames, so the FP must report them by identifier.
    @discardableResult
    func rename(identifier: String,
                toPath localPath: String,
                filename: String,
                parentIdentifier: String) throws -> ItemRecord? {
        try dbQueue.write { db in
            guard var rec = try ItemRecord.filter(Column("identifier") == identifier).fetchOne(db) else {
                return nil
            }
            rec.filename = filename
            rec.localPath = localPath
            rec.parentIdentifier = parentIdentifier
            rec.rowVersion = try Self.maxRowVersion(db) + 1
            try rec.update(db)
            return rec
        }
    }

    func setPending(identifier: String, upload: Bool = false, deletion: Bool = false) throws {
        try dbQueue.write { db in
            guard var rec = try ItemRecord.filter(Column("identifier") == identifier).fetchOne(db) else { return }
            if upload { rec.pendingUpload = true }
            // A queued deletion also tombstones the row so it drops out of
            // enumeration immediately; the host app removes it after syncing.
            if deletion { rec.pendingDeletion = true; rec.isDeleted = true }
            rec.rowVersion = try Self.maxRowVersion(db) + 1
            try rec.update(db)
        }
    }

    func clearPending(identifier: String) throws {
        try dbQueue.write { db in
            guard var rec = try ItemRecord.filter(Column("identifier") == identifier).fetchOne(db) else { return }
            rec.pendingUpload = false
            rec.pendingDeletion = false
            try rec.update(db)
        }
    }

    /// Drain the pending flags after a completed sync (OBS-22/23). The core sync
    /// already pushed the uploads/deletes by scanning the vault dir; here we just
    /// clear `pendingUpload` (content is now on the server) and remove rows marked
    /// `pendingDeletion` (the deletion has propagated). No-op unless `completed`.
    func drainPendingAfterSync(completed: Bool) throws {
        guard completed else { return }
        try dbQueue.write { db in
            try db.execute(sql: "UPDATE items SET pendingUpload = 0 WHERE pendingUpload = 1")
            try db.execute(sql: "DELETE FROM items WHERE pendingDeletion = 1")
        }
    }

    /// Remove a row (after a deletion has propagated to the server).
    func remove(identifier: String) throws {
        try dbQueue.write { db in
            _ = try ItemRecord.filter(Column("identifier") == identifier).deleteAll(db)
        }
    }

    // MARK: Reconcile (mirror the on-disk vault into the DB)

    /// Reconcile only when a sync just completed (OBS-20). Gated so an aborted or
    /// conflict-paused sync doesn't rewrite the DB. The host app calls this right
    /// after a successful sync, then signals the File Provider (OBS-21).
    func reconcileAfterSync(completed: Bool, vaultRoot: URL) throws {
        guard completed else { return }
        try reconcile(vaultRoot: vaultRoot)
    }

    /// Scan `vaultRoot` and upsert item rows, assigning stable UUIDs on first
    /// encounter and bumping `rowVersion` only for genuinely changed/new items.
    /// Rows for paths no longer on disk are dropped (the sync engine's
    /// working-manifest delete-detection handles server propagation).
    func reconcile(vaultRoot: URL) throws {
        try dbQueue.write { db in
            let existing: [String: ItemRecord] = try Dictionary(
                ItemRecord.fetchAll(db).map { ($0.localPath, $0) },
                uniquingKeysWith: { a, _ in a }
            )
            let scanned = try Self.scan(vaultRoot: vaultRoot)
            let sorted = scanned.sorted { $0.path.count < $1.path.count } // parents first

            var nextVersion = (try Self.maxRowVersion(db)) + 1
            var pathToUUID: [String: String] = [:]
            var seen: Set<String> = []

            for entry in sorted {
                let parentIdentifier: String
                if let parentPath = Self.parentPath(of: entry.path) {
                    parentIdentifier = pathToUUID[parentPath] ?? ""
                } else {
                    parentIdentifier = ""
                }
                let id = try Self.upsertEntry(
                    entry, parentIdentifier: parentIdentifier, existing: existing,
                    db: db, nextVersion: &nextVersion
                )
                pathToUUID[entry.path] = id
                seen.insert(entry.path)
            }

            for (path, rec) in existing where !seen.contains(path) && !rec.pendingUpload && !rec.pendingDeletion {
                if rec.isDeleted { continue }
                var r = rec
                r.isDeleted = true
                r.rowVersion = nextVersion
                nextVersion += 1
                try r.insert(db, onConflict: .replace)
            }
        }
    }

    // MARK: Helpers

    private struct ScannedEntry {
        let path: String
        let filename: String
        let isDirectory: Bool
        let size: Int64?
        let modified: Int64
    }

    private static func scan(vaultRoot: URL) throws -> [ScannedEntry] {
        guard let enumerator = FileManager.default.enumerator(
            at: vaultRoot,
            includingPropertiesForKeys: [.isDirectoryKey, .fileSizeKey, .contentModificationDateKey]
        ) else { return [] }
        let prefix = vaultRoot.path + "/"
        var out: [ScannedEntry] = []
        for case let url as URL in enumerator {
            let rel = url.path.replacingOccurrences(of: prefix, with: "")
            if rel.isEmpty || rel == ".obsink" || rel.hasPrefix(".obsink/") { continue }
            let vals = try? url.resourceValues(forKeys: [.isDirectoryKey, .fileSizeKey, .contentModificationDateKey])
            out.append(ScannedEntry(
                path: rel,
                filename: (rel as NSString).lastPathComponent,
                isDirectory: vals?.isDirectory ?? false,
                size: vals?.fileSize.map(Int64.init),
                modified: Int64((vals?.contentModificationDate ?? Date()).timeIntervalSince1970)
            ))
        }
        return out
    }

    /// Upsert one scanned entry. Returns the item's identifier. Bumps the running
    /// `nextVersion` (and writes the bump) only for new or genuinely changed rows.
    private static func upsertEntry(
        _ entry: ScannedEntry,
        parentIdentifier: String,
        existing: [String: ItemRecord],
        db: Database,
        nextVersion: inout Int64
    ) throws -> String {
        if let rec = existing[entry.path] {
            // Resurrection: the path reappeared after being tombstoned. Reuse the
            // existing UUID and clear the tombstone (the FP sees an update, not a
            // delete + insert).
            if rec.isDeleted {
                var r = rec
                r.isDeleted = false
                r.filename = entry.filename
                r.isDirectory = entry.isDirectory
                r.size = entry.size
                r.modified = entry.modified
                r.parentIdentifier = parentIdentifier
                r.rowVersion = nextVersion
                nextVersion += 1
                try r.insert(db, onConflict: .replace)
                return r.identifier
            }
            let changed = rec.size != entry.size
                || rec.modified != entry.modified
                || rec.isDirectory != entry.isDirectory
                || rec.parentIdentifier != parentIdentifier
                || rec.filename != entry.filename
            guard changed else { return rec.identifier }
            var r = rec
            r.filename = entry.filename
            r.isDirectory = entry.isDirectory
            r.size = entry.size
            r.modified = entry.modified
            r.parentIdentifier = parentIdentifier
            r.rowVersion = nextVersion
            nextVersion += 1
            try r.insert(db, onConflict: .replace)
            return r.identifier
        }
        var rec = ItemRecord(
            identifier: UUID().uuidString,
            parentIdentifier: parentIdentifier,
            filename: entry.filename,
            contentHash: nil,
            localPath: entry.path,
            isDirectory: entry.isDirectory,
            size: entry.size,
            modified: entry.modified,
            rowVersion: nextVersion
        )
        nextVersion += 1
        try rec.insert(db)
        return rec.identifier
    }

    private static func parentPath(of path: String) -> String? {
        let s = (path as NSString).deletingLastPathComponent
        return s.isEmpty ? nil : s
    }

    private static func maxRowVersion(_ db: Database) throws -> Int64 {
        try Int64.fetchOne(db, sql: "SELECT MAX(rowVersion) FROM items") ?? 0
    }
}
