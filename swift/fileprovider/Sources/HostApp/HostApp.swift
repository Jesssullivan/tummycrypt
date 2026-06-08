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

                probeRootIfRequested(manager)
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

    private static func probeRootIfRequested(_ manager: NSFileProviderManager) {
        guard ProcessInfo.processInfo.environment["TCFS_FILEPROVIDER_ROOT_PROBE"] == "1" else {
            return
        }

        let nonceSuffix = fileProviderActionNonceLogSuffix()
        if let rawPath = ProcessInfo.processInfo.environment[
            "TCFS_FILEPROVIDER_ROOT_PROBE_PATH"
        ], !rawPath.isEmpty {
            let rootURL = URL(fileURLWithPath: rawPath)
            hostEvent("rootProbe: direct path: \(rootURL.path)\(nonceSuffix)")
            probeDirectoryEntries(at: rootURL, label: "direct path", nonceSuffix: nonceSuffix)
        }

        let visibleSem = DispatchSemaphore(value: 0)
        var visibleURL: URL?
        var visibleError: Error?
        manager.getUserVisibleURL(for: .rootContainer) { url, error in
            visibleURL = url
            visibleError = error
            visibleSem.signal()
        }

        let timeoutSeconds = hostActionTimeoutSeconds()
        if visibleSem.wait(timeout: .now() + .seconds(timeoutSeconds)) == .timedOut {
            hostEvent("rootProbe: user-visible URL timed out after \(timeoutSeconds)s\(nonceSuffix)")
            return
        }
        if let visibleError {
            hostEvent("rootProbe: user-visible URL failed: \(describe(visibleError))\(nonceSuffix)")
            return
        }
        guard let visibleURL else {
            hostEvent("rootProbe: user-visible URL missing\(nonceSuffix)")
            return
        }

        hostEvent("rootProbe: user-visible URL: \(visibleURL.path)\(nonceSuffix)")
        probeDirectoryEntries(at: visibleURL, label: "user-visible", nonceSuffix: nonceSuffix)
    }

    private static func probeDirectoryEntries(at url: URL, label: String, nonceSuffix: String) {
        let coordinator = NSFileCoordinator(filePresenter: nil)
        var coordinationError: NSError?
        var operationError: Error?
        var entries: [String] = []

        coordinator.coordinate(readingItemAt: url, options: [], error: &coordinationError) { coordinatedURL in
            do {
                let accessed = coordinatedURL.startAccessingSecurityScopedResource()
                defer {
                    if accessed {
                        coordinatedURL.stopAccessingSecurityScopedResource()
                    }
                }

                entries = try FileManager.default.contentsOfDirectory(
                    at: coordinatedURL,
                    includingPropertiesForKeys: nil,
                    options: []
                )
                .map { $0.path }
                .sorted()
            } catch {
                operationError = error
            }
        }

        if let coordinationError {
            hostEvent("rootProbe: \(label) coordination failed: \(describe(coordinationError))\(nonceSuffix)")
            return
        }
        if let operationError {
            hostEvent("rootProbe: \(label) list failed: \(describe(operationError))\(nonceSuffix)")
            return
        }
        if entries.isEmpty {
            hostEvent("rootProbe: \(label) entries: <empty>\(nonceSuffix)")
            return
        }

        hostEvent("rootProbe: \(label) entries: \(entries.count)\(nonceSuffix)")
        for entry in entries.prefix(20) {
            hostEvent("rootProbe: \(label) entry: \(entry)\(nonceSuffix)")
        }
    }

    private static func describe(_ error: Error) -> String {
        let nsError = error as NSError
        var details = "\(nsError.localizedDescription) [domain=\(nsError.domain) code=\(nsError.code)]"

        if let path = nsError.userInfo[NSFilePathErrorKey] as? String {
            details += " path=\(path)"
        }
        if let url = nsError.userInfo[NSURLErrorKey] as? URL {
            details += " url=\(url.path)"
        }
        if let underlying = nsError.userInfo[NSUnderlyingErrorKey] as? NSError {
            details += " underlying=\(underlying.domain)(\(underlying.code)): \(underlying.localizedDescription)"
        }

        return details
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

        // (1) Master-key material — the established inlining (host reads
        //     master_key_file from ~/.config/tcfs and inlines master_key_base64
        //     so the sandboxed FileProvider .appex can decrypt without an fs read).
        if let encoded = object["master_key_base64"] as? String, !encoded.isEmpty {
            // Already inlined; still try the per-device enrichment below.
        } else if let keyPath = object["master_key_file"] as? String, !keyPath.isEmpty {
            let expandedKeyPath = (keyPath as NSString).expandingTildeInPath
            let keyURL = URL(fileURLWithPath: expandedKeyPath)
            if let keyData = try? Data(contentsOf: keyURL) {
                if keyData.count == 32 {
                    object["master_key_base64"] = keyData.base64EncodedString()
                    hostEvent("provisionConfig: added master key material to Keychain copy")
                } else {
                    hostEvent(
                        "provisionConfig: master_key_file has invalid byte length \(keyData.count)"
                    )
                }
            } else {
                hostEvent("provisionConfig: master_key_file could not be read")
            }
        } else {
            hostLogger.warning("provisionConfig: no master_key_file for Keychain enrichment")
        }

        // (2) Per-device material (TIN-1417) — inert unless wrap_mode is
        //     dual/per_device. The macOS FileProvider .appex cannot fs-read
        //     ~/.config/tcfs (devices.json / device-<id>.age), so the host (which
        //     CAN read them) inlines the active recipients + this device's age
        //     secret into the Keychain config copy, mirroring master_key_base64.
        enrichPerDeviceMaterial(&object)

        return serializeConfigObject(object, fallback: config)
    }

    /// Returns true when the config selects a non-master wrap mode (dual /
    /// per_device, or the legacy `per_device_wrapping: true` alias). Per-device
    /// Keychain inlining is INERT for the default `master` mode — the inlined
    /// fields are simply not written, keeping wrap_mode=master byte-identical.
    private static func wrapModeIsPerDevice(_ object: [String: Any]) -> Bool {
        if let mode = object["wrap_mode"] as? String, !mode.isEmpty {
            return mode == "dual" || mode == "per_device"
        }
        if let legacy = object["per_device_wrapping"] as? Bool {
            return legacy
        }
        return false
    }

    /// Inline the active per-device recipients and THIS device's age secret into
    /// the Keychain config copy so the sandboxed FileProvider can build a
    /// device-aware EncryptionContext without reading ~/.config/tcfs.
    ///
    /// The inlined keys are consumed by the Rust read-side
    /// (`crates/tcfs-file-provider/src/device_ctx.rs`,
    /// `resolve_recipients` / `resolve_device_identity`), which prefers them over
    /// the on-disk registry/secret:
    ///   - `device_recipients`: [{ "device_id", "recipient" }, ...] — active,
    ///     non-revoked devices that carry a real age recipient.
    ///   - `device_recipients_all_capable`: Bool — the host's roll-call result
    ///     (every active device carries a real age recipient). Gates the
    ///     PerDevice contract drop on the Rust side exactly like the daemon.
    ///   - `device_secret`: String — this device's armored age secret
    ///     (`device-<device_id>.age` contents).
    ///
    /// DRAFT / UNVERIFIED: this cannot be compiled or device-tested in the agent
    /// sandbox; it needs a real-device Xcode build + FileProvider QA. The
    /// registry here is read with NO signature verification (see B4 — the
    /// DeviceRegistry is forgeable today); this code only mirrors the existing
    /// trust posture and does NOT add a new trust boundary.
    private static func enrichPerDeviceMaterial(_ object: inout [String: Any]) {
        guard wrapModeIsPerDevice(object) else {
            // wrap_mode=master (default): leave the config untouched.
            return
        }

        // Resolve the registry path: explicit override, else default
        // ~/.config/tcfs/devices.json.
        let registryPath: String
        if let explicit = object["device_registry_path"] as? String, !explicit.isEmpty {
            registryPath = (explicit as NSString).expandingTildeInPath
        } else {
            let home = FileManager.default.homeDirectoryForCurrentUser
            registryPath = home.appendingPathComponent(".config/tcfs/devices.json").path
        }

        let registryURL = URL(fileURLWithPath: registryPath)
        guard let registryData = try? Data(contentsOf: registryURL),
              let registryObject = try? JSONSerialization.jsonObject(with: registryData)
              as? [String: Any],
              let devices = registryObject["devices"] as? [[String: Any]]
        else {
            hostEvent("provisionConfig: device registry unreadable; skipping per-device inlining")
            return
        }

        // Active (non-revoked) devices that carry a *real* age recipient. Mirrors
        // the Rust `active_devices().filter(is_real_age_public_key)` and the
        // roll-call gate (all_capable = no active device lacks a real recipient).
        var recipients: [[String: String]] = []
        var activeCount = 0
        var capableCount = 0
        for device in devices {
            let revoked = (device["revoked"] as? Bool) ?? false
            if revoked { continue }
            activeCount += 1
            guard let deviceId = device["device_id"] as? String, !deviceId.isEmpty,
                  let publicKey = device["public_key"] as? String,
                  isRealAgePublicKey(publicKey)
            else {
                continue
            }
            capableCount += 1
            recipients.append(["device_id": deviceId, "recipient": publicKey])
        }

        guard !recipients.isEmpty else {
            hostEvent("provisionConfig: no active age recipients; skipping per-device inlining")
            return
        }

        object["device_recipients"] = recipients
        // all_capable iff every active device carried a real recipient.
        object["device_recipients_all_capable"] = (activeCount == capableCount)

        // Inline THIS device's age secret (device-<device_id>.age), resolved
        // relative to the registry directory — mirrors the Rust
        // `device_secret_key_path(registry_path, device_id)` layout.
        if let deviceId = object["device_id"] as? String, !deviceId.isEmpty {
            let registryDir = registryURL.deletingLastPathComponent()
            let secretURL = registryDir.appendingPathComponent("device-\(deviceId).age")
            if let secretData = try? Data(contentsOf: secretURL),
               let secret = String(data: secretData, encoding: .utf8) {
                let trimmed = secret.trimmingCharacters(in: .whitespacesAndNewlines)
                if !trimmed.isEmpty {
                    object["device_secret"] = trimmed
                    hostEvent(
                        "provisionConfig: inlined \(recipients.count) recipient(s) + device "
                            + "secret for \(deviceId) (all_capable="
                            + "\(activeCount == capableCount))"
                    )
                } else {
                    hostEvent("provisionConfig: device secret file empty; recipients inlined only")
                }
            } else {
                // Recipients inlined but no local secret — the Rust read-side then
                // falls back to its fs read (which fails closed in-sandbox). We do
                // NOT inline a partial/empty secret.
                hostEvent("provisionConfig: device-\(deviceId).age unreadable; recipients only")
            }
        } else {
            hostEvent("provisionConfig: no device_id in config; cannot inline device secret")
        }
    }

    /// Approximate Swift mirror of `tcfs_secrets::device::is_real_age_public_key`,
    /// which in Rust does a FULL `age::x25519::Recipient` bech32 parse. Swift has
    /// no age crate here, so this only checks the `age1` prefix and a plausible
    /// length — a deliberately conservative heuristic.
    ///
    /// SAFETY: the Rust read-side (`resolve_recipients`) RE-VALIDATES every
    /// inlined recipient with the real `is_real_age_public_key`, so a Swift
    /// false-positive cannot inject a malformed recipient into the wrap set. The
    /// one Swift-only signal the Rust side trusts is
    /// `device_recipients_all_capable`; a Swift over-count there could let the
    /// Rust side enter PerDevice (drop the master wrap) prematurely. Hardening
    /// this to a real bech32/age parse on the Swift side is a follow-up (needs an
    /// age-Swift dependency); flagged for real-device review.
    private static func isRealAgePublicKey(_ publicKey: String) -> Bool {
        let trimmed = publicKey.trimmingCharacters(in: .whitespacesAndNewlines)
        // age1 + bech32 payload; real keys are ~62 chars. Require a healthy
        // minimum to reject obvious placeholders without over-trusting.
        return trimmed.hasPrefix("age1") && trimmed.count >= 50
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
