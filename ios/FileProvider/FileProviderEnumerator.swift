import FileProvider
import Foundation

/// Enumerates the shared `ItemStore`. Children come straight from the DB; change
/// deltas are driven by the monotonic `rowVersion` anchor. Tombstoned rows
/// (`isDeleted`) surface as deletes, everything else as updates.
final class FileProviderEnumerator: NSObject, NSFileProviderEnumerator {
    private let container: NSFileProviderItemIdentifier
    private let store: ItemStore

    init(container: NSFileProviderItemIdentifier, store: ItemStore = .shared) {
        self.container = container
        self.store = store
    }

    func invalidate() {}

    func enumerateItems(for observer: NSFileProviderEnumerationObserver, startingAt _: NSFileProviderPage) {
        let parentID = container == .rootContainer ? "" : container.rawValue
        let kids = (try? store.children(of: parentID)) ?? []
        observer.didEnumerate(kids.map(FileProviderItem.init(record:)))
        observer.finishEnumerating(upTo: nil)
    }

    func enumerateChanges(for observer: NSFileProviderChangeObserver, from anchor: NSFileProviderSyncAnchor) {
        let fromVersion = Self.decode(anchor) ?? 0
        let changed = (try? store.changes(from: fromVersion)) ?? []

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

        let nextAnchor = (try? store.currentAnchor()) ?? fromVersion
        observer.finishEnumeratingChanges(upTo: Self.encode(nextAnchor), moreComing: false)
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        completionHandler(Self.encode((try? store.currentAnchor()) ?? 0))
    }

    // MARK: Anchor codec (8-byte big-endian Int64)

    private static func encode(_ value: Int64) -> NSFileProviderSyncAnchor {
        var be = value.bigEndian
        return NSFileProviderSyncAnchor(withUnsafeBytes(of: &be) { Data($0) })
    }

    private static func decode(_ anchor: NSFileProviderSyncAnchor) -> Int64? {
        guard anchor.rawValue.count >= 8 else { return nil }
        var be: Int64 = 0
        withUnsafeMutableBytes(of: &be) { dst in
            dst.copyBytes(from: anchor.rawValue.prefix(8))
        }
        return Int64(bigEndian: be)
    }
}
