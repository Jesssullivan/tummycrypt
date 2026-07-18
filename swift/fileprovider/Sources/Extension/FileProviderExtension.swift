import FileProvider
import Foundation
import Security
import os.log

private let logger = Logger(subsystem: "io.tinyland.tcfs.fileprovider", category: "extension")
private let sharedConfigService = "io.tinyland.tcfs.config"
private let sharedConfigAccount = "configJSON"
private let sharedConfigAccessGroupFallback = "group.io.tinyland.tcfs"

/// TCFS FileProvider extension — bridges to Rust via cbindgen C FFI.
///
/// Implements NSFileProviderReplicatedExtension for on-demand hydration
/// of files stored in SeaweedFS S3 via the tcfs-file-provider Rust crate.
class TCFSFileProviderExtension: NSObject, NSFileProviderReplicatedExtension {

    let domain: NSFileProviderDomain
    /// Cached provider handle — retries creation up to 3 times if daemon is slow to start.
    private var _cachedProvider: OpaquePointer?
    private var _providerAttempts = 0
    private let _providerLock = NSLock()

    /// Thread-safe provider accessor that retries creation on failure.
    /// Once the daemon socket is available, the provider is cached for the
    /// extension's lifetime. If creation fails 3 times, returns nil permanently.
    private var provider: OpaquePointer? {
        _providerLock.lock()
        defer { _providerLock.unlock() }
        if let cached = _cachedProvider { return cached }
        if _providerAttempts >= 3 { return nil }
        _providerAttempts += 1
        let p = Self.createProvider()
        if p != nil { _cachedProvider = p; _providerAttempts = 0 }
        return p
    }

    /// FileProvider manager for signaling enumerator updates after mutations.
    private lazy var manager: NSFileProviderManager? = NSFileProviderManager(for: domain)
    /// Whether the persistent background watch stream has been started.
    private var backgroundWatchStarted = false
    /// Retained NSFileProviderManager pointer for the background watch callback.
    private var watchManagerPtr: UnsafeMutableRawPointer?

    required init(domain: NSFileProviderDomain) {
        self.domain = domain
        super.init()
    }

