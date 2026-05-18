import FileProvider
import Foundation
import Security
import os.log

private let hostLogger = Logger(subsystem: "io.tinyland.tcfs", category: "host")
private let sharedConfigService = "io.tinyland.tcfs.config"
private let sharedConfigAccount = "configJSON"
private let sharedConfigAccessGroupFallback = "group.io.tinyland.tcfs"

@main
struct TCFSProviderApp {
    static func main() {
        if policyProbeOnlyRequested() {
            hostEvent("policyProbe: main entered")
        }

        let domain = NSFileProviderDomain(
            identifier: NSFileProviderDomainIdentifier("io.tinyland.tcfs"),
            displayName: "TCFS"
        )
        if policyProbeOnlyRequested() {
            hostEvent("policyProbe: domain created")
        }
        configureTestingModeIfRequested(domain)
        if policyProbeOnlyRequested() {
            hostEvent("policyProbe: OK")
            exit(0)
        }

        // Provision config to Keychain (best-effort fallback for pre-built binaries).
        provisionConfig()

        DispatchQueue.global(qos: .userInitiated).async {
            removeDomainIfRequested(domain)

            // Add/update is idempotent and avoids racing fileproviderd while
            // macOS is also reloading this extension after app registration.
            let addSem = DispatchSemaphore(value: 0)
            NSFileProviderManager.add(domain) { error in
                if let error = error {
                    hostEvent("add: \(error.localizedDescription)")
                } else {
                    hostEvent("add: OK - domain available")
                }
                addSem.signal()
            }
            let addTimeoutSeconds = hostActionTimeoutSeconds()
            if addSem.wait(timeout: .now() + .seconds(addTimeoutSeconds)) == .timedOut {
                hostEvent("add: timed out after \(addTimeoutSeconds)s")
                exit(2)
            }

            if let manager = NSFileProviderManager(for: domain) {
                let signalSem = DispatchSemaphore(value: 0)
                manager.signalEnumerator(for: .workingSet) { error in
                    if let error = error {
                        hostEvent("signal workingSet: \(error.localizedDescription)")
                    } else {
                        hostEvent("signal workingSet: OK")
                    }
                    signalSem.signal()
                }
                if signalSem.wait(timeout: .now() + 5.0) == .timedOut {
                    hostEvent("signal workingSet: timed out")
                }

                requestDownloadIfRequested(manager)
                evictIfRequested(manager)
            } else {
                hostEvent("signal workingSet: manager unavailable")
            }

            // Give fileproviderd time to start initial enumeration.
            Thread.sleep(forTimeInterval: 5.0)
            hostEvent("host app exiting")
            exit(0)
        }

        RunLoop.current.run()
    }

    private static func removeDomainIfRequested(_ domain: NSFileProviderDomain) {
        guard ProcessInfo.processInfo.environment["TCFS_FILEPROVIDER_REBUILD_DOMAIN"] == "1" else {
            return
        }

        let removeSem = DispatchSemaphore(value: 0)
        NSFileProviderManager.remove(domain) { error in
            if let error = error {
                hostEvent("remove: \(error.localizedDescription)")
            } else {
                hostEvent("remove: OK - domain removed")
            }
            removeSem.signal()
        }

        let timeoutSeconds = hostActionTimeoutSeconds()
        if removeSem.wait(timeout: .now() + .seconds(timeoutSeconds)) == .timedOut {
            hostEvent("remove: timed out after \(timeoutSeconds)s")
            exit(2)
        }
    }

    private static func requestDownloadIfRequested(_ manager: NSFileProviderManager) {
        guard let rawIdentifier = ProcessInfo.processInfo.environment[
            "TCFS_FILEPROVIDER_REQUEST_DOWNLOAD_IDENTIFIER"
        ], !rawIdentifier.isEmpty else {
            return
        }

        let itemIdentifier = NSFileProviderItemIdentifier(rawIdentifier)
        let nonceSuffix = fileProviderActionNonceLogSuffix()
        let requestSem = DispatchSemaphore(value: 0)
        manager.requestDownloadForItem(
            withIdentifier: itemIdentifier,
            requestedRange: NSRange(location: NSNotFound, length: 0)
        ) { error in
            if let error = error {
                hostEvent("requestDownload: \(rawIdentifier): \(error.localizedDescription)\(nonceSuffix)")
            } else {
                hostEvent("requestDownload: \(rawIdentifier): OK\(nonceSuffix)")
            }
            requestSem.signal()
        }

        if requestSem.wait(timeout: .now() + 15.0) == .timedOut {
            hostEvent("requestDownload: \(rawIdentifier): timed out\(nonceSuffix)")
        }
    }

    private static func evictIfRequested(_ manager: NSFileProviderManager) {
        guard let rawIdentifier = ProcessInfo.processInfo.environment[
            "TCFS_FILEPROVIDER_EVICT_IDENTIFIER"
        ], !rawIdentifier.isEmpty else {
            return
        }

        let itemIdentifier = NSFileProviderItemIdentifier(rawIdentifier)
        let nonceSuffix = fileProviderActionNonceLogSuffix()
        let evictSem = DispatchSemaphore(value: 0)
        manager.evictItem(identifier: itemIdentifier) { error in
            if let error = error {
                hostEvent("evict: \(rawIdentifier): \(error.localizedDescription)\(nonceSuffix)")
            } else {
                hostEvent("evict: \(rawIdentifier): OK\(nonceSuffix)")
            }
            evictSem.signal()
        }

        if evictSem.wait(timeout: .now() + 15.0) == .timedOut {
            hostEvent("evict: \(rawIdentifier): timed out\(nonceSuffix)")
        }
    }

