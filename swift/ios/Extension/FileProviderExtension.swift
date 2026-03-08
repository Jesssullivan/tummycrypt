import FileProvider
import Foundation
import os.log

private let logger = Logger(subsystem: "io.tinyland.tcfs.fileprovider.ios", category: "extension")

/// iOS TCFS FileProvider extension — bridges to Rust via UniFFI.
///
/// Uses the auto-generated `TcfsProviderHandle` from UniFFI instead of
/// the macOS cbindgen C FFI. Config comes from iOS Keychain only.
class TCFSFileProviderExtension: NSObject, NSFileProviderReplicatedExtension {

    let domain: NSFileProviderDomain
    private lazy var provider: TcfsProviderHandle? = Self.createProvider()
    private lazy var manager: NSFileProviderManager? = NSFileProviderManager(for: domain)

    required init(domain: NSFileProviderDomain) {
        self.domain = domain
        super.init()
    }

    func invalidate() {
        // TcfsProviderHandle is ARC-managed by UniFFI — just nil out
        provider = nil
    }

    // MARK: - Item lookup

    func item(
        for identifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 1)

        if identifier == .rootContainer {
            completionHandler(TCFSFileProviderItem.rootItem(), nil)
            progress.completedUnitCount = 1
            return progress
        }

        if identifier == .trashContainer {
            completionHandler(nil, NSFileProviderError(.noSuchItem))
            progress.completedUnitCount = 1
            return progress
        }

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

