import SwiftUI
import FileProvider
import Security
import os.log

private let hostLogger = Logger(subsystem: "io.tinyland.tcfs.ios", category: "host")

@main
struct TCFSApp: App {
    @StateObject private var viewModel = TCFSViewModel()

    var body: some Scene {
        WindowGroup {
            ContentView(viewModel: viewModel)
        }
    }
}

class TCFSViewModel: ObservableObject {
    @Published var status: String = "Not configured"
    @Published var isConfigured: Bool = false

    private let domain = NSFileProviderDomain(
        identifier: NSFileProviderDomainIdentifier("io.tinyland.tcfs"),
        displayName: "TCFS"
    )

    init() {
        checkConfiguration()
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

    @State private var endpoint = ""
    @State private var bucket = ""
    @State private var accessKey = ""
    @State private var s3Secret = ""
    @State private var remotePrefix = ""
    @State private var deviceId = ""
    @State private var passphrase = ""
    @State private var salt = ""
    @State private var showingConfig = false

    var body: some View {
        NavigationView {
            List {
                Section("Status") {
                    HStack {
                        Text("FileProvider")
                        Spacer()
                        Text(viewModel.status)
                            .foregroundColor(viewModel.isConfigured ? .green : .secondary)
                    }
                }

                Section {
                    Button("Configure Credentials") {
                        showingConfig = true
                    }

                    Button("Register FileProvider Domain") {
                        viewModel.registerDomain()
                    }
                    .disabled(!viewModel.isConfigured)
                }
            }
            .navigationTitle("TCFS")
            .sheet(isPresented: $showingConfig) {
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
                }
            }
        }
    }
}