    func invalidate() {
        if let ptr = watchManagerPtr {
            Unmanaged<NSFileProviderManager>.fromOpaque(ptr).release()
            watchManagerPtr = nil
        }
        _providerLock.lock()
        if let p = _cachedProvider {
            tcfs_provider_free(p)
            _cachedProvider = nil
        }
        _providerAttempts = 0
        _providerLock.unlock()
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

        let rawPath = identifier.rawValue
        let parentId = Self.parentIdentifier(forPath: rawPath)
        guard let prov = provider else {
            completionHandler(nil, NSFileProviderError(.serverUnreachable))
            progress.completedUnitCount = 1
            return progress
        }

        // Item lookup must preserve the opaque manifest token exposed by
        // enumeration. Synthesizing an empty/default version here lets
        // fileproviderd later request content without the version it observed.
        DispatchQueue.global(qos: .userInitiated).async {
            let lookup = Self.lookupProviderItem(
                provider: prov,
                identifier: identifier,
                parentIdentifier: parentId
            )
            progress.completedUnitCount = 1
            guard lookup.result == TCFS_ERROR_TCFS_ERROR_NONE else {
                completionHandler(nil, Self.mapError(lookup.result))
                return
            }
            guard let item = lookup.item else {
                completionHandler(nil, NSFileProviderError(.noSuchItem))
                return
            }
            completionHandler(item, nil)
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
        // Start with estimated size; updated by callback as real size is known
        let progress = Progress(totalUnitCount: 0)
        let itemId = itemIdentifier.rawValue
        let requestedVersionToken: String?
        if let requestedVersion {
            guard let token = String(data: requestedVersion.contentVersion, encoding: .utf8) else {
                completionHandler(nil, nil, NSFileProviderError(.versionNoLongerAvailable))
                return progress
            }
            requestedVersionToken = token.isEmpty ? nil : token
        } else {
            requestedVersionToken = nil
        }

        logger.error("fetchContents start for \(itemId, privacy: .public)")

        guard let prov = provider else {
            logger.error("fetchContents provider unavailable for \(itemId, privacy: .public)")
            completionHandler(nil, nil, NSFileProviderError(.serverUnreachable))
            return progress
        }

        DispatchQueue.global(qos: .userInitiated).async {
            let tempDir = FileManager.default.temporaryDirectory
            let tempFile = tempDir.appendingPathComponent(UUID().uuidString)

            // A nil/empty requested version means "latest" in FileProvider.
            // Resolve that latest version through the same authoritative
            // enumeration surface, then pass its non-empty token into Rust so
            // the read is still conditional on the metadata snapshot observed.
            let effectiveVersionToken: String
            if let requestedVersionToken {
                effectiveVersionToken = requestedVersionToken
            } else {
                let parentId = Self.parentIdentifier(forPath: itemId)
                let lookup = Self.lookupProviderItem(
                    provider: prov,
                    identifier: itemIdentifier,
                    parentIdentifier: parentId
                )
                guard lookup.result == TCFS_ERROR_TCFS_ERROR_NONE else {
                    progress.completedUnitCount = progress.totalUnitCount
                    completionHandler(nil, nil, Self.mapError(lookup.result))
                    return
                }
                guard
                    let item = lookup.item,
                    let token = String(data: item.itemVersion.contentVersion, encoding: .utf8),
                    !token.isEmpty
                else {
                    progress.completedUnitCount = progress.totalUnitCount
                    completionHandler(nil, nil, NSFileProviderError(.versionNoLongerAvailable))
                    return
                }
                effectiveVersionToken = token
            }

            logger.error(
                "fetchContents fetching \(itemId, privacy: .public) to \(tempFile.path, privacy: .public)"
            )

            // Capture progress for the C callback closure only after lookup
            // succeeds, so early fail-closed returns do not leak the retain.
            let progressPtr = Unmanaged.passRetained(progress).toOpaque()

            // C callback that updates NSProgress from Rust's chunk loop
            let callback: @convention(c) (UInt64, UInt64, UnsafeRawPointer?) -> Void = {
                completed, total, ctx in
                guard let ctx = ctx else { return }
                let prog = Unmanaged<Progress>.fromOpaque(ctx).takeUnretainedValue()
                prog.totalUnitCount = Int64(total)
                prog.completedUnitCount = Int64(completed)
            }

            let result = itemId.withCString { idPtr in
                tempFile.path.withCString { destPtr in
                    effectiveVersionToken.withCString { versionPtr in
                        tcfs_provider_fetch_versioned_with_progress(
                            prov, idPtr, destPtr, versionPtr,
                            callback,
                            progressPtr
                        )
                    }
                }
            }

            // Balance the passRetained
            Unmanaged<Progress>.fromOpaque(progressPtr).release()

            if result == TCFS_ERROR_TCFS_ERROR_NONE {
                let fileSize = (try? FileManager.default.attributesOfItem(
                    atPath: tempFile.path
                )[.size] as? UInt64) ?? 0
                logger.error(
                    "fetchContents completed for \(itemId, privacy: .public): bytes=\(fileSize)"
                )

                let parentId = TCFSFileProviderExtension.parentIdentifier(forPath: itemId)
                let item = TCFSFileProviderItem(
                    identifier: itemIdentifier,
                    parentIdentifier: parentId,
                    filename: itemId.components(separatedBy: "/").last ?? itemId,
                    isDirectory: false,
                    fileSize: fileSize,
                    downloaded: true,
                    uploaded: true,
                    versionTag: effectiveVersionToken
                )
                progress.completedUnitCount = progress.totalUnitCount
                self.signalEnumeratorUpdate(for: parentId)
                completionHandler(tempFile, item, nil)
            } else {
                try? FileManager.default.removeItem(at: tempFile)
                let backendError = Self.providerLastError(prov)
                logger.error(
                    "fetchContents failed for \(itemId, privacy: .public): code=\(result.rawValue), backend=\(backendError, privacy: .public)"
                )
                completionHandler(nil, nil, Self.mapError(result))
            }
        }

        return progress
    }

    // MARK: - Enumeration

    func enumerator(
        for containerItemIdentifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest
    ) throws -> NSFileProviderEnumerator {
        // Start background watch on first enumerate (provider is now initialized)
        if !backgroundWatchStarted, let prov = provider, let mgr = manager {
            backgroundWatchStarted = true
            startBackgroundWatch(provider: prov, manager: mgr)
        }

        return TCFSFileProviderEnumerator(
            providerAccessor: { [weak self] in self?.provider ?? nil },
            containerIdentifier: containerItemIdentifier
        )
    }

    /// Start a persistent Watch RPC stream that signals fileproviderd on changes.
    private func startBackgroundWatch(provider prov: OpaquePointer, manager mgr: NSFileProviderManager) {
        let callback: @convention(c) (UnsafeRawPointer?) -> Void = { ctx in
            guard let ctx = ctx else { return }
            let m = Unmanaged<NSFileProviderManager>.fromOpaque(ctx).takeUnretainedValue()
            m.signalEnumerator(for: .rootContainer) { _ in }
        }

        let ptr = Unmanaged.passRetained(mgr).toOpaque()
        watchManagerPtr = UnsafeMutableRawPointer(mutating: ptr)

        let result = tcfs_provider_start_watch(prov, callback, ptr)
        if result != TCFS_ERROR_TCFS_ERROR_NONE {
            logger.error("startBackgroundWatch: failed with \(result.rawValue)")
            Unmanaged<NSFileProviderManager>.fromOpaque(ptr).release()
            watchManagerPtr = nil
        }
    }

    // MARK: - Path utilities

    /// Compute the parent item identifier from a logical relative path.
    ///
    /// - `"dotfiles/bashrc"` → `NSFileProviderItemIdentifier("dotfiles/")`
    /// - `"dotfiles/"` → `.rootContainer`
    /// - `"readme.txt"` → `.rootContainer`
    static func parentIdentifier(forPath path: String) -> NSFileProviderItemIdentifier {
        let trimmed = path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let components = trimmed.components(separatedBy: "/")
        if components.count <= 1 {
            return .rootContainer
        }
        let parentPath = components.dropLast().joined(separator: "/") + "/"
        return NSFileProviderItemIdentifier(parentPath)
    }

    private static func logicalParentPath(_ identifier: NSFileProviderItemIdentifier) -> String {
        if identifier == .rootContainer {
            return ""
        }
        return identifier.rawValue.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
    }

    private static func lookupProviderItem(
        provider: OpaquePointer,
        identifier: NSFileProviderItemIdentifier,
        parentIdentifier: NSFileProviderItemIdentifier
    ) -> (result: TcfsError, item: NSFileProviderItem?) {
        let enumeration = TCFSFileProviderEnumerator.enumerateProviderItems(
            provider: provider,
            path: logicalParentPath(parentIdentifier),
            parentIdentifier: parentIdentifier,
            recursive: false
        )
        return (
            enumeration.result,
            enumeration.items.first { $0.itemIdentifier == identifier }
        )
    }

    // MARK: - Write operations

    // FileProvider capabilities hide these actions in Finder, but direct
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

    // MARK: - Enumerator signaling

    /// Signal fileproviderd to re-enumerate after a mutation so Finder updates immediately.
    private func signalEnumeratorUpdate(for containerIdentifier: NSFileProviderItemIdentifier) {
        manager?.signalEnumerator(for: containerIdentifier) { error in
            if let error = error {
                logger.warning("signalEnumerator failed: \(error.localizedDescription)")
            }
        }
    }

    // MARK: - Error mapping

    private static func mapError(_ code: TcfsError) -> NSError {
        switch code {
        case TCFS_ERROR_TCFS_ERROR_NOT_FOUND:
            return NSFileProviderError(.noSuchItem) as NSError
        case TCFS_ERROR_TCFS_ERROR_CONFLICT:
            return NSFileProviderError(.newerExtensionVersionFound) as NSError
        case TCFS_ERROR_TCFS_ERROR_VERSION_MISMATCH:
            return NSFileProviderError(.versionNoLongerAvailable) as NSError
        case TCFS_ERROR_TCFS_ERROR_ALREADY_EXISTS:
            return NSFileProviderError(.filenameCollision) as NSError
        default:
            return NSFileProviderError(.serverUnreachable) as NSError
        }
    }

    private static func providerLastError(_ prov: OpaquePointer) -> String {
        guard let errorPtr = tcfs_provider_last_error(prov) else {
            return "<none>"
        }
        defer { tcfs_string_free(errorPtr) }
        return String(cString: errorPtr)
    }

    // MARK: - Provider setup

    private static func createProvider() -> OpaquePointer? {
        logger.error("createProvider: loading config...")
        guard let config = loadConfig() else {
            logger.error("createProvider: config load failed — provider will be nil")
            return nil
        }
        logger.error("createProvider: config loaded (\(config.count) bytes), creating provider")

        let ptr = config.withCString { configPtr in
            tcfs_provider_new(configPtr)
        }
        if ptr != nil {
            logger.error("createProvider: provider created successfully")
        } else {
            logger.error("createProvider: tcfs_provider_new returned null")
        }
        return ptr
    }

    /// Load TCFS config, trying multiple sources in order of safety.
    ///
    /// Sources (in priority order):
    /// 0. Diagnostic build-time embedded config, when explicitly enabled
    /// 1. Shared Keychain — provisioned by host app, accessed via securityd XPC
    /// 2. XDG config path — requires sandbox temp-exception entitlement
    /// 3. App Group container file — deadlock-prone, short timeout
    ///
    /// IMPORTANT: Keychain is checked FIRST because it uses securityd XPC,
    /// completely bypassing the filesystem. UserDefaults with an App Group
    /// suite stores data in the Group Container, which is file-coordinated
    /// by fileproviderd — reading it during enumeration deadlocks.
    private static func loadConfig() -> String? {
        // 0. Diagnostic build-time embedded config. Production signing disables
        //    this by default so Keychain/App Group provisioning is exercised.
        if let b64 = embeddedConfigBase64,
           let data = Data(base64Encoded: b64),
           let config = String(data: data, encoding: .utf8),
           !config.isEmpty {
            logger.error("loadConfig: loaded from build-time embedded config")
            return config
        }
        logger.warning("loadConfig: no embedded config, trying Keychain")

        // 1. Shared Keychain — provisioned by the host app.
        //    Uses securityd XPC, no file I/O, no file coordination, no deadlock.
        if let config = readConfigFromKeychain() {
            logger.error("loadConfig: loaded from shared Keychain")
            return config
        }
        logger.warning("loadConfig: Keychain empty, trying XDG path")

        // 2. XDG config path (sandbox temp-exception may or may not work for extensions).
        let home = FileManager.default.homeDirectoryForCurrentUser
        let xdgPath = home.appendingPathComponent(".config/tcfs/fileprovider/config.json")
        if let config = try? String(contentsOf: xdgPath, encoding: .utf8) {
            logger.info("loadConfig: loaded from XDG path")
            return config
        }
        logger.warning("loadConfig: XDG path not accessible, trying App Group container file")

        // 3. App Group container file (last resort, deadlock-prone).
        let groupId = "group.io.tinyland.tcfs"
        if let containerURL = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: groupId
        ) {
            let configPath = containerURL.appendingPathComponent("config.json")

            var result: String?
            var readError: String?
            let sem = DispatchSemaphore(value: 0)
            DispatchQueue.global(qos: .utility).async {
                do {
                    result = try String(contentsOf: configPath, encoding: .utf8)
                } catch {
                    readError = error.localizedDescription
                }
                sem.signal()
            }
            if sem.wait(timeout: .now() + 3.0) == .success, let config = result {
                logger.info("loadConfig: loaded from App Group container file")
                return config
            }
            if let readError = readError {
                logger.warning(
                    "loadConfig: App Group container file read failed at \(configPath.path, privacy: .public): \(readError, privacy: .public)"
                )
            } else {
                logger.warning(
                    "loadConfig: App Group container file read timed out at \(configPath.path, privacy: .public)"
                )
            }
        }

        logger.error("loadConfig: no config found at any location")
        return nil
    }

