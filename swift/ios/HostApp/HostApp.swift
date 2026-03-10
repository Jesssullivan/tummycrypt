import SwiftUI
import FileProvider
import Security
import os.log

private let hostLogger = Logger(subsystem: "io.tinyland.tcfs.ios", category: "host")

@main
struct TCFSApp: App {
    @StateObject private var viewModel = TCFSViewModel()
    @StateObject private var authViewModel = AuthViewModel()

    var body: some Scene {
        WindowGroup {
            ContentView(viewModel: viewModel, authViewModel: authViewModel)
                .onOpenURL { url in
                    handleDeepLink(url)
                }
        }
    }

    private func handleDeepLink(_ url: URL) {
        guard url.scheme == "tcfs" else {
            hostLogger.warning("Ignoring URL with unknown scheme: \(url.absoluteString)")
            return
        }

        let host = url.host()

        switch host {
        case "bootstrap":
            handleBootstrapLink(url)
        case "enroll":
            handleEnrollLink(url)
        default:
            hostLogger.warning("Unrecognized deep link host: \(host ?? "nil") in \(url.absoluteString)")
        }
    }

    private func handleBootstrapLink(_ url: URL) {
        // BootstrapConfig.parse() already handles tcfs://bootstrap?data=<base64>
        guard let config = BootstrapConfig.parse(url.absoluteString) else {
            hostLogger.error("Failed to parse bootstrap deep link: \(url.absoluteString)")
            return
        }

        let deviceId = config.device_id
            ?? "ios-\(UIDevice.current.name.lowercased().replacingOccurrences(of: " ", with: "-"))"

        viewModel.saveConfig(
            endpoint: config.s3_endpoint,
            bucket: config.s3_bucket,
            accessKey: config.access_key,
            s3Secret: config.s3_secret,
            remotePrefix: config.remote_prefix ?? "default",
            deviceId: deviceId,
            passphrase: config.encryption_passphrase ?? "",
            salt: config.encryption_salt ?? ""
        )

        hostLogger.info("Bootstrap config saved via deep link (endpoint=\(config.s3_endpoint))")
    }

    private func handleEnrollLink(_ url: URL) {
        // Extract the data query parameter, matching QRScannerView's parsing
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let dataParam = components.queryItems?.first(where: { $0.name == "data" })?.value else {
            hostLogger.error("Enroll deep link missing 'data' parameter: \(url.absoluteString)")
            return
        }

        authViewModel.processInviteData(dataParam, tcfsViewModel: viewModel)
        hostLogger.info("Enrollment invite processed via deep link")
    }
}

class TCFSViewModel: ObservableObject {
    @Published var status: String = "Not configured"
    @Published var isConfigured: Bool = false
    @Published var syncFileCount: UInt64 = 0
    @Published var syncLastError: String? = nil

    private let domain = NSFileProviderDomain(
        identifier: NSFileProviderDomainIdentifier("io.tinyland.tcfs"),
        displayName: "TCFS"
    )

    init() {
        checkConfiguration()
        refreshSyncStatus()
    }

