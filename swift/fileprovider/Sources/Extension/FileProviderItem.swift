import FileProvider
import UniformTypeIdentifiers

/// NSFileProviderItem implementation for TCFS files.
///
/// Maps between the Rust `TcfsFileItem` C struct and the
/// NSFileProviderItem protocol that macOS/iOS expect.
class TCFSFileProviderItem: NSObject, NSFileProviderItem {

    let itemIdentifier: NSFileProviderItemIdentifier
    let parentItemIdentifier: NSFileProviderItemIdentifier
    let filename: String
    let contentType: UTType
    let documentSize: NSNumber?
    let itemVersion: NSFileProviderItemVersion

    init(
        identifier: NSFileProviderItemIdentifier,
        parentIdentifier: NSFileProviderItemIdentifier,
        filename: String,
        isDirectory: Bool,
        fileSize: UInt64
    ) {
        self.itemIdentifier = identifier
        self.parentItemIdentifier = parentIdentifier
        self.filename = filename
        self.contentType = isDirectory ? .folder : (UTType(filenameExtension: (filename as NSString).pathExtension) ?? .data)
        self.documentSize = isDirectory ? nil : NSNumber(value: fileSize)
        self.itemVersion = NSFileProviderItemVersion(
            contentVersion: "1".data(using: .utf8)!,
            metadataVersion: "1".data(using: .utf8)!
        )
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
