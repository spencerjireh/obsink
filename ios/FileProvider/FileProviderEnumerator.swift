import FileProvider
import Foundation

/// Enumerates the shared `ItemStore`. Children come straight from the DB; change
/// deltas are driven by the monotonic `rowVersion` anchor. Tombstoned rows
/// (`isDeleted`) surface as deletes, everything else as updates.
///
/// Both enumerations are paged (`pageSize` rows per call) so a large vault's
/// metadata never lands in one array inside the extension's memory budget.
/// The working set (`.workingSet`) is every live item; the trash is empty
/// because the vault has no trash of its own.
final class FileProviderEnumerator: NSObject, NSFileProviderEnumerator {
    static let pageSize = 500

    private let container: NSFileProviderItemIdentifier
    private let store: ItemStore

    init(container: NSFileProviderItemIdentifier, store: ItemStore) {
        self.container = container
        self.store = store
    }

    func invalidate() {}

    func enumerateItems(for observer: NSFileProviderEnumerationObserver, startingAt page: NSFileProviderPage) {
        if container == .trashContainer {
            observer.didEnumerate([])
            observer.finishEnumerating(upTo: nil)
            return
        }
        let offset = Self.decodePage(page)
        let rows: [ItemRecord]
        do {
            rows = container == .workingSet
                ? try store.allItems(limit: Self.pageSize, offset: offset)
                : try store.children(of: Self.parentID(for: container), limit: Self.pageSize, offset: offset)
        } catch {
            observer.finishEnumeratingWithError(error)
            return
        }
        observer.didEnumerate(rows.map(FileProviderItem.init(record:)))
        let more = rows.count == Self.pageSize
        observer.finishEnumerating(upTo: more ? Self.encodePage(offset + rows.count) : nil)
    }

    func enumerateChanges(for observer: NSFileProviderChangeObserver, from anchor: NSFileProviderSyncAnchor) {
        let fromVersion = Self.decode(anchor) ?? 0
        if container == .trashContainer {
            observer.finishEnumeratingChanges(upTo: Self.encode(fromVersion), moreComing: false)
            return
        }
        let changed: [ItemRecord]
        do {
            changed = try store.changes(from: fromVersion, limit: Self.pageSize)
        } catch {
            observer.finishEnumeratingWithError(error)
            return
        }

        var updates: [NSFileProviderItem] = []
        var deletes: [NSFileProviderItemIdentifier] = []
        for rec in changed {
            if rec.isDeleted {
                deletes.append(NSFileProviderItemIdentifier(rec.identifier))
            } else {
                updates.append(FileProviderItem(record: rec))
            }
        }
        if !updates.isEmpty { observer.didUpdate(updates) }
        if !deletes.isEmpty { observer.didDeleteItems(withIdentifiers: deletes) }

        // The next anchor is the last row handed over, not MAX(rowVersion):
        // a write that lands between the read and the anchor would otherwise
        // be skipped forever.
        let last = changed.last?.rowVersion ?? fromVersion
        let more = changed.count == Self.pageSize
        observer.finishEnumeratingChanges(upTo: Self.encode(max(fromVersion, last)), moreComing: more)
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        completionHandler(Self.encode((try? store.currentAnchor()) ?? 0))
    }

    private static func parentID(for container: NSFileProviderItemIdentifier) -> String {
        container == .rootContainer ? "" : container.rawValue
    }

    // MARK: Anchor codec (8-byte big-endian Int64)

    static func encode(_ value: Int64) -> NSFileProviderSyncAnchor {
        NSFileProviderSyncAnchor(bigEndianData(value))
    }

    static func decode(_ anchor: NSFileProviderSyncAnchor) -> Int64? {
        int64(fromBigEndian: anchor.rawValue)
    }

    // MARK: Page codec (8-byte big-endian row offset)

    /// The system's initial pages (`initialPageSortedByDate`/`ByName`, which
    /// are ASCII tags, not offsets) and any short payload decode to offset 0.
    static func decodePage(_ page: NSFileProviderPage) -> Int {
        let raw = page.rawValue
        if raw == NSFileProviderPage.initialPageSortedByDate as Data
            || raw == NSFileProviderPage.initialPageSortedByName as Data {
            return 0
        }
        guard let offset = int64(fromBigEndian: raw), offset >= 0, offset <= Int64(Int.max) else { return 0 }
        return Int(offset)
    }

    static func encodePage(_ offset: Int) -> NSFileProviderPage {
        NSFileProviderPage(bigEndianData(Int64(offset)))
    }

    private static func bigEndianData(_ value: Int64) -> Data {
        var be = value.bigEndian
        return withUnsafeBytes(of: &be) { Data($0) }
    }

    private static func int64(fromBigEndian data: Data) -> Int64? {
        guard data.count >= 8 else { return nil }
        var be: Int64 = 0
        withUnsafeMutableBytes(of: &be) { dst in
            dst.copyBytes(from: data.prefix(8))
        }
        return Int64(bigEndian: be)
    }
}