    func refreshSyncStatus() {
        guard isConfigured else { return }

        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self = self else { return }
            guard let config = Self.loadConfigForStatus() else { return }

            do {
                let provider = try TcfsProviderHandle(config: config)
                let syncStatus = try provider.getSyncStatus()
                DispatchQueue.main.async {
                    self.syncFileCount = syncStatus.filesSynced
                    self.syncLastError = syncStatus.lastError
                }
            } catch {
                DispatchQueue.main.async {
                    self.syncLastError = error.localizedDescription
                }
            }
        }
    }

    private static func loadConfigForStatus() -> ProviderConfig? {
        func readKeychain(_ account: String) -> String? {
            let query: [String: Any] = [
                kSecClass as String: kSecClassGenericPassword,
                kSecAttrService as String: "io.tinyland.tcfs.config",
                kSecAttrAccount as String: account,
                kSecAttrAccessGroup as String: "group.io.tinyland.tcfs",
                kSecReturnData as String: true,
                kSecMatchLimit as String: kSecMatchLimitOne,
            ]
            var item: CFTypeRef?
            guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
                  let data = item as? Data,
                  let value = String(data: data, encoding: .utf8) else {
                return nil
            }
            return value
        }

        guard let endpoint = readKeychain("s3_endpoint"),
              let bucket = readKeychain("s3_bucket"),
              let accessKey = readKeychain("access_key"),
              let secret = readKeychain("s3_secret"),
              let prefix = readKeychain("remote_prefix"),
              let deviceId = readKeychain("device_id") else {
            return nil
        }

        return ProviderConfig(
            s3Endpoint: endpoint,
            s3Bucket: bucket,
            accessKey: accessKey,
            s3Secret: secret,
            remotePrefix: prefix,
            deviceId: deviceId,
            encryptionPassphrase: readKeychain("encryption_passphrase") ?? "",
            encryptionSalt: readKeychain("encryption_salt") ?? ""
        )
    }

    func checkConfiguration() {
        let fields = ["s3_endpoint", "s3_bucket", "access_key", "s3_secret", "device_id"]
        var allPresent = true

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
            if SecItemCopyMatching(query as CFDictionary, &item) != errSecSuccess {
                allPresent = false
                break
            }
        }

        isConfigured = allPresent
        status = allPresent ? "Configured" : "Missing credentials"
    }

    func registerDomain() {
        status = "Registering..."

        NSFileProviderManager.remove(domain) { [weak self] _ in
            guard let self = self else { return }

            Thread.sleep(forTimeInterval: 1.0)

            NSFileProviderManager.add(self.domain) { error in
                DispatchQueue.main.async {
                    if let error = error {
                        hostLogger.error("Domain registration failed: \(error.localizedDescription)")
                        self.status = "Error: \(error.localizedDescription)"
                    } else {
                        hostLogger.info("Domain registered successfully")
                        self.status = "Active"
                    }
                }
            }
        }
    }

    func saveConfig(
        endpoint: String,
        bucket: String,
        accessKey: String,
        s3Secret: String,
        remotePrefix: String,
        deviceId: String,
        passphrase: String,
        salt: String
    ) {
        let entries: [(String, String)] = [
            ("s3_endpoint", endpoint),
            ("s3_bucket", bucket),
            ("access_key", accessKey),
            ("s3_secret", s3Secret),
            ("remote_prefix", remotePrefix),
            ("device_id", deviceId),
            ("encryption_passphrase", passphrase),
            ("encryption_salt", salt),
        ]

        for (account, value) in entries {
            let data = value.data(using: .utf8)!
            let query: [String: Any] = [
                kSecClass as String: kSecClassGenericPassword,
                kSecAttrService as String: "io.tinyland.tcfs.config",
                kSecAttrAccount as String: account,
                kSecAttrAccessGroup as String: "group.io.tinyland.tcfs",
            ]

            SecItemDelete(query as CFDictionary)

            var addQuery = query
            addQuery[kSecValueData as String] = data
            addQuery[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock

            let status = SecItemAdd(addQuery as CFDictionary, nil)
            if status != errSecSuccess {
                hostLogger.error("Keychain write failed for \(account): \(status)")
            }
        }

        checkConfiguration()
    }
}

struct ContentView: View {
    @ObservedObject var viewModel: TCFSViewModel
    @ObservedObject var authViewModel: AuthViewModel

    @State private var endpoint = ""
    @State private var bucket = ""
    @State private var accessKey = ""
    @State private var s3Secret = ""
    @State private var remotePrefix = ""
    @State private var deviceId = ""
    @State private var passphrase = ""
    @State private var salt = ""
    @State private var showingConfig = false

    /// Load current keychain values into the form fields when the sheet opens.
    private func loadKeychainIntoForm() {
        func read(_ account: String) -> String {
            let query: [String: Any] = [
                kSecClass as String: kSecClassGenericPassword,
                kSecAttrService as String: "io.tinyland.tcfs.config",
                kSecAttrAccount as String: account,
                kSecAttrAccessGroup as String: "group.io.tinyland.tcfs",
                kSecReturnData as String: true,
                kSecMatchLimit as String: kSecMatchLimitOne,
            ]
            var item: CFTypeRef?
            guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
                  let data = item as? Data,
                  let value = String(data: data, encoding: .utf8) else {
                return ""
            }
            return value
        }
        endpoint = read("s3_endpoint")
        bucket = read("s3_bucket")
        accessKey = read("access_key")
        s3Secret = read("s3_secret")
        remotePrefix = read("remote_prefix")
        deviceId = read("device_id")
        passphrase = read("encryption_passphrase")
        salt = read("encryption_salt")
    }

