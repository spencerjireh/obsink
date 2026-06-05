import FileProvider
import Foundation

/// Replicated File Provider backed by the vault directory in the shared App
/// Group container. The ObSink app performs the actual encrypted sync (via the
/// Rust core) and writes plaintext files here; this extension exposes them to
/// Obsidian and the Files app.
///
/// This is a disk-backed scaffold: enumeration, reads, and local writes work
/// against the shared container. Full bidirectional change-tracking with the
/// sync engine is wired through the host app signalling the enumerator.
final class FileProviderExtension: NSObject, NSFileProviderReplicatedExtension {
    private let root: URL

    required init(domain: NSFileProviderDomain) {
        self.root = FileProviderPaths.vaultRoot
        super.init()
    }

    func invalidate() {}

    func item(
        for identifier: NSFileProviderItemIdentifier,
        request _: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        if identifier == .rootContainer {
            completionHandler(FileProviderItem(identifier: .rootContainer, name: "ObSink", isFolder: true, size: nil, contentVersion: FileProviderPaths.anchorData()), nil)
            return Progress()
        }
        let url = FileProviderPaths.url(for: identifier, root: root)
        guard FileManager.default.fileExists(atPath: url.path) else {
            completionHandler(nil, NSFileProviderError(.noSuchItem))
            return Progress()
        }
        completionHandler(FileProviderPaths.item(at: url, root: root), nil)
        return Progress()
    }

    func fetchContents(
        for itemIdentifier: NSFileProviderItemIdentifier,
        version _: NSFileProviderItemVersion?,
        request _: NSFileProviderRequest,
        completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        let url = FileProviderPaths.url(for: itemIdentifier, root: root)
        guard FileManager.default.fileExists(atPath: url.path) else {
            completionHandler(nil, nil, NSFileProviderError(.noSuchItem))
            return Progress()
        }
        completionHandler(url, FileProviderPaths.item(at: url, root: root), nil)
        return Progress()
    }

    func createItem(
        basedOn itemTemplate: NSFileProviderItem,
        fields _: NSFileProviderItemFields,
        contents url: URL?,
        options _: NSFileProviderCreateItemOptions = [],
        request _: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        let parent = FileProviderPaths.url(for: itemTemplate.parentItemIdentifier, root: root)
        let destination = parent.appendingPathComponent(itemTemplate.filename)
        do {
            let isFolder = itemTemplate.contentType == .folder
            if isFolder {
                try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
            } else if let url {
                try? FileManager.default.removeItem(at: destination)
                try FileManager.default.copyItem(at: url, to: destination)
            } else {
                FileManager.default.createFile(atPath: destination.path, contents: nil)
            }
            completionHandler(FileProviderPaths.item(at: destination, root: root), [], false, nil)
        } catch {
            completionHandler(nil, [], false, error)
        }
        return Progress()
    }

    func modifyItem(
        _ item: NSFileProviderItem,
        baseVersion _: NSFileProviderItemVersion,
        changedFields _: NSFileProviderItemFields,
        contents newContents: URL?,
        options _: NSFileProviderModifyItemOptions = [],
        request _: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        let url = FileProviderPaths.url(for: item.itemIdentifier, root: root)
        do {
            if let newContents {
                try? FileManager.default.removeItem(at: url)
                try FileManager.default.copyItem(at: newContents, to: url)
            }
            completionHandler(FileProviderPaths.item(at: url, root: root), [], false, nil)
        } catch {
            completionHandler(nil, [], false, error)
        }
        return Progress()
    }

    func deleteItem(
        identifier: NSFileProviderItemIdentifier,
        baseVersion _: NSFileProviderItemVersion,
        options _: NSFileProviderDeleteItemOptions = [],
        request _: NSFileProviderRequest,
        completionHandler: @escaping (Error?) -> Void
    ) -> Progress {
        let url = FileProviderPaths.url(for: identifier, root: root)
        do {
            try FileManager.default.removeItem(at: url)
            completionHandler(nil)
        } catch {
            completionHandler(error)
        }
        return Progress()
    }

    func enumerator(
        for containerItemIdentifier: NSFileProviderItemIdentifier,
        request _: NSFileProviderRequest
    ) throws -> NSFileProviderEnumerator {
        FileProviderEnumerator(container: containerItemIdentifier, root: root)
    }
}

/// Maps File Provider item identifiers to URLs in the shared container and back.
enum FileProviderPaths {
    static let appGroup = "group.com.obsink.shared"

    static var vaultRoot: URL {
        let base = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroup)
            ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        let dir = base.appendingPathComponent("Vault", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    static func url(for identifier: NSFileProviderItemIdentifier, root: URL) -> URL {
        identifier == .rootContainer ? root : root.appendingPathComponent(identifier.rawValue)
    }

    static func identifier(for url: URL, root: URL) -> NSFileProviderItemIdentifier {
        let relative = url.path.replacingOccurrences(of: root.path + "/", with: "")
        return relative.isEmpty || url.path == root.path ? .rootContainer : NSFileProviderItemIdentifier(relative)
    }

    static func item(at url: URL, root: URL) -> FileProviderItem {
        let values = try? url.resourceValues(forKeys: [.isDirectoryKey, .fileSizeKey, .contentModificationDateKey])
        let isFolder = values?.isDirectory ?? false
        let size = values?.fileSize.map { NSNumber(value: $0) }
        let version = "\(values?.contentModificationDate?.timeIntervalSince1970 ?? 0)".data(using: .utf8) ?? Data()
        return FileProviderItem(
            identifier: identifier(for: url, root: root),
            name: url.lastPathComponent,
            isFolder: isFolder,
            size: isFolder ? nil : size,
            contentVersion: version
        )
    }

    static func anchorData() -> Data {
        let mtime = (try? vaultRoot.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate
        return "\(mtime?.timeIntervalSince1970 ?? 0)".data(using: .utf8) ?? Data()
    }

    static func currentAnchor() -> NSFileProviderSyncAnchor {
        NSFileProviderSyncAnchor(anchorData())
    }
}
