import FileProvider
import Foundation

/// Replicated File Provider backed by the per-vault `ItemStore` (spec §11).
/// One domain per vault: the domain identifier is the vault ID, which picks
/// the on-disk cache `Vault/<vaultID>/` and the database. The ObSink app
/// performs the encrypted sync via the Rust core, writes plaintext to that
/// dir, and reconciles the item DB; this extension exposes those items to
/// Obsidian and the Files app. The extension never touches the network (spec
/// §11.4) — it reads the DB + on-disk cache only.
///
/// Identifiers are stable UUIDs assigned by `ItemStore`. Local writes
/// (`createItem`/`modifyItem`/`deleteItem`) update the cache and mark
/// `pendingUpload`/`pendingDeletion` so the host app's next sync drains them.
///
/// The store is opened once at init. If that fails (a data-protection-locked
/// container before first unlock, a stale `-wal`), every request answers
/// `cannotSynchronize` until the system re-creates the extension, instead of
/// crashing and leaving the Files listing at LOADING.
final class FileProviderExtension: NSObject, NSFileProviderReplicatedExtension {
    private let domain: NSFileProviderDomain
    private let root: URL
    private let storeResult: Result<ItemStore, Error>

    /// Designated initializer; tests inject a temp store and root.
    init(domain: NSFileProviderDomain, store: Result<ItemStore, Error>, root: URL) {
        self.domain = domain
        self.root = root
        self.storeResult = store
        super.init()
    }

    required convenience init(domain: NSFileProviderDomain) {
        let vaultID = domain.identifier.rawValue
        let store = Result { try ItemStore.store(for: vaultID) }
        self.init(domain: domain, store: store, root: FileProviderPaths.vaultRoot(vaultID: vaultID))
        switch store {
        case .success: NSLog("ObSinkFP: init domain=%@", vaultID)
        case .failure(let error): NSLog("ObSinkFP: init domain=%@ store unavailable: %@", vaultID, "\(error)")
        }
    }

    func invalidate() {}

    /// The store, or `cannotSynchronize` when it could not be opened.
    private func openStore() throws -> ItemStore {
        switch storeResult {
        case .success(let store): return store
        case .failure: throw NSFileProviderError(.cannotSynchronize)
        }
    }

    func item(
        for identifier: NSFileProviderItemIdentifier,
        request _: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        NSLog("ObSinkFP: item(for:) %@", identifier.rawValue)
        if identifier == .rootContainer {
            completionHandler(FileProviderItem.root(named: domain.displayName), nil)
            return Progress()
        }
        do {
            let store = try openStore()
            if let rec = try? store.item(for: identifier.rawValue) {
                completionHandler(FileProviderItem(record: rec), nil)
            } else {
                completionHandler(nil, NSFileProviderError(.noSuchItem))
            }
        } catch {
            completionHandler(nil, error)
        }
        return Progress()
    }