    /// Read config JSON from the shared macOS keychain.
    /// Keychain access uses securityd XPC — no filesystem I/O, immune to
    /// fileproviderd's file coordination locks.
    ///
    /// The host app writes this item with an explicit app-group access group so
    /// the extension can read it without depending on each target's bundle ID.
    private static func readConfigFromKeychain() -> String? {
        let accessGroup = resolvedSharedConfigAccessGroup()
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrAccessGroup as String: accessGroup,
            kSecAttrService as String: sharedConfigService,
            kSecAttrAccount as String: sharedConfigAccount,
            kSecUseDataProtectionKeychain as String: kCFBooleanTrue as Any,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]

        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)

        if status != errSecSuccess {
            logger.warning("readConfigFromKeychain: SecItemCopyMatching returned \(status)")
        }

        guard status == errSecSuccess,
              let data = item as? Data,
              let config = String(data: data, encoding: .utf8),
              !config.isEmpty else {
            return nil
        }
        return config
    }

    private static func resolvedSharedConfigAccessGroup() -> String {
        guard let task = SecTaskCreateFromSelf(nil),
              let value = SecTaskCopyValueForEntitlement(
                  task,
                  "keychain-access-groups" as CFString,
                  nil
              )
        else {
            return sharedConfigAccessGroupFallback
        }

        if let groups = value as? [String],
           let group = groups.first(where: isSharedConfigAccessGroup) {
            return group
        }

        if let group = value as? String, isSharedConfigAccessGroup(group) {
            return group
        }

        return sharedConfigAccessGroupFallback
    }

    private static func isSharedConfigAccessGroup(_ group: String) -> Bool {
        group == sharedConfigAccessGroupFallback ||
            group.hasSuffix(".\(sharedConfigAccessGroupFallback)")
    }
}

