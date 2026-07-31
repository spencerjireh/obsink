import XCTest
import FileProvider

/// Validates OBS-12: the enumerator splits `ItemStore.changes(from:)` into updates
/// and deletes (tombstones) for the File Provider change observer.
final class FileProviderEnumeratorTests: XCTestCase {

    private func makeStore() throws -> ItemStore {
        let id = UUID().uuidString
        let dbURL = FileManager.default.temporaryDirectory.appendingPathComponent("obsink-fp-\(id).sqlite")
        return try ItemStore(databaseURL: dbURL)
    }

    func testChangesSplitIntoUpdatesAndDeletes() throws {
        let store = try makeStore()
        try store.upsert(ItemRecord(identifier: "A", parentIdentifier: "", filename: "a.md",
                                    contentHash: nil, localPath: "a.md", isDirectory: false, size: 1, modified: 1))
        try store.upsert(ItemRecord(identifier: "B", parentIdentifier: "", filename: "b.md",
                                    contentHash: nil, localPath: "b.md", isDirectory: false, size: 1, modified: 1))
        let anchor = try store.currentAnchor()

        // Update A (any rowVersion bump); tombstone B.
        try store.setPending(identifier: "A", upload: true)
        try store.upsert(ItemRecord(identifier: "B", parentIdentifier: "", filename: "b.md",
                                    contentHash: nil, localPath: "b.md", isDirectory: false, size: 1,
                                    modified: 1, isDeleted: true, rowVersion: try store.currentAnchor() + 1))

        let enumerator = FileProviderEnumerator(container: .rootContainer, store: store)
        let spy = ChangeSpy()
        enumerator.enumerateChanges(for: spy, from: Self.zeroAnchor)

        XCTAssertEqual(spy.updated.map(\.itemIdentifier.rawValue), ["A"])
        XCTAssertEqual(spy.deleted.map(\.rawValue), ["B"])
        XCTAssertTrue(spy.finished)
    }

    private static var zeroAnchor: NSFileProviderSyncAnchor {
        var be = Int64(0).bigEndian
        return NSFileProviderSyncAnchor(withUnsafeBytes(of: &be) { Data($0) })
    }
}

final class ChangeSpy: NSObject, NSFileProviderChangeObserver {
    var updated: [NSFileProviderItem] = []
    var deleted: [NSFileProviderItemIdentifier] = []
    var finished = false

    func didUpdate(_ updatedItems: [NSFileProviderItem]) {
        updated.append(contentsOf: updatedItems)
    }

    func didDeleteItems(withIdentifiers deletedItemIdentifiers: [NSFileProviderItemIdentifier]) {
        deleted.append(contentsOf: deletedItemIdentifiers)
    }

    func finishEnumeratingChanges(upTo _: NSFileProviderSyncAnchor, moreComing _: Bool) {
        finished = true
    }

    func finishEnumeratingWithError(_ error: Error) {}
}
