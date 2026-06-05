import FileProvider

/// Enumerates the children of a container by listing the backing directory in
/// the shared App Group container.
final class FileProviderEnumerator: NSObject, NSFileProviderEnumerator {
    private let container: NSFileProviderItemIdentifier
    private let root: URL

    init(container: NSFileProviderItemIdentifier, root: URL) {
        self.container = container
        self.root = root
    }

    func invalidate() {}

    func enumerateItems(for observer: NSFileProviderEnumerationObserver, startingAt _: NSFileProviderPage) {
        let dir = FileProviderPaths.url(for: container, root: root)
        let entries = (try? FileManager.default.contentsOfDirectory(
            at: dir,
            includingPropertiesForKeys: [.isDirectoryKey, .fileSizeKey, .contentModificationDateKey],
            options: [.skipsHiddenFiles]
        )) ?? []

        let items: [NSFileProviderItem] = entries.map { url in
            FileProviderPaths.item(at: url, root: root)
        }
        observer.didEnumerate(items)
        observer.finishEnumerating(upTo: nil)
    }

    func enumerateChanges(for observer: NSFileProviderChangeObserver, from _: NSFileProviderSyncAnchor) {
        // The host app drives sync and signals the enumerator; report no
        // incremental changes here and advance the anchor.
        observer.finishEnumeratingChanges(upTo: FileProviderPaths.currentAnchor(), moreComing: false)
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        completionHandler(FileProviderPaths.currentAnchor())
    }
}
