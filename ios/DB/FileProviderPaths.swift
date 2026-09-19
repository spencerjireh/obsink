import Foundation

/// On-disk layout for the shared vault cache. Identifiers live in the DB; this is
/// only the path↔URL mapping.
enum FileProviderPaths {
    static let appGroup = "group.com.obsink.shared"

    /// `Vault/` under the App Group container: one subdirectory per vault.
    static var vaultsBase: URL {
        let base = FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: appGroup)
            ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        return base.appendingPathComponent("Vault", isDirectory: true)
    }

    /// The cache directory of one vault, created on first use.
    static func vaultRoot(vaultID: String) -> URL {
        let dir = vaultsBase.appendingPathComponent(vaultID, isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    /// Copy `source` to a uniquely named file under `staging` and return it,
    /// leaving `source` in place. The File Provider consumes the returned
    /// file, so the vault copy must never be handed over directly.
    static func stagedCopy(of source: URL, in staging: URL) throws -> URL {
        try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
        let copy = staging.appendingPathComponent(UUID().uuidString)
        try FileManager.default.copyItem(at: source, to: copy)
        return copy
    }

    /// Absolute URL for a vault-relative path, rejecting anything that escapes the root.
    static func url(forLocalPath localPath: String, root: URL) -> URL? {
        let url = root.appendingPathComponent(localPath)
        guard url.path == root.path || url.path.hasPrefix(root.path + "/") else { return nil }
        return url
    }
}
