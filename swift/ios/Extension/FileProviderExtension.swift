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

        guard let prov = provider else {
            completionHandler(nil, NSFileProviderError(.serverUnreachable))
            progress.completedUnitCount = 1
            return progress
        }

        let logicalPath = identifier.rawValue.trimmingCharacters(
            in: CharacterSet(charactersIn: "/")
        )
        let parentPath = Self.parentPath(forPath: logicalPath)
        let parentIdentifier = Self.parentIdentifier(forPath: logicalPath)
        DispatchQueue.global(qos: .userInitiated).async {
            do {
                let item = try prov.listItems(path: parentPath).first {
                    $0.itemId == logicalPath
                }
                guard let item else {
                    completionHandler(nil, NSFileProviderError(.noSuchItem))
                    progress.completedUnitCount = 1
                    return
                }
                completionHandler(
                    TCFSFileProviderItem(
                        identifier: identifier,
                        parentIdentifier: parentIdentifier,
                        filename: item.filename,
                        isDirectory: item.isDirectory,
                        fileSize: item.fileSize,
                        modifiedTimestamp: item.modifiedTimestamp,
                        downloaded: false,
                        uploaded: true,
                        versionTag: item.contentHash,
                        conflictWith: item.conflictWith
                    ),
                    nil
                )
            } catch {
                completionHandler(nil, Self.mapError(error))
            }
            progress.completedUnitCount = 1
        }
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
        let requestedVersionToken: String?
        if let requestedVersion {
            guard let token = String(data: requestedVersion.contentVersion, encoding: .utf8) else {
                completionHandler(
                    nil,
                    nil,
                    Self.versionUnavailableError("requested version token is not valid UTF-8")
                )
                return progress
            }
            requestedVersionToken = token.isEmpty ? nil : token
        } else {
            requestedVersionToken = nil
        }

        guard let prov = provider else {
            completionHandler(nil, nil, NSFileProviderError(.serverUnreachable))
            return progress
        }

        DispatchQueue.global(qos: .userInitiated).async {
            let tempDir = FileManager.default.temporaryDirectory
            let tempFile = tempDir.appendingPathComponent(UUID().uuidString)
            let itemId = itemIdentifier.rawValue

            do {
                let effectiveVersionToken: String
                if let requestedVersionToken {
                    effectiveVersionToken = requestedVersionToken
                } else {
                    let parentPath = Self.parentPath(forPath: itemId)
                    guard
                        let listedItem = try prov.listItems(path: parentPath).first(where: {
                            $0.itemId == itemId
                        }),
                        !listedItem.contentHash.isEmpty
                    else {
                        progress.completedUnitCount = 100
                        completionHandler(
                            nil,
                            nil,
                            Self.versionUnavailableError(
                                "no immutable manifest version is available for \(itemId)"
                            )
                        )
                        return
                    }
                    effectiveVersionToken = listedItem.contentHash
                }

                let progressAdapter = HydrationProgressCallback(progress: progress)
                try prov.hydrateFileVersionWithProgress(
                    itemId: itemId,
                    destinationPath: tempFile.path,
                    requestedVersion: effectiveVersionToken,
                    callback: progressAdapter
                )
                let attrs = try? FileManager.default.attributesOfItem(atPath: tempFile.path)
                let fileSize = (attrs?[.size] as? UInt64) ?? 0

                let item = TCFSFileProviderItem(
                    identifier: itemIdentifier,
                    parentIdentifier: Self.parentIdentifier(forPath: itemId),
                    filename: itemId.components(separatedBy: "/").last ?? itemId,
                    isDirectory: false,
                    fileSize: fileSize,
                    downloaded: true,
                    uploaded: true,
                    versionTag: effectiveVersionToken
                )
                progress.completedUnitCount = 100
                self.signalEnumeratorUpdate(for: Self.parentIdentifier(forPath: itemId))
                completionHandler(tempFile, item, nil)
            } catch {
                try? FileManager.default.removeItem(at: tempFile)
                logger.error("fetchContents failed: \(error.localizedDescription)")
                progress.completedUnitCount = 100
                completionHandler(nil, nil, Self.mapError(error))
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

    // FileProvider capabilities hide these actions in Files, but direct
    // filesystem changes can still invoke the callbacks. Keep them fail-closed
    // until Rust can condition publication on the exact baseVersion token.

    func createItem(
        basedOn itemTemplate: NSFileProviderItem,
        fields: NSFileProviderItemFields,
        contents url: URL?,
        options: NSFileProviderCreateItemOptions,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 100)
        progress.completedUnitCount = 100
        completionHandler(nil, [], false, NSFileProviderError(.cannotSynchronize))
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
        progress.completedUnitCount = 100
        completionHandler(nil, [], false, NSFileProviderError(.cannotSynchronize))
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
        progress.completedUnitCount = 1
        completionHandler(NSFileProviderError(.cannotSynchronize))
        return progress
    }

    // MARK: - Helpers

    private static func parentPath(forPath path: String) -> String {
        let components = path
            .trimmingCharacters(in: CharacterSet(charactersIn: "/"))
            .split(separator: "/")
        guard components.count > 1 else { return "" }
        return components.dropLast().joined(separator: "/")
    }

    private static func parentIdentifier(
        forPath path: String
    ) -> NSFileProviderItemIdentifier {
        let parent = parentPath(forPath: path)
        return parent.isEmpty ? .rootContainer : NSFileProviderItemIdentifier(parent)
    }

    private func signalEnumeratorUpdate(for containerIdentifier: NSFileProviderItemIdentifier) {
        manager?.signalEnumerator(for: containerIdentifier) { error in
            if let error = error {
                logger.warning("signalEnumerator failed: \(error.localizedDescription)")
            }
        }
    }

    private static func versionUnavailableError(_ description: String) -> NSError {
        // versionNoLongerAvailable is macOS-only. FileProvider requires iOS
        // extensions to wrap errors without a native representation this way.
        let underlying = NSError(
            domain: "io.tinyland.tcfs.ios.fileprovider.version",
            code: 1,
            userInfo: [NSLocalizedDescriptionKey: description]
        )
        return NSError(
            domain: NSCocoaErrorDomain,
            code: CocoaError.Code.xpcConnectionReplyInvalid.rawValue,
            userInfo: [
                NSLocalizedDescriptionKey: description,
                NSUnderlyingErrorKey: underlying,
            ]
        )
    }

    private static func mapError(_ error: Error) -> NSError {
        if let providerError = error as? ProviderError {
            switch providerError {
            case .NotFound:
                return NSFileProviderError(.noSuchItem) as NSError
            case .Conflict:
                // newerExtensionVersionFound is unavailable on iOS; use serverUnreachable
                return NSFileProviderError(.serverUnreachable) as NSError
            case .VersionMismatch:
                return versionUnavailableError("the requested immutable version is no longer current")
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

/// Bridges UniFFI ProgressCallback to NSProgress for iOS Files app progress bars.
class HydrationProgressCallback: ProgressCallback {
    private let progress: Progress

    init(progress: Progress) {
        self.progress = progress
    }

    func onProgress(completed: UInt64, total: UInt64) {
        guard total > 0 else { return }
        // Map chunk progress to 0-95 range (leave 5% for final write)
        let pct = Int64(Double(completed) / Double(total) * 95.0)
        progress.completedUnitCount = pct
    }
}