    func fetchContents(
        for itemIdentifier: NSFileProviderItemIdentifier,
        version _: NSFileProviderItemVersion?,
        request _: NSFileProviderRequest,
        completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        guard let store = try? openStore() else {
            completionHandler(nil, nil, NSFileProviderError(.cannotSynchronize))
            return Progress()
        }
        guard
            itemIdentifier != .rootContainer,
            let rec = try? store.item(for: itemIdentifier.rawValue),
            let url = FileProviderPaths.url(forLocalPath: rec.localPath, root: root),
            FileManager.default.fileExists(atPath: url.path)
        else {
            completionHandler(nil, nil, NSFileProviderError(.noSuchItem))
            return Progress()
        }
        // The system takes ownership of the returned file (it is moved into
        // the replicated store), so hand it a copy: returning the live vault
        // file would remove it from `Vault/` and the next sync would delete
        // it on the server.
        do {
            let staging = (try? NSFileProviderManager(for: domain)?.temporaryDirectoryURL())
                ?? FileManager.default.temporaryDirectory
            let copy = try FileProviderPaths.stagedCopy(of: url, in: staging)
            completionHandler(copy, FileProviderItem(record: rec), nil)
        } catch {
            completionHandler(nil, nil, error)
        }
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
        guard let store = try? openStore() else {
            completionHandler(nil, [], false, NSFileProviderError(.cannotSynchronize))
            return Progress()
        }
        let parentID = parentIdentifierValue(of: itemTemplate.parentItemIdentifier)
        let parentPath = parentPath(forIdentifier: itemTemplate.parentItemIdentifier, store: store)
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

            let rec = try store.insert(ItemRecord(
                identifier: UUID().uuidString,
                parentIdentifier: parentID,
                filename: filename,
                contentHash: nil,
                localPath: localPath,
                isDirectory: isFolder,
                size: isFolder ? nil : size(of: destination),
                modified: mtime(of: destination),
                pendingUpload: true
            ))
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
        guard let store = try? openStore() else {
            completionHandler(nil, [], false, NSFileProviderError(.cannotSynchronize))
            return Progress()
        }
        let id = item.itemIdentifier
        guard
            id != .rootContainer,
            let rec = try? store.item(for: id.rawValue),
            let url = FileProviderPaths.url(forLocalPath: rec.localPath, root: root)
        else {
            completionHandler(nil, [], false, NSFileProviderError(.noSuchItem))
            return Progress()
        }
        do {
            var current = rec
            if let newContents {
                try? FileManager.default.removeItem(at: url)
                try FileManager.default.copyItem(at: newContents, to: url)
                // New bytes change the content version (size + mtime); the
                // rename below, if any, changes only the metadata version.
                if let updated = try store.updateContent(
                    identifier: current.identifier, size: size(of: url), modified: mtime(of: url)
                ) {
                    current = updated
                }
            }
            if changedFields.contains(.filename) || changedFields.contains(.parentItemIdentifier) {
                let newFilename = changedFields.contains(.filename) ? item.filename : current.filename
                let newParentID = changedFields.contains(.parentItemIdentifier)
                    ? parentIdentifierValue(of: item.parentItemIdentifier)
                    : current.parentIdentifier
                let newParentPath = changedFields.contains(.parentItemIdentifier)
                    ? parentPath(forIdentifier: item.parentItemIdentifier, store: store)
                    : parentPath(forIdentifier: NSFileProviderItemIdentifier(current.parentIdentifier), store: store)
                let newLocalPath = newParentPath.isEmpty ? newFilename : "\(newParentPath)/\(newFilename)"
                if newLocalPath != current.localPath {
                    let destination = root.appendingPathComponent(newLocalPath)
                    try? FileManager.default.createDirectory(at: destination.deletingLastPathComponent(), withIntermediateDirectories: true)
                    // A failed move is an error, not a phantom rename in the DB.
                    try FileManager.default.moveItem(at: url, to: destination)
                    if let moved = try store.rename(
                        identifier: current.identifier,
                        toPath: newLocalPath,
                        filename: newFilename,
                        parentIdentifier: newParentID
                    ) {
                        current = moved
                    }
                }
            }
            try? store.setPending(identifier: current.identifier, upload: true)
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
        guard let store = try? openStore() else {
            completionHandler(NSFileProviderError(.cannotSynchronize))
            return Progress()
        }
        guard
            identifier != .rootContainer,
            let rec = try? store.item(for: identifier.rawValue),
            let url = FileProviderPaths.url(forLocalPath: rec.localPath, root: root)
        else {
            completionHandler(NSFileProviderError(.noSuchItem))
            return Progress()
        }
        try? FileManager.default.removeItem(at: url)
        // Tombstones the row and, for a folder, everything under it.
        try? store.setPending(identifier: rec.identifier, deletion: true)
        completionHandler(nil)
        return Progress()
    }

    func enumerator(
        for containerItemIdentifier: NSFileProviderItemIdentifier,
        request _: NSFileProviderRequest
    ) throws -> NSFileProviderEnumerator {
        NSLog("ObSinkFP: enumerator(for:) %@", containerItemIdentifier.rawValue)
        return FileProviderEnumerator(container: containerItemIdentifier, store: try openStore())
    }

    // MARK: Helpers

    /// The DB parentIdentifier value for an FP parent id ("" for the root container).
    private func parentIdentifierValue(of parent: NSFileProviderItemIdentifier) -> String {
        parent == .rootContainer ? "" : parent.rawValue
    }

    /// The on-disk relative path of an item's parent, looked up from the DB.
    private func parentPath(forIdentifier parent: NSFileProviderItemIdentifier, store: ItemStore) -> String {
        guard parent != .rootContainer, let rec = try? store.item(for: parent.rawValue) else {
            return ""
        }
        return rec.localPath
    }

    private func size(of url: URL) -> Int64? {
        (try? url.resourceValues(forKeys: [.fileSizeKey]))?.fileSize.map(Int64.init)
    }

    private func mtime(of url: URL) -> Int64 {
        let date = (try? url.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate
        return Int64((date ?? Date()).timeIntervalSince1970)
    }
}
