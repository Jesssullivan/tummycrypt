import FileProvider
import Foundation

/// TCFS FileProvider extension — bridges to Rust via cbindgen C FFI.
///
/// Implements NSFileProviderReplicatedExtension for on-demand hydration
/// of files stored in SeaweedFS S3 via the tcfs-file-provider Rust crate.
class TCFSFileProviderExtension: NSObject, NSFileProviderReplicatedExtension {

    let domain: NSFileProviderDomain
    private var provider: OpaquePointer?

    required init(domain: NSFileProviderDomain) {
        self.domain = domain
        super.init()
        self.provider = Self.createProvider()
    }

    func invalidate() {
        if let p = provider {
            tcfs_provider_free(p)
            provider = nil
        }
    }

    // MARK: - Item lookup

    func item(
        for identifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 1)

        if identifier == .rootContainer {
            completionHandler(
                TCFSFileProviderItem.rootItem(),
                nil
            )
            progress.completedUnitCount = 1
            return progress
        }

        // For non-root items, return the item from its identifier path
        completionHandler(
            TCFSFileProviderItem(
                identifier: identifier,
                parentIdentifier: .rootContainer,
                filename: identifier.rawValue.components(separatedBy: "/").last ?? identifier.rawValue,
                isDirectory: false,
                fileSize: 0
            ),
            nil
        )
        progress.completedUnitCount = 1
        return progress
    }

    // MARK: - Content fetching (hydration)

    func fetchContents(
        for itemIdentifier: NSFileProviderItemIdentifier,
        version requestedVersion: NSFileProviderItemVersion?,
        request: NSFileProviderRequest,
        completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 100)

        guard let prov = provider else {
            completionHandler(nil, nil, NSFileProviderError(.serverUnreachable))
            return progress
        }

        DispatchQueue.global(qos: .userInitiated).async {
            let tempDir = FileManager.default.temporaryDirectory
            let tempFile = tempDir.appendingPathComponent(UUID().uuidString)

            let itemId = itemIdentifier.rawValue
            let result = itemId.withCString { idPtr in
                tempFile.path.withCString { destPtr in
                    tcfs_provider_fetch(prov, idPtr, destPtr)
                }
            }

            if result == TCFS_ERROR_TCFS_ERROR_NONE {
                let item = TCFSFileProviderItem(
                    identifier: itemIdentifier,
                    parentIdentifier: .rootContainer,
                    filename: itemId.components(separatedBy: "/").last ?? itemId,
                    isDirectory: false,
                    fileSize: (try? FileManager.default.attributesOfItem(atPath: tempFile.path)[.size] as? UInt64) ?? 0
                )
                progress.completedUnitCount = 100
                completionHandler(tempFile, item, nil)
            } else {
                progress.completedUnitCount = 100
                completionHandler(nil, nil, NSFileProviderError(.serverUnreachable))
            }
        }

        return progress
    }

    // MARK: - Enumeration

    func enumerator(
        for containerItemIdentifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest
    ) throws -> NSFileProviderEnumerator {
        return TCFSFileProviderEnumerator(
            provider: provider,
            containerIdentifier: containerItemIdentifier
        )
    }

    // MARK: - Write stubs (read-only for MVP)

    func createItem(
        basedOn itemTemplate: NSFileProviderItem,
        fields: NSFileProviderItemFields,
        contents url: URL?,
        options: NSFileProviderCreateItemOptions,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        completionHandler(nil, [], false, NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
        return Progress()
    }

    func modifyItem(
        _ item: NSFileProviderItem,
        baseVersion version: NSFileProviderItemVersion,
        changedFields: NSFileProviderItemFields,
        contents newContents: URL?,
        options: NSFileProviderModifyItemOptions,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        completionHandler(nil, [], false, NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
        return Progress()
    }

    func deleteItem(
        identifier: NSFileProviderItemIdentifier,
        baseVersion version: NSFileProviderItemVersion,
        options: NSFileProviderDeleteItemOptions,
        request: NSFileProviderRequest,
        completionHandler: @escaping (Error?) -> Void
    ) -> Progress {
        completionHandler(NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
        return Progress()
    }

    // MARK: - Provider setup

    private static func createProvider() -> OpaquePointer? {
        guard let config = loadConfig() else { return nil }

        return config.withCString { configPtr in
            tcfs_provider_new(configPtr)
        }
    }

    /// Load TCFS config from App Group shared container.
    private static func loadConfig() -> String? {
        let groupId = "group.io.tinyland.tcfs"

        guard let containerURL = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: groupId
        ) else {
            return nil
        }

        let configPath = containerURL.appendingPathComponent("config.json")
        return try? String(contentsOf: configPath, encoding: .utf8)
    }
}
