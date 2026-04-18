import FileProvider
import Foundation
import Security
import os.log

private let hostLogger = Logger(subsystem: "io.tinyland.tcfs", category: "host")
private let sharedConfigService = "io.tinyland.tcfs.config"
private let sharedConfigAccount = "configJSON"
private let sharedConfigAccessGroup = "group.io.tinyland.tcfs"

@main
struct TCFSProviderApp {
    static func main() {
        let domain = NSFileProviderDomain(
            identifier: NSFileProviderDomainIdentifier("io.tinyland.tcfs"),
            displayName: "TCFS"
        )

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

        guard let data = config.data(using: .utf8) else { return }

        let updateQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecUseDataProtectionKeychain as String: true,
            kSecAttrAccessGroup as String: sharedConfigAccessGroup,
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
                "provisionConfig: provisioned \(config.count) bytes to shared Keychain group"
            )
        } else if status == errSecMissingEntitlement {
            hostLogger.error(
                "provisionConfig: Keychain write missing entitlement for \(sharedConfigAccessGroup)"
            )
        } else {
            hostLogger.error("provisionConfig: Keychain write failed with status \(status)")
        }
    }
}
