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
                let contentHash = item.content_hash.map { String(cString: $0) } ?? "1"
                let hydration = item.hydration_state.map { String(cString: $0) } ?? ""

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
                        uploaded: true,
                        versionTag: contentHash,
                        hydrationState: hydration
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
        let accessor = providerAccessor
        let containerId = containerIdentifier

        // Incremental enumeration: only fetch items changed since the anchor timestamp.
        // The daemon's Watch RPC returns catch-up events from the state cache,
        // reducing O(N) full re-enumerate to O(K) where K = actual changes.
        DispatchQueue.global(qos: .userInitiated).async {
            guard let prov = accessor() else {
                observer.finishEnumeratingWithError(NSFileProviderError(.serverUnreachable))
                return
            }

            let path: String
            if containerId == .rootContainer {
                path = ""
            } else {
                path = containerId.rawValue
            }

            // Extract timestamp from anchor (milliseconds since epoch → seconds)
            let sinceTimestamp: Int64 = Self.anchorToTimestamp(anchor)

            var outEvents: UnsafeMutablePointer<TcfsChangeEvent>?
            var outCount: UInt = 0

            let result = path.withCString { pathPtr in
                tcfs_provider_enumerate_changes(prov, pathPtr, sinceTimestamp, &outEvents, &outCount)
            }

            guard result == TCFS_ERROR_TCFS_ERROR_NONE else {
                // Fallback: if incremental fails, signal full re-enumerate
                enumLogger.warning("enumerateChanges: incremental failed (\(result.rawValue)), requesting full re-enumerate")
                let newAnchor = Self.makeAnchor()
                observer.finishEnumeratingChanges(upTo: newAnchor, moreComing: false)
                return
            }

            var updatedItems: [NSFileProviderItem] = []
            var deletedIds: [NSFileProviderItemIdentifier] = []
            var maxTimestamp: Int64 = sinceTimestamp

            if let events = outEvents, outCount > 0 {
                let count = Int(outCount)
                for i in 0..<count {
                    let event = events[i]
                    let itemPath = event.path.map { String(cString: $0) } ?? ""
                    let filename = event.filename.map { String(cString: $0) } ?? ""
                    let eventType = event.event_type.map { String(cString: $0) } ?? ""
                    let contentHash = event.content_hash.map { String(cString: $0) } ?? "1"

                    maxTimestamp = max(maxTimestamp, event.timestamp)

                    if eventType == "deleted" {
                        deletedIds.append(NSFileProviderItemIdentifier(itemPath))
                    } else {
                        updatedItems.append(
                            TCFSFileProviderItem(
                                identifier: NSFileProviderItemIdentifier(itemPath),
                                parentIdentifier: containerId,
                                filename: filename,
                                isDirectory: event.is_directory,
                                fileSize: event.file_size,
                                downloaded: false,
                                uploaded: true,
                                versionTag: contentHash
                            )
                        )
                    }
                }
                tcfs_change_events_free(outEvents, outCount)
            }

            enumLogger.info("enumerateChanges: \(updatedItems.count) updated, \(deletedIds.count) deleted (since \(sinceTimestamp))")

            if !updatedItems.isEmpty {
                observer.didUpdate(updatedItems)
            }
            if !deletedIds.isEmpty {
                observer.didDeleteItems(withIdentifiers: deletedIds)
            }
            // Use max event timestamp as anchor to avoid skipping events
            // that arrive between anchor creation and next enumerateChanges
            let newAnchor = outCount > 0
                ? Self.makeAnchorFromTimestamp(maxTimestamp)
                : anchor
            observer.finishEnumeratingChanges(upTo: newAnchor, moreComing: false)
        }
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        completionHandler(Self.makeAnchor())
    }

    /// Sync anchor from current timestamp (milliseconds since epoch).
    private static func makeAnchor() -> NSFileProviderSyncAnchor {
        var timestamp = UInt64(Date().timeIntervalSince1970 * 1000)
        let data = Data(bytes: &timestamp, count: MemoryLayout<UInt64>.size)
        return NSFileProviderSyncAnchor(data)
    }

    /// Sync anchor from a specific Unix timestamp (seconds).
    private static func makeAnchorFromTimestamp(_ seconds: Int64) -> NSFileProviderSyncAnchor {
        var millis = UInt64(seconds) * 1000
        let data = Data(bytes: &millis, count: MemoryLayout<UInt64>.size)
        return NSFileProviderSyncAnchor(data)
    }

    /// Extract Unix timestamp (seconds) from a sync anchor.
    private static func anchorToTimestamp(_ anchor: NSFileProviderSyncAnchor) -> Int64 {
        let data = anchor.rawValue
        guard data.count == MemoryLayout<UInt64>.size else { return 0 }
        let millis = data.withUnsafeBytes { $0.load(as: UInt64.self) }
        return Int64(millis / 1000)
    }
}
