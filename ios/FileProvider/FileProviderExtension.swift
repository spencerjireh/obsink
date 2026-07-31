import FileProvider
import Foundation

/// Replicated File Provider backed by the shared `ItemStore` (spec §11). The
/// ObSink app performs the encrypted sync via the Rust core, writes plaintext to
/// the shared `Vault/` dir, and reconciles the item DB; this extension exposes
/// those items to Obsidian and the Files app. The extension never touches the
/// network (spec §11.4) — it reads the DB + on-disk cache only.
///
/// Identifiers are stable UUIDs assigned by `ItemStore`. Local writes
/// (`createItem`/`modifyItem`/`deleteItem`) update the cache and mark
/// `pendingUpload`/`pendingDeletion` so the host app's next sync drains them.
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
            completionHandler(FileProviderItem.root(), nil)
            return Progress()
        }
        if let rec = try? ItemStore.shared.item(for: identifier.rawValue) {
            completionHandler(FileProviderItem(record: rec), nil)
        } else {
            completionHandler(nil, NSFileProviderError(.noSuchItem))
        }
        return Progress()
    }

    func fetchContents(
        for itemIdentifier: NSFileProviderItemIdentifier,
        version _: NSFileProviderItemVersion?,
        request _: NSFileProviderRequest,
        completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        guard
            itemIdentifier != .rootContainer,
            let rec = try? ItemStore.shared.item(for: itemIdentifier.rawValue),
            let url = FileProviderPaths.url(forLocalPath: rec.localPath, root: root),
            FileManager.default.fileExists(atPath: url.path)
        else {
            completionHandler(nil, nil, NSFileProviderError(.noSuchItem))
            return Progress()
        }
        completionHandler(url, FileProviderItem(record: rec), nil)
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
        let parentID = parentIdentifierValue(of: itemTemplate.parentItemIdentifier)
        let parentPath = parentPath(forIdentifier: itemTemplate.parentItemIdentifier)
        let filename = itemTemplate.filename
        let localPath = parentPath.isEmpty ? filename : "\(parentPath)/\(filename)"
        let destination = root.appendingPathComponent(localPath)
        let isFolder = itemTemplate.contentType == .folder
        do {
            if isFolder {
                try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
            } else if let url {
                try? FileManager.default.removeItem(at: destination)
                try FileManager.default.copyItem(at: url, to: destination)
            } else {
                FileManager.default.createFile(atPath: destination.path, contents: nil)
            }

            let size: Int64? = isFolder
                ? nil
                : (try? destination.resourceValues(forKeys: [.fileSizeKey]))?.fileSize.map(Int64.init)
            let rec = ItemRecord(
                identifier: UUID().uuidString,
                parentIdentifier: parentID,
                filename: filename,
                contentHash: nil,
                localPath: localPath,
                isDirectory: isFolder,
                size: size,
                modified: mtime(of: destination),
                pendingUpload: true
            )
            try ItemStore.shared.upsert(rec)
            completionHandler(FileProviderItem(record: rec), [], false, nil)
        } catch {
            completionHandler(nil, [], false, error)
        }
        return Progress()
    }

    func modifyItem(
        _ item: NSFileProviderItem,
        baseVersion _: NSFileProviderItemVersion,
        changedFields: NSFileProviderItemFields,
        contents newContents: URL?,
        options _: NSFileProviderModifyItemOptions = [],
        request _: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        let id = item.itemIdentifier
        guard
            id != .rootContainer,
            let rec = try? ItemStore.shared.item(for: id.rawValue),
            let url = FileProviderPaths.url(forLocalPath: rec.localPath, root: root)
        else {
            completionHandler(nil, [], false, NSFileProviderError(.noSuchItem))
            return Progress()
        }
        do {
            if let newContents {
                try? FileManager.default.removeItem(at: url)
                try FileManager.default.copyItem(at: newContents, to: url)
            }
            var current = rec
            if changedFields.contains(.filename) || changedFields.contains(.parentItemIdentifier) {
                let newFilename = changedFields.contains(.filename) ? item.filename : current.filename
                let newParentID = changedFields.contains(.parentItemIdentifier)
                    ? parentIdentifierValue(of: item.parentItemIdentifier)
                    : current.parentIdentifier
                let newParentPath = changedFields.contains(.parentItemIdentifier)
                    ? parentPath(forIdentifier: item.parentItemIdentifier)
                    : parentPath(forIdentifier: NSFileProviderItemIdentifier(current.parentIdentifier))
                let newLocalPath = newParentPath.isEmpty ? newFilename : "\(newParentPath)/\(newFilename)"
                if newLocalPath != current.localPath {
                    let destination = root.appendingPathComponent(newLocalPath)
                    try? FileManager.default.createDirectory(at: destination.deletingLastPathComponent(), withIntermediateDirectories: true)
                    try? FileManager.default.moveItem(at: url, to: destination)
                    if let moved = try? ItemStore.shared.rename(
                        identifier: current.identifier,
                        toPath: newLocalPath,
                        filename: newFilename,
                        parentIdentifier: newParentID
                    ) {
                        current = moved
                    }
                }
            }
            try? ItemStore.shared.setPending(identifier: current.identifier, upload: true)
            completionHandler(FileProviderItem(record: current), [], false, nil)
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
        guard
            identifier != .rootContainer,
            let rec = try? ItemStore.shared.item(for: identifier.rawValue),
            let url = FileProviderPaths.url(forLocalPath: rec.localPath, root: root)
        else {
            completionHandler(NSFileProviderError(.noSuchItem))
            return Progress()
        }
        try? FileManager.default.removeItem(at: url)
        try? ItemStore.shared.setPending(identifier: rec.identifier, deletion: true)
        completionHandler(nil)
        return Progress()
    }

    func enumerator(
        for containerItemIdentifier: NSFileProviderItemIdentifier,
        request _: NSFileProviderRequest
    ) throws -> NSFileProviderEnumerator {
        FileProviderEnumerator(container: containerItemIdentifier)
    }

    // MARK: Helpers

    /// The DB parentIdentifier value for an FP parent id ("" for the root container).
    private func parentIdentifierValue(of parent: NSFileProviderItemIdentifier) -> String {
        parent == .rootContainer ? "" : parent.rawValue
    }

    /// The on-disk relative path of an item's parent, looked up from the DB.
    private func parentPath(forIdentifier parent: NSFileProviderItemIdentifier) -> String {
        guard parent != .rootContainer, let rec = try? ItemStore.shared.item(for: parent.rawValue) else {
            return ""
        }
        return rec.localPath
    }

    private func mtime(of url: URL) -> Int64 {
        let date = (try? url.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate
        return Int64((date ?? Date()).timeIntervalSince1970)
    }
}

/// On-disk layout for the shared vault cache. Identifiers live in the DB; this is
/// only the path↔URL mapping.
enum FileProviderPaths {
    static let appGroup = "group.com.obsink.shared"

    static var vaultRoot: URL {
        let base = FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: appGroup)
            ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        let dir = base.appendingPathComponent("Vault", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    /// Absolute URL for a vault-relative path, rejecting anything that escapes the root.
    static func url(forLocalPath localPath: String, root: URL) -> URL? {
        let url = root.appendingPathComponent(localPath)
        guard url.path == root.path || url.path.hasPrefix(root.path + "/") else { return nil }
        return url
    }
}
