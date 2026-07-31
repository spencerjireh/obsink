import FileProvider
import UniformTypeIdentifiers

/// An item in the replicated File Provider, backed by an `ItemRecord`. The
/// identifier is the record's stable UUID (spec §11.6) — never the file path.
final class FileProviderItem: NSObject, NSFileProviderItem {
    private let record: ItemRecord?
    private let isRoot: Bool

    private init(record: ItemRecord?, isRoot: Bool) {
        self.record = record
        self.isRoot = isRoot
        super.init()
    }

    init(record: ItemRecord) {
        self.record = record
        self.isRoot = false
        super.init()
    }

    /// Synthesized root container item.
    static func root() -> FileProviderItem {
        FileProviderItem(record: nil, isRoot: true)
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

    var filename: String { isRoot ? "ObSink" : (record?.filename ?? "") }

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
        return rec.isDirectory
            ? [.allowsAddingSubItems, .allowsContentEnumerating, .allowsReading]
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

    // Replicated extensions require an item version. rowVersion changes on every
    // real mutation, so it doubles as both content and metadata version.
    var itemVersion: NSFileProviderItemVersion {
        var be = (record?.rowVersion ?? 0).bigEndian
        let data = withUnsafeBytes(of: &be) { Data($0) }
        return NSFileProviderItemVersion(contentVersion: data, metadataVersion: data)
    }
}
