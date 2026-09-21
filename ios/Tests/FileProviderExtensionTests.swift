import XCTest
import FileProvider
import UniformTypeIdentifiers

/// OBS-103 §3.6/§3.8: the extension answers `cannotSynchronize` when its
/// store could not be opened, and its mutations write rows the enumerator
/// can report.
final class FileProviderExtensionTests: XCTestCase {

    private struct OpenFailed: Error {}

    private let domain = NSFileProviderDomain(
        identifier: NSFileProviderDomainIdentifier("vault_test"), displayName: "Test vault"
    )

    private func makeRoot() throws -> URL {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("obsink-fpext-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        return root
    }

    private func makeStore() throws -> ItemStore {
        let dbURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("obsink-fpext-\(UUID().uuidString).sqlite")
        return try ItemStore(databaseURL: dbURL)
    }

    private func isCannotSynchronize(_ error: Error?) -> Bool {
        (error as? NSFileProviderError)?.code == .cannotSynchronize
    }

    private final class Template: NSObject, NSFileProviderItem {
        let itemIdentifier: NSFileProviderItemIdentifier
        let parentItemIdentifier: NSFileProviderItemIdentifier
        let filename: String
        let contentType: UTType
        init(parent: NSFileProviderItemIdentifier, filename: String, contentType: UTType) {
            self.itemIdentifier = NSFileProviderItemIdentifier(UUID().uuidString)
            self.parentItemIdentifier = parent
            self.filename = filename
            self.contentType = contentType
        }
    }

    func testEveryRequestReportsCannotSynchronizeWhenTheStoreIsUnavailable() throws {
        let ext = FileProviderExtension(domain: domain, store: .failure(OpenFailed()), root: try makeRoot())
        let request = NSFileProviderRequest()
        let id = NSFileProviderItemIdentifier("missing")

        var errors: [Error?] = []
        _ = ext.item(for: id, request: request) { _, error in errors.append(error) }
        _ = ext.fetchContents(for: id, version: nil, request: request) { _, _, error in errors.append(error) }
        _ = ext.createItem(
            basedOn: Template(parent: .rootContainer, filename: "a.md", contentType: .plainText),
            fields: [], contents: nil, options: [], request: request
        ) { _, _, _, error in errors.append(error) }
        _ = ext.deleteItem(identifier: id, baseVersion: NSFileProviderItemVersion(), options: [], request: request) {
            errors.append($0)
        }
        XCTAssertEqual(errors.count, 4)
        XCTAssertTrue(errors.allSatisfy(isCannotSynchronize))
        XCTAssertThrowsError(try ext.enumerator(for: .rootContainer, request: request)) {
            XCTAssertTrue(self.isCannotSynchronize($0))
        }

        // The root container needs no store.
        var rootItem: NSFileProviderItem?
        _ = ext.item(for: .rootContainer, request: request) { item, _ in rootItem = item }
        XCTAssertEqual(rootItem?.filename, "Test vault")
    }

    func testCreateItemAssignsARowVersionTheEnumeratorReports() throws {
        let root = try makeRoot()
        let store = try makeStore()
        let ext = FileProviderExtension(domain: domain, store: .success(store), root: root)
        let request = NSFileProviderRequest()

        let source = FileManager.default.temporaryDirectory.appendingPathComponent("obsink-src-\(UUID().uuidString).md")
        try Data("hello".utf8).write(to: source)

        var created: NSFileProviderItem?
        _ = ext.createItem(
            basedOn: Template(parent: .rootContainer, filename: "hello.md", contentType: .plainText),
            fields: [.contents, .filename], contents: source, options: [], request: request
        ) { item, _, _, error in
            XCTAssertNil(error)
            created = item
        }
        let id = try XCTUnwrap(created?.itemIdentifier.rawValue)
        XCTAssertEqual(try store.item(for: id)?.rowVersion, 1)
        XCTAssertEqual(try store.item(for: id)?.pendingUpload, true)
        XCTAssertEqual(try store.item(for: id)?.size, 5)
        XCTAssertTrue(FileManager.default.fileExists(atPath: root.appendingPathComponent("hello.md").path))

        let spy = ChangeSpy()
        let enumerator = try XCTUnwrap(
            try ext.enumerator(for: .rootContainer, request: request) as? FileProviderEnumerator
        )
        enumerator.enumerateChanges(for: spy, from: FileProviderEnumerator.encode(0))
        XCTAssertEqual(spy.updated.map(\.itemIdentifier.rawValue), [id])
    }

    func testDeletingAFolderRemovesItAndTombstonesItsChildren() throws {
        let root = try makeRoot()
        let store = try makeStore()
        let ext = FileProviderExtension(domain: domain, store: .success(store), root: root)
        let request = NSFileProviderRequest()

        try FileManager.default.createDirectory(at: root.appendingPathComponent("notes"), withIntermediateDirectories: true)
        try Data("x".utf8).write(to: root.appendingPathComponent("notes/a.md"))
        try store.reconcile(vaultRoot: root)
        let folder = try XCTUnwrap(try store.item(path: "notes"))
        let child = try XCTUnwrap(try store.item(path: "notes/a.md"))

        var result: Error?
        _ = ext.deleteItem(identifier: NSFileProviderItemIdentifier(folder.identifier),
                           baseVersion: NSFileProviderItemVersion(), options: [.recursive], request: request) {
            result = $0
        }
        XCTAssertNil(result)
        XCTAssertFalse(FileManager.default.fileExists(atPath: root.appendingPathComponent("notes").path))
        XCTAssertNil(try store.item(for: folder.identifier))
        XCTAssertNil(try store.item(for: child.identifier))
        XCTAssertEqual(try store.pendingCount(), 2)
    }
}
