import FileProvider
import Foundation

/// TCFS FileProvider extension — bridges to Rust via cbindgen C FFI.
///
/// Implements NSFileProviderReplicatedExtension for on-demand hydration
/// of files stored in SeaweedFS S3 via the tcfs-file-provider Rust crate.
class TCFSFileProviderExtension: NSObject, NSFileProviderReplicatedExtension {

    let domain: NSFileProviderDomain
    /// Provider is created lazily on first use to avoid blocking the XPC bringup.
    /// `tcfs_provider_new()` creates a tokio runtime and S3 operator, which can
    /// take seconds — long enough to exceed fileproviderd's initial handshake timeout.
    private lazy var provider: OpaquePointer? = Self.createProvider()

    required init(domain: NSFileProviderDomain) {
        self.domain = domain
        super.init()
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

        // Trash container not supported — tell fileproviderd so it doesn't
        // create reconciliation entries that never resolve.
        if identifier == .trashContainer {
            completionHandler(nil, NSFileProviderError(.noSuchItem))
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
        // Pass a closure so the enumerator can resolve the provider off the
        // calling (file-coordination) thread.  Accessing `self.provider` here
        // would trigger the lazy init synchronously, which blocks long enough
        // to cause an EDEADLK file-coordination deadlock on first access.
        return TCFSFileProviderEnumerator(
            providerAccessor: { [weak self] in self?.provider ?? nil },
            containerIdentifier: containerItemIdentifier
        )
    }

    // MARK: - Write operations

    func createItem(
        basedOn itemTemplate: NSFileProviderItem,
        fields: NSFileProviderItemFields,
        contents url: URL?,
        options: NSFileProviderCreateItemOptions,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 100)

        guard let prov = provider else {
            completionHandler(nil, [], false, NSFileProviderError(.serverUnreachable))
            return progress
        }

        DispatchQueue.global(qos: .userInitiated).async {
            let parentPath = itemTemplate.parentItemIdentifier == .rootContainer
                ? "" : itemTemplate.parentItemIdentifier.rawValue
            let filename = itemTemplate.filename

            if itemTemplate.contentType == .folder {
                // Create directory
                let result = parentPath.withCString { parentPtr in
                    filename.withCString { namePtr in
                        tcfs_provider_create_dir(prov, parentPtr, namePtr)
                    }
                }

                if result == TCFS_ERROR_TCFS_ERROR_NONE {
                    let dirPath = parentPath.isEmpty ? filename : "\(parentPath)/\(filename)"
                    let item = TCFSFileProviderItem(
                        identifier: NSFileProviderItemIdentifier(dirPath),
                        parentIdentifier: itemTemplate.parentItemIdentifier,
                        filename: filename,
                        isDirectory: true,
                        fileSize: 0
                    )
                    progress.completedUnitCount = 100
                    completionHandler(item, [], false, nil)
                } else {
                    progress.completedUnitCount = 100
                    completionHandler(nil, [], false, Self.mapError(result))
                }
            } else if let contentsURL = url {
                // Upload file
                let accessed = contentsURL.startAccessingSecurityScopedResource()
                defer { if accessed { contentsURL.stopAccessingSecurityScopedResource() } }

                let remotePath = parentPath.isEmpty ? filename : "\(parentPath)/\(filename)"
                let result = contentsURL.path.withCString { localPtr in
                    remotePath.withCString { remotePtr in
                        tcfs_provider_upload(prov, localPtr, remotePtr)
                    }
                }

                if result == TCFS_ERROR_TCFS_ERROR_NONE {
                    let fileSize = (try? FileManager.default.attributesOfItem(atPath: contentsURL.path)[.size] as? UInt64) ?? 0
                    let item = TCFSFileProviderItem(
                        identifier: NSFileProviderItemIdentifier(remotePath),
                        parentIdentifier: itemTemplate.parentItemIdentifier,
                        filename: filename,
                        isDirectory: false,
                        fileSize: fileSize
                    )
                    progress.completedUnitCount = 100
                    completionHandler(item, [], false, nil)
                } else {
                    progress.completedUnitCount = 100
                    completionHandler(nil, [], false, Self.mapError(result))
                }
            } else {
                completionHandler(nil, [], false, NSFileProviderError(.noSuchItem))
            }
        }

        return progress
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
        let progress = Progress(totalUnitCount: 100)

        guard let prov = provider else {
            completionHandler(nil, [], false, NSFileProviderError(.serverUnreachable))
            return progress
        }

        DispatchQueue.global(qos: .userInitiated).async {
            // Handle content modification (re-upload)
            if changedFields.contains(.contents), let contentsURL = newContents {
                let accessed = contentsURL.startAccessingSecurityScopedResource()
                defer { if accessed { contentsURL.stopAccessingSecurityScopedResource() } }

                let remotePath = item.itemIdentifier.rawValue
                let result = contentsURL.path.withCString { localPtr in
                    remotePath.withCString { remotePtr in
                        tcfs_provider_upload(prov, localPtr, remotePtr)
                    }
                }

                if result == TCFS_ERROR_TCFS_ERROR_NONE {
                    let fileSize = (try? FileManager.default.attributesOfItem(atPath: contentsURL.path)[.size] as? UInt64) ?? 0
                    let updatedItem = TCFSFileProviderItem(
                        identifier: item.itemIdentifier,
                        parentIdentifier: item.parentItemIdentifier,
                        filename: item.filename,
                        isDirectory: false,
                        fileSize: fileSize
                    )
                    progress.completedUnitCount = 100
                    completionHandler(updatedItem, [], false, nil)
                } else {
                    progress.completedUnitCount = 100
                    completionHandler(nil, [], false, Self.mapError(result))
                }
            } else if changedFields.contains(.filename) {
                // Rename: delete old index entry, re-upload to new path
                let oldPath = item.itemIdentifier.rawValue
                let parentPath = item.parentItemIdentifier == .rootContainer
                    ? "" : item.parentItemIdentifier.rawValue
                let newRemotePath = parentPath.isEmpty ? item.filename : "\(parentPath)/\(item.filename)"

                // Delete old entry
                let deleteResult = oldPath.withCString { idPtr in
                    tcfs_provider_delete(prov, idPtr)
                }
                if deleteResult != TCFS_ERROR_TCFS_ERROR_NONE {
                    progress.completedUnitCount = 100
                    completionHandler(nil, [], false, Self.mapError(deleteResult))
                    return
                }

                let renamedItem = TCFSFileProviderItem(
                    identifier: NSFileProviderItemIdentifier(newRemotePath),
                    parentIdentifier: item.parentItemIdentifier,
                    filename: item.filename,
                    isDirectory: item.contentType == .folder,
                    fileSize: (item.documentSize as? UInt64) ?? 0
                )
                progress.completedUnitCount = 100
                completionHandler(renamedItem, [], false, nil)
            } else {
                // No content or filename change — return item as-is
                progress.completedUnitCount = 100
                completionHandler(item, [], false, nil)
            }
        }

        return progress
    }

