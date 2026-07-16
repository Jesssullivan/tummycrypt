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
            } else if containerId == .workingSet {
                path = ""
            } else {
                path = containerId.rawValue
            }

            let recursive = containerId == .workingSet
            let parentIdentifier: NSFileProviderItemIdentifier = containerId == .workingSet
                ? .rootContainer : containerId
            let enumeration = Self.enumerateProviderItems(
                provider: prov,
                path: path,
                parentIdentifier: parentIdentifier,
                recursive: recursive
            )

            enumLogger.info(
                "enumerateItems: enumerate returned \(enumeration.result.rawValue), count=\(enumeration.items.count)"
            )

            guard enumeration.result == TCFS_ERROR_TCFS_ERROR_NONE else {
                enumLogger.error("enumerateItems: enumerate failed with code \(enumeration.result.rawValue)")
                observer.finishEnumeratingWithError(NSFileProviderError(.serverUnreachable))
                return
            }

            guard !enumeration.items.isEmpty else {
                observer.finishEnumerating(upTo: nil)
                return
            }

            enumLogger.info("enumerateItems: returning \(enumeration.items.count) placeholder items")
            observer.didEnumerate(enumeration.items)
            observer.finishEnumerating(upTo: nil)
        }
    }

    func enumerateChanges(
        for observer: NSFileProviderChangeObserver,
        from anchor: NSFileProviderSyncAnchor
    ) {
        let accessor = providerAccessor
        let containerId = containerIdentifier

        // Positive cursors are rejected until tcfsd has a complete
        // authoritative change journal. `.syncAnchorExpired` below makes
        // fileproviderd discard its cached baseline and drive a full listing.
        DispatchQueue.global(qos: .userInitiated).async {
            guard let prov = accessor() else {
                observer.finishEnumeratingWithError(NSFileProviderError(.serverUnreachable))
                return
            }

            let path: String
            if containerId == .rootContainer {
                path = ""
            } else if containerId == .workingSet {
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
                enumLogger.warning(
                    "enumerateChanges: incremental authority failed (\(result.rawValue)); preserving anchor and requesting full re-enumerate"
                )
                if result == TCFS_ERROR_TCFS_ERROR_SYNC_ANCHOR_EXPIRED {
                    observer.finishEnumeratingWithError(
                        NSFileProviderError(.syncAnchorExpired)
                    )
                } else {
                    observer.finishEnumeratingWithError(
                        NSFileProviderError(.serverUnreachable)
                    )
                }
                return
            }

            var updatedItems: [NSFileProviderItem] = []
            var deletedIds: [NSFileProviderItemIdentifier] = []
            var updatedIds = Set<String>()
            var maxTimestamp: Int64 = sinceTimestamp

            if let events = outEvents, outCount > 0 {
                let count = Int(outCount)
                for i in 0..<count {
                    let event = events[i]
                    let itemPath = event.path.map { String(cString: $0) } ?? ""
                    let filename = event.filename.map { String(cString: $0) } ?? ""
                    let eventType = event.event_type.map { String(cString: $0) } ?? ""
                    let contentHash = event.content_hash.map { String(cString: $0) } ?? ""
                    let itemIdentifier = Self.normalizedItemIdentifier(
                        itemPath,
                        isDirectory: event.is_directory
                    )

                    if eventType != "deleted" && !event.is_directory && contentHash.isEmpty {
                        enumLogger.error(
                            "enumerateChanges: file update lacks an exact version token"
                        )
                        tcfs_change_events_free(outEvents, outCount)
                        observer.finishEnumeratingWithError(
                            NSFileProviderError(.serverUnreachable)
                        )
                        return
                    }

                    maxTimestamp = max(maxTimestamp, event.timestamp)

                    if eventType == "deleted" {
                        deletedIds.append(NSFileProviderItemIdentifier(itemIdentifier))
                    } else {
                        Self.appendAncestorDirectoryItems(
                            forPath: itemIdentifier,
                            to: &updatedItems,
                            seen: &updatedIds
                        )
                        if updatedIds.insert(itemIdentifier).inserted {
                            updatedItems.append(
                                TCFSFileProviderItem(
                                    identifier: NSFileProviderItemIdentifier(itemIdentifier),
                                    parentIdentifier: TCFSFileProviderExtension.parentIdentifier(forPath: itemIdentifier),
                                    filename: filename,
                                    isDirectory: event.is_directory,
                                    fileSize: event.file_size,
                                    downloaded: false,
                                    uploaded: true,
                                    versionTag: contentHash
                                )
                            )
                        }
                        enumLogger.info(
                            "enumerateChanges: queued update \(itemIdentifier, privacy: .public)"
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
            // A positive timestamp is deliberately expired by tcfsd on the
            // next call until a complete journal exists. No partial cache
            // snapshot is ever promoted to a successful baseline here.
            let newAnchor: NSFileProviderSyncAnchor
            if outCount > 0 {
                newAnchor = Self.makeAnchorFromTimestamp(maxTimestamp)
            } else {
                newAnchor = anchor
            }
            observer.finishEnumeratingChanges(upTo: newAnchor, moreComing: false)
        }
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        // A wall-clock timestamp is not a journal cursor. Advertising one here
        // would make fileproviderd immediately replay it into tcfsd, which must
        // expire every positive cursor until a durable journal exists. Keep the
        // baseline explicitly absent so the system uses authoritative listings.
        completionHandler(nil)
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

    static func enumerateProviderItems(
        provider prov: OpaquePointer,
        path: String,
        parentIdentifier: NSFileProviderItemIdentifier,
        recursive: Bool,
        depth: Int = 0
    ) -> (result: TcfsError, items: [NSFileProviderItem]) {
        if depth > 32 {
            enumLogger.warning("enumerateProviderItems: stopping at recursion depth \(depth)")
            return (TCFS_ERROR_TCFS_ERROR_NONE, [])
        }

        enumLogger.info("enumerateProviderItems: calling tcfs_provider_enumerate for path='\(path)'")

        var outItems: UnsafeMutablePointer<TcfsFileItem>?
        var outCount: UInt = 0

        let result = path.withCString { pathPtr in
            tcfs_provider_enumerate(prov, pathPtr, &outItems, &outCount)
        }

        guard result == TCFS_ERROR_TCFS_ERROR_NONE else {
            tcfs_file_items_free(outItems, outCount)
            return (result, [])
        }

        guard let items = outItems, outCount > 0 else {
            tcfs_file_items_free(outItems, outCount)
            return (result, [])
        }

        var providerItems: [NSFileProviderItem] = []
        let count = Int(outCount)

        for i in 0..<count {
            let item = items[i]

            let itemId = item.item_id.map { String(cString: $0) } ?? ""
            let filename = item.filename.map { String(cString: $0) } ?? ""
            let contentHash = item.content_hash.map { String(cString: $0) } ?? ""
            let hydration = item.hydration_state.map { String(cString: $0) } ?? ""
            let itemIdentifier = normalizedItemIdentifier(itemId, isDirectory: item.is_directory)

            if !item.is_directory && contentHash.isEmpty {
                enumLogger.error(
                    "enumerateProviderItems: file \(itemIdentifier, privacy: .public) lacks an exact version token"
                )
                tcfs_file_items_free(outItems, outCount)
                return (TCFS_ERROR_TCFS_ERROR_STORAGE, [])
            }

            if !hydration.isEmpty {
                enumLogger.info(
                    "enumerateProviderItems: item=\(itemIdentifier, privacy: .public) hydration_state=\(hydration, privacy: .public)"
                )
            }

            providerItems.append(
                TCFSFileProviderItem(
                    identifier: NSFileProviderItemIdentifier(itemIdentifier),
                    parentIdentifier: parentIdentifier,
                    filename: filename,
                    isDirectory: item.is_directory,
                    fileSize: item.file_size,
                    downloaded: false,
                    uploaded: true,
                    versionTag: contentHash,
                    hydrationState: hydration
                )
            )

            if recursive && item.is_directory && !itemId.isEmpty {
                let childEnumeration = enumerateProviderItems(
                    provider: prov,
                    path: itemId,
                    parentIdentifier: NSFileProviderItemIdentifier(itemIdentifier),
                    recursive: true,
                    depth: depth + 1
                )
                if childEnumeration.result != TCFS_ERROR_TCFS_ERROR_NONE {
                    tcfs_file_items_free(outItems, outCount)
                    return childEnumeration
                }
                providerItems.append(contentsOf: childEnumeration.items)
            }
        }

        tcfs_file_items_free(outItems, outCount)
        return (result, providerItems)
    }

    private static func appendAncestorDirectoryItems(
        forPath itemIdentifier: String,
        to items: inout [NSFileProviderItem],
        seen: inout Set<String>
    ) {
        let trimmed = itemIdentifier.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let components = trimmed.split(separator: "/").map(String.init)
        guard components.count > 1 else {
            return
        }

        var current = ""
        for component in components.dropLast() {
            current = current.isEmpty ? "\(component)/" : "\(current)\(component)/"
            guard seen.insert(current).inserted else {
                continue
            }
            items.append(
                TCFSFileProviderItem(
                    identifier: NSFileProviderItemIdentifier(current),
                    parentIdentifier: TCFSFileProviderExtension.parentIdentifier(forPath: current),
                    filename: component,
                    isDirectory: true,
                    fileSize: 0,
                    downloaded: false,
                    uploaded: true,
                    versionTag: "1"
                )
            )
        }
    }

    private static func normalizedItemIdentifier(_ raw: String, isDirectory: Bool) -> String {
        guard isDirectory, !raw.isEmpty, !raw.hasSuffix("/") else {
            return raw
        }
        return "\(raw)/"
    }
}
