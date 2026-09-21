import XCTest
import FileProvider

/// Validates OBS-12 (change deltas split into updates and deletes) and OBS-103
/// (working set, trash, and paging in both enumerations).
final class FileProviderEnumeratorTests: XCTestCase {

    private func makeStore() throws -> ItemStore {
        let id = UUID().uuidString
        let dbURL = FileManager.default.temporaryDirectory.appendingPathComponent("obsink-fp-\(id).sqlite")
        return try ItemStore(databaseURL: dbURL)
    }

    private func record(_ id: String, parent: String, path: String, directory: Bool = false) -> ItemRecord {
        ItemRecord(identifier: id, parentIdentifier: parent, filename: (path as NSString).lastPathComponent,
                   contentHash: nil, localPath: path, isDirectory: directory, size: directory ? nil : 1, modified: 1)
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
        enumerator.enumerateChanges(for: spy, from: FileProviderEnumerator.encode(anchor))

        XCTAssertEqual(spy.updated.map(\.itemIdentifier.rawValue), ["A"])
        XCTAssertEqual(spy.deleted.map(\.rawValue), ["B"])
        XCTAssertTrue(spy.finished)
        XCTAssertEqual(spy.moreComing, false)
        XCTAssertEqual(spy.anchor.flatMap(FileProviderEnumerator.decode), try store.currentAnchor())
    }

    // OBS-103 §3.3: the working set is every live item, not the children of a
    // phantom parent.
    func testWorkingSetEnumeratesEveryLiveItem() throws {
        let store = try makeStore()
        try store.insert(record("D", parent: "", path: "notes", directory: true))
        try store.insert(record("A", parent: "", path: "a.md"))
        try store.insert(record("B", parent: "D", path: "notes/b.md"))
        try store.insert(record("T", parent: "D", path: "notes/t.md"))
        try store.setPending(identifier: "T", deletion: true)

        let spy = EnumerationSpy()
        FileProviderEnumerator(container: .workingSet, store: store)
            .enumerateItems(for: spy, startingAt: NSFileProviderPage(NSFileProviderPage.initialPageSortedByName as Data))
        XCTAssertEqual(Set(spy.items.map(\.itemIdentifier.rawValue)), ["D", "A", "B"])
        XCTAssertTrue(spy.finished)
        XCTAssertNil(spy.nextPage)
    }

    func testTrashIsEmpty() throws {
        let store = try makeStore()
        try store.insert(record("A", parent: "", path: "a.md"))

        let spy = EnumerationSpy()
        let enumerator = FileProviderEnumerator(container: .trashContainer, store: store)
        enumerator.enumerateItems(for: spy, startingAt: FileProviderEnumerator.encodePage(0))
        XCTAssertTrue(spy.items.isEmpty)
        XCTAssertTrue(spy.finished)

        let changes = ChangeSpy()
        enumerator.enumerateChanges(for: changes, from: FileProviderEnumerator.encode(7))
        XCTAssertTrue(changes.updated.isEmpty)
        XCTAssertTrue(changes.finished)
        XCTAssertEqual(changes.anchor.flatMap(FileProviderEnumerator.decode), 7)
    }

    // OBS-103 §3.7: children come in pages of `pageSize`.
    func testChildrenArePaged() throws {
        let store = try makeStore()
        let total = FileProviderEnumerator.pageSize * 2 + 200
        for i in 0..<total {
            try store.insert(record(String(format: "F%05d", i), parent: "", path: String(format: "f%05d.md", i)))
        }

        var page = NSFileProviderPage(NSFileProviderPage.initialPageSortedByDate as Data)
        var seen: [String] = []
        var pages = 0
        while true {
            let spy = EnumerationSpy()
            FileProviderEnumerator(container: .rootContainer, store: store).enumerateItems(for: spy, startingAt: page)
            XCTAssertTrue(spy.finished)
            seen += spy.items.map(\.filename)
            pages += 1
            guard let next = spy.nextPage else { break }
            page = next
        }
        XCTAssertEqual(pages, 3)
        XCTAssertEqual(seen.count, total)
        XCTAssertEqual(seen, seen.sorted())
        XCTAssertEqual(Set(seen).count, total, "no row is repeated across pages")
    }

    // OBS-103 §3.7: a large delta is handed over with `moreComing` and an
    // intermediate anchor; the final anchor is the last row delivered.
    func testChangesArePaged() throws {
        let store = try makeStore()
        let total = FileProviderEnumerator.pageSize + 10
        for i in 0..<total {
            try store.insert(record("R\(i)", parent: "", path: "r\(i).md"))
        }

        let enumerator = FileProviderEnumerator(container: .rootContainer, store: store)
        let first = ChangeSpy()
        enumerator.enumerateChanges(for: first, from: FileProviderEnumerator.encode(0))
        XCTAssertEqual(first.updated.count, FileProviderEnumerator.pageSize)
        XCTAssertEqual(first.moreComing, true)
        XCTAssertEqual(first.anchor.flatMap(FileProviderEnumerator.decode), Int64(FileProviderEnumerator.pageSize))

        let second = ChangeSpy()
        enumerator.enumerateChanges(for: second, from: first.anchor!)
        XCTAssertEqual(second.updated.count, 10)
        XCTAssertEqual(second.moreComing, false)
        XCTAssertEqual(second.anchor.flatMap(FileProviderEnumerator.decode), Int64(total))
        XCTAssertEqual(Set(first.updated.map(\.itemIdentifier.rawValue)).intersection(second.updated.map(\.itemIdentifier.rawValue)), [])
    }

    func testPageCodecRoundTripsAndTreatsSystemPagesAsStart() {
        XCTAssertEqual(FileProviderEnumerator.decodePage(FileProviderEnumerator.encodePage(1234)), 1234)
        XCTAssertEqual(FileProviderEnumerator.decodePage(NSFileProviderPage(NSFileProviderPage.initialPageSortedByDate as Data)), 0)
        XCTAssertEqual(FileProviderEnumerator.decodePage(NSFileProviderPage(NSFileProviderPage.initialPageSortedByName as Data)), 0)
        XCTAssertEqual(FileProviderEnumerator.decodePage(NSFileProviderPage(Data([1, 2]))), 0)
    }
}

final class ChangeSpy: NSObject, NSFileProviderChangeObserver {
    var updated: [NSFileProviderItem] = []
    var deleted: [NSFileProviderItemIdentifier] = []
    var finished = false
    var moreComing: Bool?
    var anchor: NSFileProviderSyncAnchor?

    func didUpdate(_ updatedItems: [NSFileProviderItem]) {
        updated.append(contentsOf: updatedItems)
    }

    func didDeleteItems(withIdentifiers deletedItemIdentifiers: [NSFileProviderItemIdentifier]) {
        deleted.append(contentsOf: deletedItemIdentifiers)
    }

    func finishEnumeratingChanges(upTo anchor: NSFileProviderSyncAnchor, moreComing: Bool) {
        finished = true
        self.anchor = anchor
        self.moreComing = moreComing
    }

    func finishEnumeratingWithError(_ error: Error) {}
}

final class EnumerationSpy: NSObject, NSFileProviderEnumerationObserver {
    var items: [NSFileProviderItem] = []
    var finished = false
    var nextPage: NSFileProviderPage?
    var error: Error?

    func didEnumerate(_ updatedItems: [NSFileProviderItem]) {
        items.append(contentsOf: updatedItems)
    }

    func finishEnumerating(upTo nextPage: NSFileProviderPage?) {
        finished = true
        self.nextPage = nextPage
    }

    func finishEnumeratingWithError(_ error: Error) {
        self.error = error
    }
}