    private static func hostEvent(_ message: String) {
        hostLogger.error("\(message, privacy: .public)")

        guard ProcessInfo.processInfo.environment["TCFS_FILEPROVIDER_HOST_STDERR_LOG"] == "1",
              let data = "\(message)\n".data(using: .utf8)
        else {
            return
        }

        FileHandle.standardError.write(data)
    }

    private static func hostActionTimeoutSeconds() -> Int {
        guard let raw = ProcessInfo.processInfo.environment[
            "TCFS_FILEPROVIDER_HOST_ACTION_TIMEOUT_SECS"
        ],
              let parsed = Int(raw),
              parsed > 0
        else {
            return 30
        }

        return parsed
    }

    private static func policyProbeOnlyRequested() -> Bool {
        ProcessInfo.processInfo.environment["TCFS_FILEPROVIDER_HOST_POLICY_PROBE_ONLY"] == "1"
    }

    private static func fileProviderActionNonceLogSuffix() -> String {
        guard let nonce = ProcessInfo.processInfo.environment[
            "TCFS_FILEPROVIDER_ACTION_NONCE"
        ], !nonce.isEmpty else {
            return ""
        }
        return " nonce=\(nonce)"
    }

    private static func provisionConfig() {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let xdgPath = home.appendingPathComponent(".config/tcfs/fileprovider/config.json")

        guard let config = try? String(contentsOf: xdgPath, encoding: .utf8) else {
            hostEvent("provisionConfig: no config at \(xdgPath.path)")
            return
        }

        let keychainConfig = configForKeychain(config)
        guard let data = keychainConfig.data(using: .utf8) else { return }

        let accessGroup = resolvedSharedConfigAccessGroup()
        let updateQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrAccessGroup as String: accessGroup,
            kSecAttrService as String: sharedConfigService,
            kSecAttrAccount as String: sharedConfigAccount,
            kSecUseDataProtectionKeychain as String: kCFBooleanTrue as Any,
        ]
        let updateAttrs: [String: Any] = [
            kSecValueData as String: data,
        ]

        let addItem: () -> OSStatus = {
            var addQuery = updateQuery
            addQuery[kSecValueData as String] = data
            addQuery[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
            return SecItemAdd(addQuery as CFDictionary, nil)
        }

        var status = SecItemUpdate(updateQuery as CFDictionary, updateAttrs as CFDictionary)

        if status == errSecItemNotFound {
            status = addItem()
        }

        if status == errSecDuplicateItem {
            hostLogger.warning("provisionConfig: replacing duplicate shared Keychain config item")
            _ = SecItemDelete(updateQuery as CFDictionary)

            let legacyQuery: [String: Any] = [
                kSecClass as String: kSecClassGenericPassword,
                kSecAttrService as String: sharedConfigService,
                kSecAttrAccount as String: sharedConfigAccount,
            ]
            _ = SecItemDelete(legacyQuery as CFDictionary)
            status = addItem()
        }

        if status == errSecSuccess {
            hostEvent("provisionConfig: provisioned \(keychainConfig.count) bytes to shared Keychain group")
        } else if status == errSecMissingEntitlement {
            hostEvent("provisionConfig: Keychain write missing entitlement for configured access group")
        } else {
            hostEvent("provisionConfig: Keychain write failed with status \(status)")
        }
    }

    private static func configureTestingModeIfRequested(_ domain: NSFileProviderDomain) {
        guard ProcessInfo.processInfo.environment["TCFS_FILEPROVIDER_TESTING_MODE_ALWAYS_ENABLED"] == "1" else {
            return
        }

        domain.testingModes = [.alwaysEnabled]
        hostEvent("testingMode: requested alwaysEnabled for FileProvider domain")
    }

    private static func configForKeychain(_ config: String) -> String {
        guard let data = config.data(using: .utf8),
              var object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else {
            hostLogger.warning("provisionConfig: config is not JSON; storing original bytes")
            return config
        }

        if let encoded = object["master_key_base64"] as? String, !encoded.isEmpty {
            return serializeConfigObject(object, fallback: config)
        }

        guard let keyPath = object["master_key_file"] as? String, !keyPath.isEmpty else {
            hostLogger.warning("provisionConfig: no master_key_file for Keychain enrichment")
            return serializeConfigObject(object, fallback: config)
        }

        let expandedKeyPath = (keyPath as NSString).expandingTildeInPath
        let keyURL = URL(fileURLWithPath: expandedKeyPath)
        guard let keyData = try? Data(contentsOf: keyURL) else {
            hostEvent("provisionConfig: master_key_file could not be read")
            return serializeConfigObject(object, fallback: config)
        }

        guard keyData.count == 32 else {
            hostEvent("provisionConfig: master_key_file has invalid byte length \(keyData.count)")
            return serializeConfigObject(object, fallback: config)
        }

        object["master_key_base64"] = keyData.base64EncodedString()
        hostEvent("provisionConfig: added master key material to Keychain copy")
        return serializeConfigObject(object, fallback: config)
    }

    private static func serializeConfigObject(
        _ object: [String: Any],
        fallback: String
    ) -> String {
        guard JSONSerialization.isValidJSONObject(object),
              let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]),
              let serialized = String(data: data, encoding: .utf8)
        else {
            hostLogger.warning("provisionConfig: could not serialize enriched config")
            return fallback
        }

        return serialized
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
