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

    /// Swift mirror of `tcfs_secrets::device::is_real_age_public_key`, which in
    /// Rust does a FULL `age::x25519::Recipient` bech32 parse. Swift has no age
    /// crate here, so this performs a real bech32 decode in pure Swift —
    /// validating the `age` human-readable part, the bech32 charset, the BCH
    /// checksum, and that the decoded payload is exactly the 32-byte X25519
    /// public key an age recipient carries — instead of a prefix/length
    /// heuristic. An age v1 recipient is `bech32("age", x25519_pubkey[32])`.
    ///
    /// DEFENSE IN DEPTH (this is one of two layers — neither is solely trusted):
    ///   1. The Rust read-side (`resolve_recipients`) RE-VALIDATES every inlined
    ///      recipient with the real `is_real_age_public_key` and RE-DERIVES
    ///      `all_capable` from its OWN re-validated set — it does NOT trust the
    ///      host's `device_recipients_all_capable` boolean. So even a Swift
    ///      false-positive here cannot inject a malformed recipient into the wrap
    ///      set NOR force a premature PerDevice master-wrap drop (lockout).
    ///   2. This bech32 decode tightens the host-side roll call so the inlined
    ///      `device_recipients_all_capable` signal is accurate in the common
    ///      case, reducing spurious Dual downgrades.
    ///
    /// NOTE: this still does not verify registry AUTHENTICITY (a forged registry
    /// can carry a genuinely well-formed attacker recipient) — that is B4's
    /// signed-registry job, out of scope here.
    ///
    /// DRAFT / UNVERIFIED: cannot be compiled or device-tested in the agent
    /// sandbox; needs a real-device Xcode build + FileProvider QA. Do not claim
    /// Swift build success.
    private static func isRealAgePublicKey(_ publicKey: String) -> Bool {
        let trimmed = publicKey.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let decoded = decodeBech32(trimmed), decoded.hrp == "age" else {
            return false
        }
        // age v1 recipient payload is the raw 32-byte X25519 public key.
        return decoded.data.count == 32
    }

    /// Minimal bech32 decoder (BIP-0173) sufficient to validate an age v1
    /// recipient: lowercase-only, HRP separated by the last `1`, valid charset,
    /// and a correct BCH checksum. Returns the human-readable part and the
    /// 8-bit-regrouped data payload, or nil on any malformation. (age uses
    /// classic bech32, not bech32m.)
    private static func decodeBech32(_ input: String) -> (hrp: String, data: [UInt8])? {
        // Reject mixed case (bech32 forbids it) and non-ASCII.
        let lower = input.lowercased()
        if lower != input && input.uppercased() != input {
            return nil
        }
        let s = lower
        guard s.count >= 8, s.count <= 1023, s.allSatisfy({ $0.isASCII }) else {
            return nil
        }
        guard let sepIndex = s.lastIndex(of: "1") else { return nil }
        let hrp = String(s[s.startIndex..<sepIndex])
        let dataPart = String(s[s.index(after: sepIndex)...])
        // HRP must be non-empty; data part must include the 6-char checksum.
        guard !hrp.isEmpty, dataPart.count >= 6 else { return nil }
        guard hrp.allSatisfy({ c in
            guard let v = c.asciiValue else { return false }
            return v >= 33 && v <= 126
        }) else { return nil }

        let charset = Array("qpzry9x8gf2tvdw0s3jn54khce6mua7l")
        var values: [UInt8] = []
        values.reserveCapacity(dataPart.count)
        for c in dataPart {
            guard let idx = charset.firstIndex(of: c) else { return nil }
            values.append(UInt8(idx))
        }

        guard bech32VerifyChecksum(hrp: hrp, values: values) else { return nil }

        // Strip the 6-symbol checksum and regroup 5-bit -> 8-bit.
        let payload5 = Array(values.dropLast(6))
        guard let payload8 = convertBits(payload5, from: 5, to: 8, pad: false) else {
            return nil
        }
        return (hrp, payload8)
    }

    private static func bech32HrpExpand(_ hrp: String) -> [UInt8] {
        let bytes = Array(hrp.utf8)
        var out: [UInt8] = []
        out.reserveCapacity(bytes.count * 2 + 1)
        for b in bytes { out.append(b >> 5) }
        out.append(0)
        for b in bytes { out.append(b & 31) }
        return out
    }

    private static func bech32Polymod(_ values: [UInt8]) -> UInt32 {
        let gen: [UInt32] = [0x3b6a_57b2, 0x2650_8e6d, 0x1ea1_19fa, 0x3d42_33dd, 0x2a14_62b3]
        var chk: UInt32 = 1
        for v in values {
            let top = chk >> 25
            chk = ((chk & 0x1ff_ffff) << 5) ^ UInt32(v)
            for i in 0..<5 where ((top >> UInt32(i)) & 1) == 1 {
                chk ^= gen[i]
            }
        }
        return chk
    }

    private static func bech32VerifyChecksum(hrp: String, values: [UInt8]) -> Bool {
        // Classic bech32 constant is 1 (bech32m would be 0x2bc830a3).
        bech32Polymod(bech32HrpExpand(hrp) + values) == 1
    }

    /// Regroup a base-2^`from` array into base-2^`to`. Mirrors the reference
    /// bech32 `convertbits`. With `pad: false` any leftover bits must be zero.
    private static func convertBits(
        _ data: [UInt8],
        from: UInt32,
        to: UInt32,
        pad: Bool
    ) -> [UInt8]? {
        var acc: UInt32 = 0
        var bits: UInt32 = 0
        var out: [UInt8] = []
        let maxv: UInt32 = (1 << to) - 1
        for value in data {
            let v = UInt32(value)
            if (v >> from) != 0 { return nil }
            acc = (acc << from) | v
            bits += from
            while bits >= to {
                bits -= to
                out.append(UInt8((acc >> bits) & maxv))
            }
        }
        if pad {
            if bits > 0 {
                out.append(UInt8((acc << (to - bits)) & maxv))
            }
        } else if bits >= from || ((acc << (to - bits)) & maxv) != 0 {
            return nil
        }
        return out
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
