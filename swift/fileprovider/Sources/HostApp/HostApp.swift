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
        let domain = NSFileProviderDomain(
            identifier: NSFileProviderDomainIdentifier("io.tinyland.tcfs"),
            displayName: "TCFS"
        )
        configureTestingModeIfRequested(domain)

        // Provision config to Keychain (best-effort fallback for pre-built binaries).
        provisionConfig()

        DispatchQueue.global(qos: .userInitiated).async {
            // Always remove then re-add the domain. This triggers a fresh
            // domainCreation in fileproviderd, which forces initial enumeration.
            //
            // IMPORTANT: Do NOT use NSFileProviderManager(for:) after add.
            // That constructor accesses the Group Container to find domain
            // metadata, which deadlocks because fileproviderd holds a
            // permanent file coordination lock on the Group Container.
            let removeSem = DispatchSemaphore(value: 0)
            NSFileProviderManager.remove(domain) { error in
                if let error = error {
                    hostLogger.error("remove: \(error.localizedDescription)")
                } else {
                    hostLogger.error("remove: OK")
                }
                removeSem.signal()
            }
            removeSem.wait()

            // Brief pause for fileproviderd to clean up.
            Thread.sleep(forTimeInterval: 2.0)

            let addSem = DispatchSemaphore(value: 0)
            NSFileProviderManager.add(domain) { error in
                if let error = error {
                    hostLogger.error("add: \(error.localizedDescription)")
                } else {
                    hostLogger.error("add: OK — domain created, enumeration will start")
                }
                addSem.signal()
            }
            addSem.wait()

            // Give fileproviderd time to start initial enumeration.
            Thread.sleep(forTimeInterval: 5.0)
            hostLogger.error("host app exiting")
            exit(0)
        }

        RunLoop.current.run()
    }

    private static func provisionConfig() {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let xdgPath = home.appendingPathComponent(".config/tcfs/fileprovider/config.json")

        guard let config = try? String(contentsOf: xdgPath, encoding: .utf8) else {
            hostLogger.error("provisionConfig: no config at \(xdgPath.path)")
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
        ]
        let updateAttrs: [String: Any] = [
            kSecValueData as String: data,
        ]

        var status = SecItemUpdate(updateQuery as CFDictionary, updateAttrs as CFDictionary)

        if status == errSecItemNotFound {
            var addQuery = updateQuery
            addQuery[kSecValueData as String] = data
            addQuery[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
            status = SecItemAdd(addQuery as CFDictionary, nil)
        }

        if status == errSecSuccess {
            hostLogger.error(
                "provisionConfig: provisioned \(keychainConfig.count) bytes to shared Keychain group"
            )
        } else if status == errSecMissingEntitlement {
            hostLogger.error(
                "provisionConfig: Keychain write missing entitlement for configured access group"
            )
        } else {
            hostLogger.error("provisionConfig: Keychain write failed with status \(status)")
        }
    }

    private static func configureTestingModeIfRequested(_ domain: NSFileProviderDomain) {
        guard ProcessInfo.processInfo.environment["TCFS_FILEPROVIDER_TESTING_MODE_ALWAYS_ENABLED"] == "1" else {
            return
        }

        domain.testingModes = [.alwaysEnabled]
        hostLogger.error("testingMode: requested alwaysEnabled for FileProvider domain")
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
            hostLogger.error("provisionConfig: master_key_file could not be read")
            return serializeConfigObject(object, fallback: config)
        }

        guard keyData.count == 32 else {
            hostLogger.error(
                "provisionConfig: master_key_file has invalid byte length \(keyData.count)"
            )
            return serializeConfigObject(object, fallback: config)
        }

        object["master_key_base64"] = keyData.base64EncodedString()
        hostLogger.error("provisionConfig: added master key material to Keychain copy")
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