// MARK: - Custom Actions (context menu items)

extension TCFSFileProviderExtension: NSFileProviderCustomAction {
    func performAction(
        identifier actionIdentifier: NSFileProviderExtensionActionIdentifier,
        onItemsWithIdentifiers itemIdentifiers: [NSFileProviderItemIdentifier],
        completionHandler: @escaping (Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: Int64(itemIdentifiers.count))

        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let prov = self?.provider else {
                completionHandler(NSFileProviderError(.serverUnreachable))
                return
            }

            for itemId in itemIdentifiers {
                let itemPath = itemId.rawValue

                switch actionIdentifier.rawValue {
                case "io.tinyland.tcfs.action.unsync":
                    // Dehydrate without deleting remote storage.
                    logger.info("action.unsync: \(itemPath)")
                    let result = itemPath.withCString { pathPtr in
                        tcfs_provider_unsync(prov, pathPtr)
                    }
                    if result != TCFS_ERROR_TCFS_ERROR_NONE {
                        logger.error("action.unsync failed for \(itemPath): \(result.rawValue)")
                    }

                case "io.tinyland.tcfs.action.pin":
                    // Pin: fetch content immediately so it stays on disk
                    logger.info("action.pin: \(itemPath)")
                    let tmpDir = FileManager.default.temporaryDirectory
                    let dest = tmpDir.appendingPathComponent(UUID().uuidString)
                    let result = itemPath.withCString { pathPtr in
                        dest.path.withCString { destPtr in
                            tcfs_provider_fetch(prov, pathPtr, destPtr)
                        }
                    }
                    if result != TCFS_ERROR_TCFS_ERROR_NONE {
                        let backendError = Self.providerLastError(prov)
                        logger.error(
                            "action.pin failed for \(itemPath, privacy: .public): code=\(result.rawValue), backend=\(backendError, privacy: .public)"
                        )
                    }
                    // Clean up temp file — the system manages the real materialized copy
                    try? FileManager.default.removeItem(at: dest)

                default:
                    logger.warning("unknown action: \(actionIdentifier.rawValue)")
                }

                progress.completedUnitCount += 1
            }

            // Signal enumerator to refresh badges after action
            if let domain = self?.domain {
                NSFileProviderManager(for: domain)?.signalEnumerator(
                    for: .rootContainer,
                    completionHandler: { _ in }
                )
            }

            completionHandler(nil)
        }

        return progress
    }
}
