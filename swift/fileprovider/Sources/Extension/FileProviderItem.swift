import FileProvider
import UniformTypeIdentifiers

/// NSFileProviderItem implementation for TCFS files.
///
/// Maps between the Rust `TcfsFileItem` C struct and the
/// NSFileProviderItem protocol that macOS/iOS expect.
///
/// Supports placeholder (dataless) files: items with `isDownloaded = false`
/// appear in Finder but content is only fetched on demand via `fetchContents`.
class TCFSFileProviderItem: NSObject, NSFileProviderItem {

    let itemIdentifier: NSFileProviderItemIdentifier
    let parentItemIdentifier: NSFileProviderItemIdentifier
    let filename: String
    let contentType: UTType
    let documentSize: NSNumber?
    let itemVersion: NSFileProviderItemVersion

    /// Whether the file content is available locally (false = placeholder).
    var isDownloaded: Bool

    /// Whether the file has been uploaded to remote storage.
    var isUploaded: Bool

    /// Whether the local copy is the latest version from remote.
    var isMostRecentVersionDownloaded: Bool

    init(
        identifier: NSFileProviderItemIdentifier,
        parentIdentifier: NSFileProviderItemIdentifier,
        filename: String,
        isDirectory: Bool,
        fileSize: UInt64,
        downloaded: Bool = true,
        uploaded: Bool = true,
        versionTag: String = "1"
    ) {
        self.itemIdentifier = identifier
        self.parentItemIdentifier = parentIdentifier
        self.filename = filename
        self.contentType = isDirectory ? .folder : (UTType(filenameExtension: (filename as NSString).pathExtension) ?? .data)
        self.documentSize = isDirectory ? nil : NSNumber(value: fileSize)
        self.itemVersion = NSFileProviderItemVersion(
            contentVersion: versionTag.data(using: .utf8)!,
            metadataVersion: versionTag.data(using: .utf8)!
        )
        self.isDownloaded = isDirectory ? true : downloaded
        self.isUploaded = uploaded
        self.isMostRecentVersionDownloaded = isDirectory ? true : downloaded
    }

    var capabilities: NSFileProviderItemCapabilities {
        if contentType == .folder {
            return [.allowsReading, .allowsContentEnumerating, .allowsAddingSubItems, .allowsDeleting, .allowsRenaming]
        }
        return [.allowsReading, .allowsWriting, .allowsDeleting, .allowsRenaming, .allowsReparenting]
    }

    /// Content policy controls eviction (dehydration) for placeholder support.
    /// `.downloadLazilyAndEvictOnRemoteUpdate` means files are only downloaded
    /// when opened and automatically evicted when a newer remote version exists.
    var contentPolicy: NSFileProviderContentPolicy {
        if contentType == .folder {
            return .inherited
        }
        return .downloadLazilyAndEvictOnRemoteUpdate
    }

    static func rootItem() -> TCFSFileProviderItem {
        return TCFSFileProviderItem(
            identifier: .rootContainer,
            parentIdentifier: .rootContainer,
            filename: "TCFS",
            isDirectory: true,
            fileSize: 0
        )
    }
}
