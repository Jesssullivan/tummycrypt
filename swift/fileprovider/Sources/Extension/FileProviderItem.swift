import FileProvider
import UniformTypeIdentifiers

/// Decoration identifiers for custom Finder badges.
///
/// These must match declarations in Extension-Info.plist under
/// NSExtension > NSFileProviderDecorations.
enum TCFSDecoration {
    static let conflict = NSFileProviderItemDecorationIdentifier(
        rawValue: "io.tinyland.tcfs.fileprovider.decoration.conflict"
    )
    static let locked = NSFileProviderItemDecorationIdentifier(
        rawValue: "io.tinyland.tcfs.fileprovider.decoration.locked"
    )
    static let pinned = NSFileProviderItemDecorationIdentifier(
        rawValue: "io.tinyland.tcfs.fileprovider.decoration.pinned"
    )
    static let excluded = NSFileProviderItemDecorationIdentifier(
        rawValue: "io.tinyland.tcfs.fileprovider.decoration.excluded"
    )
}

/// NSFileProviderItem implementation for TCFS files.
///
/// Maps between the Rust `TcfsFileItem` C struct and the
/// NSFileProviderItem protocol that macOS/iOS expect.
///
/// Supports placeholder (dataless) files: items with `isDownloaded = false`
/// appear in Finder but content is only fetched on demand via `fetchContents`.
///
/// Conforms to `NSFileProviderItemDecorating` to render custom badges
/// (conflict, locked, pinned, excluded) directly in Finder without
/// needing a separate Finder Sync Extension.
class TCFSFileProviderItem: NSObject, NSFileProviderItem, NSFileProviderItemDecorating {

    let itemIdentifier: NSFileProviderItemIdentifier
    let parentItemIdentifier: NSFileProviderItemIdentifier
    let filename: String
    let contentType: UTType
    let isDirectory: Bool
    let documentSize: NSNumber?
    let itemVersion: NSFileProviderItemVersion

    /// Whether the file content is available locally (false = placeholder).
    var isDownloaded: Bool

    /// Whether the file has been uploaded to remote storage.
    var isUploaded: Bool

    /// Whether the local copy is the latest version from remote.
    var isMostRecentVersionDownloaded: Bool

    /// Hydration state from daemon: "synced", "not_synced", "active", "locked", "conflict".
    var hydrationState: String

    init(
        identifier: NSFileProviderItemIdentifier,
        parentIdentifier: NSFileProviderItemIdentifier,
        filename: String,
        isDirectory: Bool,
        fileSize: UInt64,
        downloaded: Bool = true,
        uploaded: Bool = true,
        versionTag: String = "1",
        hydrationState: String = ""
    ) {
        self.itemIdentifier = identifier
        self.parentItemIdentifier = parentIdentifier
        self.filename = filename
        self.isDirectory = isDirectory
        self.contentType = isDirectory ? .folder : (UTType(filenameExtension: (filename as NSString).pathExtension) ?? .data)
        self.documentSize = isDirectory ? nil : NSNumber(value: fileSize)
        self.itemVersion = NSFileProviderItemVersion(
            contentVersion: versionTag.data(using: .utf8)!,
            metadataVersion: versionTag.data(using: .utf8)!
        )
        self.isDownloaded = isDirectory ? true : downloaded
        self.isUploaded = uploaded
        self.isMostRecentVersionDownloaded = isDirectory ? true : downloaded
        self.hydrationState = hydrationState
    }

    /// Direct TCFS enumeration can currently identify directories but not their
    /// exact child counts. Returning a non-zero hint keeps fileproviderd from
    /// treating lazy remote folders as known-empty before enumeration runs.
    var childItemCount: NSNumber? {
        return isDirectory ? NSNumber(value: 1) : nil
    }

    var capabilities: NSFileProviderItemCapabilities {
        if isDirectory {
            return [.allowsReading, .allowsContentEnumerating, .allowsAddingSubItems, .allowsDeleting, .allowsRenaming]
        }
        return [.allowsReading, .allowsWriting, .allowsDeleting, .allowsRenaming, .allowsReparenting, .allowsEvicting]
    }

    /// Content policy controls eviction (dehydration) for placeholder support.
    /// `.downloadLazilyAndEvictOnRemoteUpdate` means files are only downloaded
    /// when opened and automatically evicted when a newer remote version exists.
    var contentPolicy: NSFileProviderContentPolicy {
        if isDirectory {
            return .inherited
        }
        return .downloadLazilyAndEvictOnRemoteUpdate
    }

    // MARK: - NSFileProviderItemDecorating

    /// Custom badge decorations based on hydration/sync state.
    ///
    /// Replaces the FinderSync extension badges — FileProvider handles
    /// standard sync badges (cloud/checkmark/progress) automatically,
    /// so we only add custom decorations for states that need extra
    /// visual indicators.
    var decorations: [NSFileProviderItemDecorationIdentifier]? {
        switch hydrationState {
        case "conflict":
            return [TCFSDecoration.conflict]
        case "locked":
            return [TCFSDecoration.locked]
        default:
            // "synced", "not_synced", "active" — use FileProvider's
            // built-in badges (isDownloaded, isUploading, etc.)
            return nil
        }
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
