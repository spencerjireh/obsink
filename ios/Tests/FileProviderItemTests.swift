import XCTest
import FileProvider

/// OBS-103 §3.4/§3.5: folder capabilities, and the content/metadata version
/// split so a rename or a pending flag does not force a content re-fetch.
final class FileProviderItemTests: XCTestCase {

    private func record(path: String, directory: Bool = false, size: Int64? = 10,
                        modified: Int64 = 100, rowVersion: Int64 = 1) -> ItemRecord {
        ItemRecord(identifier: "ID", parentIdentifier: "", filename: (path as NSString).lastPathComponent,
                   contentHash: nil, localPath: path, isDirectory: directory,
                   size: directory ? nil : size, modified: modified, rowVersion: rowVersion)
    }

    func testFoldersCanBeDeletedRenamedAndMoved() {
        let caps = FileProviderItem(record: record(path: "notes", directory: true)).capabilities
        XCTAssertTrue(caps.contains(.allowsDeleting))
        XCTAssertTrue(caps.contains(.allowsRenaming))
        XCTAssertTrue(caps.contains(.allowsReparenting))
        XCTAssertTrue(caps.contains(.allowsAddingSubItems))
        XCTAssertTrue(caps.contains(.allowsContentEnumerating))
        XCTAssertFalse(FileProviderItem.root().capabilities.contains(.allowsDeleting))
    }

    func testRenameChangesOnlyTheMetadataVersion() {
        let before = FileProviderItem(record: record(path: "a.md", rowVersion: 1)).itemVersion
        let after = FileProviderItem(record: record(path: "b.md", rowVersion: 2)).itemVersion
        XCTAssertEqual(before.contentVersion, after.contentVersion)
        XCTAssertNotEqual(before.metadataVersion, after.metadataVersion)
    }

    func testNewBytesChangeTheContentVersion() {
        let base = record(path: "a.md", size: 10, modified: 100, rowVersion: 1)
        let bigger = FileProviderItem(record: record(path: "a.md", size: 11, modified: 100, rowVersion: 2)).itemVersion
        let later = FileProviderItem(record: record(path: "a.md", size: 10, modified: 101, rowVersion: 2)).itemVersion
        let original = FileProviderItem(record: base).itemVersion
        XCTAssertNotEqual(original.contentVersion, bigger.contentVersion)
        XCTAssertNotEqual(original.contentVersion, later.contentVersion)
        XCTAssertEqual(original.contentVersion.count, 16)
        XCTAssertEqual(original.metadataVersion.count, 8)
    }

    func testDirectoriesHaveAFixedContentVersion() {
        let one = FileProviderItem(record: record(path: "a", directory: true, modified: 1, rowVersion: 1)).itemVersion
        let two = FileProviderItem(record: record(path: "a", directory: true, modified: 2, rowVersion: 5)).itemVersion
        XCTAssertEqual(one.contentVersion, two.contentVersion)
        XCTAssertEqual(one.contentVersion, Data(count: 16))
        XCTAssertNotEqual(one.metadataVersion, two.metadataVersion)
    }
}
