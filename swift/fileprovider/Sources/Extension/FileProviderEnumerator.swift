import FileProvider
import Foundation
import os.log

private let enumLogger = Logger(subsystem: "io.tinyland.tcfs.fileprovider", category: "enumerator")

/// Enumerates TCFS directory contents by calling into the Rust FFI layer.
///
/// Items are returned as placeholders (`isDownloaded = false`) so that
/// macOS shows them in Finder without downloading content. Content is
/// fetched on demand via `fetchContents` when the user opens a file.
class TCFSFileProviderEnumerator: NSObject, NSFileProviderEnumerator {

    private let providerAccessor: () -> OpaquePointer?
    private let containerIdentifier: NSFileProviderItemIdentifier

    init(
        providerAccessor: @escaping () -> OpaquePointer?,
        containerIdentifier: NSFileProviderItemIdentifier
    ) {
        self.providerAccessor = providerAccessor
        self.containerIdentifier = containerIdentifier
        super.init()
    }

    func invalidate() {
        // No cleanup needed — provider lifetime managed by extension
    }

    func enumerateItems(
        for observer: NSFileProviderEnumerationObserver,
        startingAt page: NSFileProviderPage
    ) {
        let containerId = containerIdentifier
        let accessor = providerAccessor

        // Dispatch off the file-coordination thread to avoid EDEADLK when the
        // lazy provider init (tokio runtime + S3 operator) blocks.
        DispatchQueue.global(qos: .userInitiated).async {
            enumLogger.info("enumerateItems: resolving provider for container \(containerId.rawValue)")
            guard let prov = accessor() else {
                enumLogger.error("enumerateItems: provider is nil — returning serverUnreachable")
                observer.finishEnumeratingWithError(NSFileProviderError(.serverUnreachable))
                return
            }

            let path: String
            if containerId == .rootContainer {
                path = ""
            } else {
                path = containerId.rawValue
            }

            enumLogger.info("enumerateItems: calling tcfs_provider_enumerate for path='\(path)'")

            var outItems: UnsafeMutablePointer<TcfsFileItem>?
            var outCount: UInt = 0

            let result = path.withCString { pathPtr in
                tcfs_provider_enumerate(prov, pathPtr, &outItems, &outCount)
            }

            enumLogger.info("enumerateItems: enumerate returned \(result.rawValue), count=\(outCount)")

            guard result == TCFS_ERROR_TCFS_ERROR_NONE, let items = outItems, outCount > 0 else {
                if result != TCFS_ERROR_TCFS_ERROR_NONE {
                    enumLogger.error("enumerateItems: enumerate failed with code \(result.rawValue)")
                }
                observer.finishEnumerating(upTo: nil)
                return
            }

            var providerItems: [NSFileProviderItem] = []
            let count = Int(outCount)

            for i in 0..<count {
                let item = items[i]

                let itemId = item.item_id.map { String(cString: $0) } ?? ""
                let filename = item.filename.map { String(cString: $0) } ?? ""

                // Items are created as placeholders (downloaded: false).
                // Content will be fetched on demand via fetchContents.
                providerItems.append(
                    TCFSFileProviderItem(
                        identifier: NSFileProviderItemIdentifier(itemId),
                        parentIdentifier: containerId,
                        filename: filename,
                        isDirectory: item.is_directory,
                        fileSize: item.file_size,
                        downloaded: false,
                        uploaded: true
                    )
                )
            }

            // Free the C array
            tcfs_file_items_free(outItems, outCount)

            enumLogger.info("enumerateItems: returning \(providerItems.count) placeholder items")
            observer.didEnumerate(providerItems)
            observer.finishEnumerating(upTo: nil)
        }
    }

    func enumerateChanges(
        for observer: NSFileProviderChangeObserver,
        from anchor: NSFileProviderSyncAnchor
    ) {
        // TODO: Wire to daemon Watch gRPC stream for incremental changes.
        // For now, signal no changes — fileproviderd will re-enumerate periodically.
        observer.finishEnumeratingChanges(upTo: anchor, moreComing: false)
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        // Use timestamp as anchor — will be replaced with daemon event sequence
        let data = "\(Date().timeIntervalSince1970)".data(using: .utf8)!
        completionHandler(NSFileProviderSyncAnchor(data))
    }
}
