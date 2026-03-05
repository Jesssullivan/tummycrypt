import FileProvider
import Foundation

/// Enumerates TCFS directory contents by calling into the Rust FFI layer.
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

            var outItems: UnsafeMutablePointer<TcfsFileItem>?
            var outCount: UInt = 0

            let result = path.withCString { pathPtr in
                tcfs_provider_enumerate(prov, pathPtr, &outItems, &outCount)
            }

            guard result == TCFS_ERROR_TCFS_ERROR_NONE, let items = outItems, outCount > 0 else {
                observer.finishEnumerating(upTo: nil)
                return
            }

            var providerItems: [NSFileProviderItem] = []
            let count = Int(outCount)

            for i in 0..<count {
                let item = items[i]

                let itemId = item.item_id.map { String(cString: $0) } ?? ""
                let filename = item.filename.map { String(cString: $0) } ?? ""

                providerItems.append(
                    TCFSFileProviderItem(
                        identifier: NSFileProviderItemIdentifier(itemId),
                        parentIdentifier: containerId,
                        filename: filename,
                        isDirectory: item.is_directory,
                        fileSize: item.file_size
                    )
                )
            }

            // Free the C array
            tcfs_file_items_free(outItems, outCount)

            observer.didEnumerate(providerItems)
            observer.finishEnumerating(upTo: nil)
        }
    }

    func enumerateChanges(
        for observer: NSFileProviderChangeObserver,
        from anchor: NSFileProviderSyncAnchor
    ) {
        // MVP: no incremental changes — full re-enumeration
        observer.finishEnumeratingChanges(upTo: anchor, moreComing: false)
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        // MVP: use timestamp as anchor
        let data = "\(Date().timeIntervalSince1970)".data(using: .utf8)!
        completionHandler(NSFileProviderSyncAnchor(data))
    }
}
