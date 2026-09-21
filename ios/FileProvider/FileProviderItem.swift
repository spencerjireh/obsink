import FileProvider
import UniformTypeIdentifiers

/// An item in the replicated File Provider, backed by an `ItemRecord`. The
/// identifier is the record's stable UUID (spec §11.6) — never the file path.
final class FileProviderItem: NSObject, NSFileProviderItem {
    private let record: ItemRecord?
    private let isRoot: Bool
    private let rootName: String

    private init(record: ItemRecord?, isRoot: Bool, rootName: String) {
        self.record = record
        self.isRoot = isRoot
        self.rootName = rootName
        super.init()
    }

    init(record: ItemRecord) {
        self.record = record
        self.isRoot = false
        self.rootName = ""
        super.init()
    }

    /// Synthesized root container item, shown under the domain's name (the
    /// vault name).
    static func root(named name: String = "ObSink") -> FileProviderItem {
        FileProviderItem(record: nil, isRoot: true, rootName: name)
    }

    var itemIdentifier: NSFileProviderItemIdentifier {
        isRoot ? .rootContainer : NSFileProviderItemIdentifier(record!.identifier)
    }

    var parentItemIdentifier: NSFileProviderItemIdentifier {
        guard !isRoot, let parent = record?.parentIdentifier, !parent.isEmpty else {
            return .rootContainer
        }
        return NSFileProviderItemIdentifier(parent)
    }

    var filename: String { isRoot ? rootName : (record?.filename ?? "") }

    var contentType: UTType {
        guard !isRoot, let rec = record else { return .folder }
        return rec.isDirectory
            ? .folder
            : (UTType(filenameExtension: (rec.filename as NSString).pathExtension) ?? .data)
    }

    var capabilities: NSFileProviderItemCapabilities {
        guard !isRoot, let rec = record else {
            return [.allowsAddingSubItems, .allowsContentEnumerating, .allowsReading]
        }
        // Folders can be reorganised too: a notes vault gets restructured
        // often, and the store cascades a folder's rename or delete to its
        // descendants.
        return rec.isDirectory
            ? [.allowsAddingSubItems, .allowsContentEnumerating, .allowsReading,
               .allowsDeleting, .allowsRenaming, .allowsReparenting]
            : [.allowsReading, .allowsWriting, .allowsDeleting, .allowsReparenting, .allowsRenaming]
    }

    var documentSize: NSNumber? {
        guard !isRoot, let rec = record, let size = rec.size else { return nil }
        return NSNumber(value: size)
    }

    var contentModificationDate: Date? {
        guard !isRoot, let rec = record else { return nil }
        return Date(timeIntervalSince1970: TimeInterval(rec.modified))
    }

    // Replicated extensions require an item version. The content version is
    // the bytes' (size, mtime) so a rename or a pending flag (which bump
    // rowVersion) does not make the system re-fetch unchanged contents; the
    // metadata version is rowVersion, which changes on every real mutation.
    // The extension has no vault keys, so the content HMAC is not available
    // here; `contentHash` stays reserved.
    var itemVersion: NSFileProviderItemVersion {
        NSFileProviderItemVersion(contentVersion: contentVersionData, metadataVersion: metadataVersionData)
    }

    var contentVersionData: Data {
        guard !isRoot, let rec = record, !rec.isDirectory else { return Data(count: 16) }
        var size = (rec.size ?? -1).bigEndian
        var modified = rec.modified.bigEndian
        var data = withUnsafeBytes(of: &size) { Data($0) }
        data.append(withUnsafeBytes(of: &modified) { Data($0) })
        return data
    }

    var metadataVersionData: Data {
        var be = (record?.rowVersion ?? 0).bigEndian
        return withUnsafeBytes(of: &be) { Data($0) }
    }
}
