import FileProvider
import Foundation
import os.log

private let enumLogger = Logger(subsystem: "io.tinyland.tcfs.fileprovider.ios", category: "enumerator")

/// Enumerates TCFS directory contents via UniFFI bindings.
///
/// Uses `TcfsProviderHandle.listItems(path:)` instead of the macOS C FFI.
/// Items are returned as placeholders (`isDownloaded = false`) so that
/// iOS shows them in Files without downloading content.
class TCFSFileProviderEnumerator: NSObject, NSFileProviderEnumerator {

    private let providerAccessor: () -> TcfsProviderHandle?
    private let containerIdentifier: NSFileProviderItemIdentifier

    init(
        providerAccessor: @escaping () -> TcfsProviderHandle?,
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

        DispatchQueue.global(qos: .userInitiated).async {
            enumLogger.info("enumerateItems: container=\(containerId.rawValue)")
            guard let prov = accessor() else {
                enumLogger.error("enumerateItems: provider is nil")
                observer.finishEnumeratingWithError(NSFileProviderError(.serverUnreachable))
                return
            }

            let path = containerId == .rootContainer ? "" : containerId.rawValue

            do {
                let items = try prov.listItems(path: path)
                enumLogger.info("enumerateItems: got \(items.count) items")

                let providerItems: [NSFileProviderItem] = items.map { item in
                    TCFSFileProviderItem(
                        identifier: NSFileProviderItemIdentifier(item.itemId),
                        parentIdentifier: containerId,
                        filename: item.filename,
                        isDirectory: item.isDirectory,
                        fileSize: item.fileSize,
                        modifiedTimestamp: item.modifiedTimestamp,
                        downloaded: false,
                        uploaded: true,
                        versionTag: item.contentHash,
                        conflictWith: item.conflictWith
                    )
                }

                observer.didEnumerate(providerItems)
                observer.finishEnumerating(upTo: nil)
            } catch {
                enumLogger.error("enumerateItems failed: \(error.localizedDescription)")
                observer.finishEnumerating(upTo: nil)
            }
        }
    }

    func enumerateChanges(
        for observer: NSFileProviderChangeObserver,
        from anchor: NSFileProviderSyncAnchor
    ) {
        let accessor = providerAccessor
        let containerId = containerIdentifier

        DispatchQueue.global(qos: .userInitiated).async {
            guard let prov = accessor() else {
                observer.finishEnumeratingWithError(NSFileProviderError(.serverUnreachable))
                return
            }

            let path = containerId == .rootContainer ? "" : containerId.rawValue

            do {
                let items = try prov.listItems(path: path)
                let providerItems: [NSFileProviderItem] = items.map { item in
                    TCFSFileProviderItem(
                        identifier: NSFileProviderItemIdentifier(item.itemId),
                        parentIdentifier: containerId,
                        filename: item.filename,
                        isDirectory: item.isDirectory,
                        fileSize: item.fileSize,
                        modifiedTimestamp: item.modifiedTimestamp,
                        downloaded: false,
                        uploaded: true,
                        versionTag: item.contentHash,
                        conflictWith: item.conflictWith
                    )
                }

                if !providerItems.isEmpty {
                    observer.didUpdate(providerItems)
                }
            } catch {
                enumLogger.error("enumerateChanges failed: \(error.localizedDescription)")
            }

            let newAnchor = Self.makeAnchor()
            observer.finishEnumeratingChanges(upTo: newAnchor, moreComing: false)
        }
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        completionHandler(Self.makeAnchor())
    }

    private static func makeAnchor() -> NSFileProviderSyncAnchor {
        var timestamp = UInt64(Date().timeIntervalSince1970 * 1000)
        let data = Data(bytes: &timestamp, count: MemoryLayout<UInt64>.size)
        return NSFileProviderSyncAnchor(data)
    }
}