    func deleteItem(
        identifier: NSFileProviderItemIdentifier,
        baseVersion version: NSFileProviderItemVersion,
        options: NSFileProviderDeleteItemOptions,
        request: NSFileProviderRequest,
        completionHandler: @escaping (Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 1)

        guard let prov = provider else {
            completionHandler(NSFileProviderError(.serverUnreachable))
            return progress
        }

        DispatchQueue.global(qos: .userInitiated).async {
            let result = identifier.rawValue.withCString { idPtr in
                tcfs_provider_delete(prov, idPtr)
            }

            progress.completedUnitCount = 1
            if result == TCFS_ERROR_TCFS_ERROR_NONE {
                completionHandler(nil)
            } else {
                completionHandler(Self.mapError(result))
            }
        }

        return progress
    }

    // MARK: - Error mapping

    private static func mapError(_ code: TcfsError) -> NSError {
        switch code {
        case TCFS_ERROR_TCFS_ERROR_NOT_FOUND:
            return NSFileProviderError(.noSuchItem) as NSError
        case TCFS_ERROR_TCFS_ERROR_CONFLICT:
            return NSFileProviderError(.newerExtensionVersionFound) as NSError
        case TCFS_ERROR_TCFS_ERROR_ALREADY_EXISTS:
            return NSFileProviderError(.filenameCollision) as NSError
        default:
            return NSFileProviderError(.serverUnreachable) as NSError
        }
    }

    // MARK: - Provider setup

    private static func createProvider() -> OpaquePointer? {
        guard let config = loadConfig() else { return nil }

        return config.withCString { configPtr in
            tcfs_provider_new(configPtr)
        }
    }

    /// Load TCFS config from App Group shared container, falling back to XDG config.
    private static func loadConfig() -> String? {
        // Try App Group container first (sandboxed .appex)
        let groupId = "group.io.tinyland.tcfs"
        if let containerURL = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: groupId
        ) {
            let configPath = containerURL.appendingPathComponent("config.json")
            if let config = try? String(contentsOf: configPath, encoding: .utf8) {
                return config
            }
        }

        // Fall back to XDG config path (development / non-sandboxed)
        let home = FileManager.default.homeDirectoryForCurrentUser
        let xdgPath = home.appendingPathComponent(".config/tcfs/fileprovider/config.json")
        return try? String(contentsOf: xdgPath, encoding: .utf8)
    }
}
