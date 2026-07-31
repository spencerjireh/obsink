import Foundation
import GRDB

/// One row in the shared File Provider item database (spec §11.5).
///
/// `identifier` is a stable UUID assigned on first encounter (spec §11.6) — never
/// the file path. The File Provider extension and the host app read/write this
/// same table via the App Group container.
struct ItemRecord: Codable, FetchableRecord, MutablePersistableRecord {
    static let databaseTableName = "items"

    var identifier: String
    var parentIdentifier: String   // "" for items whose parent is the root container
    var filename: String
    var contentHash: String?
    var localPath: String          // vault-relative, e.g. "notes/a.md"
    var isDirectory: Bool
    var size: Int64?
    var modified: Int64            // seconds since epoch
    var pendingUpload: Bool
    var pendingDeletion: Bool
    var rowVersion: Int64          // monotonic; bumped on every real change for enumerateChanges

    init(identifier: String,
         parentIdentifier: String,
         filename: String,
         contentHash: String?,
         localPath: String,
         isDirectory: Bool,
         size: Int64?,
         modified: Int64,
         pendingUpload: Bool = false,
         pendingDeletion: Bool = false,
         rowVersion: Int64 = 0) {
        self.identifier = identifier
        self.parentIdentifier = parentIdentifier
        self.filename = filename
        self.contentHash = contentHash
        self.localPath = localPath
        self.isDirectory = isDirectory
        self.size = size
        self.modified = modified
        self.pendingUpload = pendingUpload
        self.pendingDeletion = pendingDeletion
        self.rowVersion = rowVersion
    }
}