            do {
                try prov.hydrateFile(itemId: itemId, destinationPath: tempFile.path)
                let attrs = try? FileManager.default.attributesOfItem(atPath: tempFile.path)
                let fileSize = (attrs?[.size] as? UInt64) ?? 0

                let item = TCFSFileProviderItem(
                    identifier: itemIdentifier,
                    parentIdentifier: .rootContainer,
                    filename: itemId.components(separatedBy: "/").last ?? itemId,
                    isDirectory: false,
                    fileSize: fileSize,
                    downloaded: true,
                    uploaded: true
                )
                progress.completedUnitCount = 100
                self.signalEnumeratorUpdate(for: .rootContainer)
                completionHandler(tempFile, item, nil)
            } catch {
                logger.error("fetchContents failed: \(error.localizedDescription)")
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
            providerAccessor: { [weak self] in self?.provider },
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
                do {
                    try prov.createDirectory(parentPath: parentPath, dirName: filename)
                    let dirPath = parentPath.isEmpty ? filename : "\(parentPath)/\(filename)"
                    let item = TCFSFileProviderItem(
                        identifier: NSFileProviderItemIdentifier(dirPath),
                        parentIdentifier: itemTemplate.parentItemIdentifier,
                        filename: filename,
                        isDirectory: true,
                        fileSize: 0
                    )
                    progress.completedUnitCount = 100
                    self.signalEnumeratorUpdate(for: itemTemplate.parentItemIdentifier)
                    completionHandler(item, [], false, nil)
                } catch {
                    progress.completedUnitCount = 100
                    completionHandler(nil, [], false, Self.mapError(error))
                }
            } else if let contentsURL = url {
                let accessed = contentsURL.startAccessingSecurityScopedResource()
                defer { if accessed { contentsURL.stopAccessingSecurityScopedResource() } }

                let remotePath = parentPath.isEmpty ? filename : "\(parentPath)/\(filename)"
                do {
                    try prov.uploadFile(localPath: contentsURL.path, remotePath: remotePath)
                    let fileSize = (try? FileManager.default.attributesOfItem(atPath: contentsURL.path)[.size] as? UInt64) ?? 0
                    let item = TCFSFileProviderItem(
                        identifier: NSFileProviderItemIdentifier(remotePath),
                        parentIdentifier: itemTemplate.parentItemIdentifier,
                        filename: filename,
                        isDirectory: false,
                        fileSize: fileSize,
                        downloaded: true,
                        uploaded: true
                    )
                    progress.completedUnitCount = 100
                    self.signalEnumeratorUpdate(for: itemTemplate.parentItemIdentifier)
                    completionHandler(item, [], false, nil)
                } catch {
                    progress.completedUnitCount = 100
                    completionHandler(nil, [], false, Self.mapError(error))
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
            if changedFields.contains(.contents), let contentsURL = newContents {
                let accessed = contentsURL.startAccessingSecurityScopedResource()
                defer { if accessed { contentsURL.stopAccessingSecurityScopedResource() } }

                let remotePath = item.itemIdentifier.rawValue
                do {
                    try prov.uploadFile(localPath: contentsURL.path, remotePath: remotePath)
                    let fileSize = (try? FileManager.default.attributesOfItem(atPath: contentsURL.path)[.size] as? UInt64) ?? 0
                    let updatedItem = TCFSFileProviderItem(
                        identifier: item.itemIdentifier,
                        parentIdentifier: item.parentItemIdentifier,
                        filename: item.filename,
                        isDirectory: false,
                        fileSize: fileSize,
                        downloaded: true,
                        uploaded: true
                    )
                    progress.completedUnitCount = 100
                    self.signalEnumeratorUpdate(for: item.parentItemIdentifier)
                    completionHandler(updatedItem, [], false, nil)
                } catch {
                    progress.completedUnitCount = 100
                    completionHandler(nil, [], false, Self.mapError(error))
                }
            } else {
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
            do {
                try prov.deleteItem(itemId: identifier.rawValue)
                progress.completedUnitCount = 1
                self.signalEnumeratorUpdate(for: .rootContainer)
                completionHandler(nil)
            } catch {
                progress.completedUnitCount = 1
                completionHandler(Self.mapError(error))
            }
        }

        return progress
    }

    // MARK: - Helpers

    private func signalEnumeratorUpdate(for containerIdentifier: NSFileProviderItemIdentifier) {
        manager?.signalEnumerator(for: containerIdentifier) { error in
            if let error = error {
                logger.warning("signalEnumerator failed: \(error.localizedDescription)")
            }
        }
    }

    private static func mapError(_ error: Error) -> NSError {
        if let providerError = error as? ProviderError {
            switch providerError {
            case .NotFound:
                return NSFileProviderError(.noSuchItem) as NSError
            case .Conflict:
                // newerExtensionVersionFound is unavailable on iOS; use serverUnreachable
                return NSFileProviderError(.serverUnreachable) as NSError
            default:
                return NSFileProviderError(.serverUnreachable) as NSError
            }
        }
        return NSFileProviderError(.serverUnreachable) as NSError
    }

    // MARK: - Provider setup

    private static func createProvider() -> TcfsProviderHandle? {
        logger.error("createProvider: loading config from Keychain...")
        guard let config = loadConfigFromKeychain() else {
            logger.error("createProvider: Keychain config not found")
            return nil
        }

        do {
            let provider = try TcfsProviderHandle(config: config)
            logger.error("createProvider: provider created successfully")
            return provider
        } catch {
            logger.error("createProvider: failed: \(error.localizedDescription)")
            return nil
        }
    }

    /// Load TCFS config from iOS Keychain.
    ///
    /// The host app writes credentials to the shared Keychain access group
    /// during initial setup. The extension reads them here.
    private static func loadConfigFromKeychain() -> ProviderConfig? {
        let fields = ["s3_endpoint", "s3_bucket", "access_key", "s3_secret",
                      "remote_prefix", "device_id", "encryption_passphrase", "encryption_salt"]

        var values: [String: String] = [:]
        for field in fields {
            let query: [String: Any] = [
                kSecClass as String: kSecClassGenericPassword,
                kSecAttrService as String: "io.tinyland.tcfs.config",
                kSecAttrAccount as String: field,
                kSecAttrAccessGroup as String: "group.io.tinyland.tcfs",
                kSecReturnData as String: true,
                kSecMatchLimit as String: kSecMatchLimitOne,
            ]

            var item: CFTypeRef?
            let status = SecItemCopyMatching(query as CFDictionary, &item)

            if status == errSecSuccess,
               let data = item as? Data,
               let value = String(data: data, encoding: .utf8) {
                values[field] = value
            } else if field == "encryption_passphrase" || field == "encryption_salt" {
                values[field] = ""  // Optional fields default to empty
            } else {
                logger.error("loadConfigFromKeychain: missing required field '\(field)' (status: \(status))")
                return nil
            }
        }

        return ProviderConfig(
            s3Endpoint: values["s3_endpoint"]!,
            s3Bucket: values["s3_bucket"]!,
            accessKey: values["access_key"]!,
            s3Secret: values["s3_secret"]!,
            remotePrefix: values["remote_prefix"]!,
            deviceId: values["device_id"]!,
            encryptionPassphrase: values["encryption_passphrase"]!,
            encryptionSalt: values["encryption_salt"]!
        )
    }
}
