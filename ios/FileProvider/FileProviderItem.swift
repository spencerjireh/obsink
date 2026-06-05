import FileProvider
import UniformTypeIdentifiers

/// An item in the replicated File Provider. Identifiers are vault-relative paths
/// (the root container maps to the vault directory in the shared App Group).
final class FileProviderItem: NSObject, NSFileProviderItem {
    let id: NSFileProviderItemIdentifier
    let name: String
    let isFolder: Bool
    let byteSize: NSNumber?
    let contentVersion: Data

    init(identifier: NSFileProviderItemIdentifier, name: String, isFolder: Bool, size: NSNumber?, contentVersion: Data) {
        self.id = identifier
        self.name = name
        self.isFolder = isFolder
        self.byteSize = size
        self.contentVersion = contentVersion
    }

    var itemIdentifier: NSFileProviderItemIdentifier { id }

    var parentItemIdentifier: NSFileProviderItemIdentifier {
        guard id != .rootContainer else { return .rootContainer }
        let path = id.rawValue
        guard let slash = path.lastIndex(of: "/") else { return .rootContainer }
        return NSFileProviderItemIdentifier(String(path[..<slash]))
    }

    var filename: String { name }

    var contentType: UTType { isFolder ? .folder : (UTType(filenameExtension: (name as NSString).pathExtension) ?? .data) }

    var capabilities: NSFileProviderItemCapabilities {
        isFolder
            ? [.allowsAddingSubItems, .allowsContentEnumerating, .allowsReading]
            : [.allowsReading, .allowsWriting, .allowsDeleting, .allowsReparenting, .allowsRenaming]
    }

    var documentSize: NSNumber? { byteSize }

    // Replicated extensions require an item version (content + metadata).
    var itemVersion: NSFileProviderItemVersion {
        NSFileProviderItemVersion(contentVersion: contentVersion, metadataVersion: contentVersion)
    }
}