    var body: some View {
        NavigationView {
            Group {
            if !viewModel.isConfigured {
                // --- Onboarding: unconfigured device ---
                VStack(spacing: 24) {
                    Spacer()

                    Image(systemName: "externaldrive.badge.icloud")
                        .font(.system(size: 64))
                        .foregroundColor(.accentColor)

                    Text("Welcome to TCFS")
                        .font(.title2.bold())

                    Text("Scan an enrollment QR code from an existing device to get started.")
                        .multilineTextAlignment(.center)
                        .foregroundColor(.secondary)
                        .padding(.horizontal, 32)

                    NavigationLink {
                        QREnrollmentView(viewModel: viewModel, authViewModel: authViewModel)
                    } label: {
                        Label("Scan QR Code", systemImage: "qrcode.viewfinder")
                            .font(.headline)
                            .frame(maxWidth: .infinity)
                            .padding()
                            .background(Color.accentColor)
                            .foregroundColor(.white)
                            .cornerRadius(12)
                    }
                    .padding(.horizontal, 32)

                    Button("Configure Manually") {
                        showingConfig = true
                    }
                    .foregroundColor(.secondary)
                    .font(.subheadline)

                    Spacer()
                }
                .navigationTitle("TCFS")
            } else {
                // --- Configured: main dashboard ---
                List {
                    Section("Status") {
                        HStack {
                            Text("FileProvider")
                            Spacer()
                            Text(viewModel.status)
                                .foregroundColor(viewModel.isConfigured ? .green : .secondary)
                        }
                        HStack {
                            Text("Files Synced")
                            Spacer()
                            Text("\(viewModel.syncFileCount)")
                                .foregroundColor(.secondary)
                        }
                        if let error = viewModel.syncLastError {
                            HStack {
                                Text("Last Error")
                                Spacer()
                                Text(error)
                                    .foregroundColor(.red)
                                    .font(.caption)
                                    .lineLimit(2)
                            }
                        }
                    }

                    Section {
                        Button("Configure Credentials") {
                            showingConfig = true
                        }

                        Button("Register FileProvider Domain") {
                            viewModel.registerDomain()
                        }

                        Button("Refresh Sync Status") {
                            viewModel.refreshSyncStatus()
                        }
                    }

                    Section("Security") {
                        NavigationLink {
                            AuthView(viewModel: authViewModel, tcfsViewModel: viewModel)
                        } label: {
                            HStack {
                                Label("Authentication", systemImage: "lock.shield")
                                Spacer()
                                Text(authViewModel.authState.rawValue)
                                    .foregroundColor(.secondary)
                                    .font(.caption)
                            }
                        }
                    }

                    Section("Build") {
                        HStack {
                            Text("Version")
                            Spacer()
                            Text(Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "?")
                                .foregroundColor(.secondary)
                                .font(.system(.caption, design: .monospaced))
                        }
                        HStack {
                            Text("Build")
                            Spacer()
                            Text(Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "?")
                                .foregroundColor(.secondary)
                                .font(.system(.caption, design: .monospaced))
                        }
                        HStack {
                            Text("Commit")
                            Spacer()
                            Text(Bundle.main.infoDictionary?["GITCommitSHA"] as? String ?? "dev")
                                .foregroundColor(.secondary)
                                .font(.system(.caption, design: .monospaced))
                        }
                    }
                }
                .navigationTitle("TCFS")
            }
            } // Group
            .sheet(isPresented: $showingConfig, onDismiss: nil) {
                NavigationView {
                    Form {
                        Section("S3 Storage") {
                            TextField("Endpoint", text: $endpoint)
                                .autocapitalization(.none)
                                .disableAutocorrection(true)
                            TextField("Bucket", text: $bucket)
                                .autocapitalization(.none)
                            TextField("Access Key", text: $accessKey)
                                .autocapitalization(.none)
                            SecureField("Secret", text: $s3Secret)
                        }

                        Section("Sync") {
                            TextField("Remote Prefix", text: $remotePrefix)
                                .autocapitalization(.none)
                            TextField("Device ID", text: $deviceId)
                                .autocapitalization(.none)
                        }

                        Section("Encryption (optional)") {
                            SecureField("Passphrase", text: $passphrase)
                            TextField("Salt", text: $salt)
                                .autocapitalization(.none)
                        }

                        Button("Save") {
                            viewModel.saveConfig(
                                endpoint: endpoint,
                                bucket: bucket,
                                accessKey: accessKey,
                                s3Secret: s3Secret,
                                remotePrefix: remotePrefix,
                                deviceId: deviceId,
                                passphrase: passphrase,
                                salt: salt
                            )
                            showingConfig = false
                        }
                    }
                    .navigationTitle("Configure TCFS")
                    .navigationBarItems(trailing: Button("Cancel") {
                        showingConfig = false
                    })
                    .onAppear {
                        loadKeychainIntoForm()
                    }
                }
            }
        }
    }
}
