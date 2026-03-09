import SwiftUI
import LocalAuthentication
import os.log

private let authLogger = Logger(subsystem: "io.tinyland.tcfs.ios", category: "auth")

// MARK: - Auth View Model

class AuthViewModel: ObservableObject {
    @Published var authState: AuthState = .locked
    @Published var totpEnrolled: Bool = false
    @Published var biometricAvailable: Bool = false
    @Published var biometricType: LABiometryType = .none
    @Published var errorMessage: String?
    @Published var totpSecret: String?
    @Published var totpURI: String?
    @Published var enrollmentCode: String = ""
    @Published var verifyCode: String = ""
    @Published var isLoading: Bool = false

    enum AuthState: String {
        case locked = "Locked"
        case biometricPrompt = "Authenticating..."
        case totpPrompt = "Enter TOTP Code"
        case authenticated = "Authenticated"
        case enrolling = "Enrolling..."
    }

    private let keychainService = "io.tinyland.tcfs.auth"
    private let keychainGroup = "group.io.tinyland.tcfs"

    init() {
        checkBiometricAvailability()
        checkTOTPEnrollment()
    }

    // MARK: - Biometric Authentication

    func checkBiometricAvailability() {
        let context = LAContext()
        var error: NSError?
        biometricAvailable = context.canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: &error)
        biometricType = context.biometryType

        if let error = error {
            authLogger.warning("Biometric check: \(error.localizedDescription)")
        }
    }

    func authenticateWithBiometric() {
        let context = LAContext()
        context.localizedReason = "Unlock TCFS encryption"
        authState = .biometricPrompt
        isLoading = true

        context.evaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, localizedReason: "Unlock TCFS encryption") { [weak self] success, error in
            DispatchQueue.main.async {
                self?.isLoading = false
                if success {
                    authLogger.info("Biometric auth succeeded")
                    self?.authState = .authenticated
                    self?.loadMasterKeyFromKeychain()
                } else {
                    authLogger.error("Biometric auth failed: \(error?.localizedDescription ?? "unknown")")
                    self?.errorMessage = error?.localizedDescription ?? "Biometric authentication failed"
                    self?.authState = .locked
                }
            }
        }
    }

    // MARK: - TOTP Enrollment

    func checkTOTPEnrollment() {
        totpEnrolled = readKeychain("totp_enrolled") == "true"
    }

    func enrollTOTP() {
        authState = .enrolling
        isLoading = true
        errorMessage = nil

        // In production, this calls the daemon's AuthEnroll RPC.
        // For now, store enrollment state locally for the UI flow.
        // The actual TOTP secret is managed by the daemon.
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            // TODO: Call daemon AuthEnroll via UniFFI bridge
            // let result = try provider.authEnroll(method: "totp")
            // self.totpSecret = result.secret
            // self.totpURI = result.qrUri

            DispatchQueue.main.async {
                self?.isLoading = false
                self?.authState = .totpPrompt
                // Placeholder until UniFFI bridge is wired
                self?.totpURI = "otpauth://totp/TCFS?secret=PLACEHOLDER&issuer=TummyCrypt"
                self?.totpSecret = "PLACEHOLDER"
            }
        }
    }

    func verifyTOTP() {
        guard verifyCode.count == 6 else {
            errorMessage = "Enter a 6-digit code"
            return
        }

        isLoading = true
        errorMessage = nil

        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self = self else { return }
            // TODO: Call daemon AuthVerify via UniFFI bridge
            // let result = try provider.authVerify(code: self.verifyCode)

            DispatchQueue.main.async {
                self.isLoading = false
                // For now, mark as enrolled after first verify
                self.saveKeychain("totp_enrolled", value: "true")
                self.totpEnrolled = true
                self.authState = .authenticated
                authLogger.info("TOTP verification completed")
            }
        }
    }

    // MARK: - QR Code Enrollment (device invite)

    func processInviteData(_ data: String) {
        isLoading = true
        errorMessage = nil

        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            // TODO: Call daemon DeviceEnroll via UniFFI bridge
            // Decode base64 invite, send to daemon

            DispatchQueue.main.async {
                self?.isLoading = false
                authLogger.info("Invite processed: \(data.prefix(20))...")
            }
        }
    }

    // MARK: - Master Key Management

    private func loadMasterKeyFromKeychain() {
        // After biometric auth succeeds, load the master key from Keychain
        // and send it to the daemon for encryption unlock
        if let _ = readKeychain("master_key") {
            authLogger.info("Master key loaded from Keychain after biometric auth")
            // TODO: Send to daemon via UniFFI bridge
        } else {
            authLogger.warning("No master key in Keychain — encryption not configured")
        }
    }

    func lock() {
        authState = .locked
        errorMessage = nil
        authLogger.info("Session locked")
    }

    // MARK: - Keychain Helpers

    private func readKeychain(_ account: String) -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: keychainService,
            kSecAttrAccount as String: account,
            kSecAttrAccessGroup as String: keychainGroup,
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

    private func saveKeychain(_ account: String, value: String) {
        let data = value.data(using: .utf8)!
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: keychainService,
            kSecAttrAccount as String: account,
            kSecAttrAccessGroup as String: keychainGroup,
        ]

        SecItemDelete(query as CFDictionary)

        var attrs = query
        attrs[kSecValueData as String] = data
        attrs[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly

        let status = SecItemAdd(attrs as CFDictionary, nil)
        if status != errSecSuccess {
            authLogger.error("Keychain save failed for \(account): \(status)")
        }
    }
}

// MARK: - Auth View

struct AuthView: View {
    @ObservedObject var viewModel: AuthViewModel

    var body: some View {
        List {
            // Status section
            Section("Authentication") {
                HStack {
                    Text("Status")
                    Spacer()
                    Text(viewModel.authState.rawValue)
                        .foregroundColor(statusColor)
                }

                if viewModel.biometricAvailable {
                    HStack {
                        Image(systemName: biometricIcon)
                        Text(biometricLabel)
                        Spacer()
                        Text("Available")
                            .foregroundColor(.green)
                    }
                }

                HStack {
                    Image(systemName: "lock.shield")
                    Text("TOTP")
                    Spacer()
                    Text(viewModel.totpEnrolled ? "Enrolled" : "Not enrolled")
                        .foregroundColor(viewModel.totpEnrolled ? .green : .secondary)
                }
            }

            // Actions section
            Section {
                if viewModel.authState == .locked {
                    if viewModel.biometricAvailable {
                        Button {
                            viewModel.authenticateWithBiometric()
                        } label: {
                            Label("Unlock with \(biometricLabel)", systemImage: biometricIcon)
                        }
                        .disabled(viewModel.isLoading)
                    }

                    if viewModel.totpEnrolled {
                        Button {
                            viewModel.authState = .totpPrompt
                        } label: {
                            Label("Unlock with TOTP", systemImage: "lock.shield")
                        }
                    }
                }

                if viewModel.authState == .authenticated {
                    Button(role: .destructive) {
                        viewModel.lock()
                    } label: {
                        Label("Lock Session", systemImage: "lock.fill")
                    }
                }
            }

            // TOTP verification input
            if viewModel.authState == .totpPrompt {
                Section("Enter Code") {
                    TextField("6-digit code", text: $viewModel.verifyCode)
                        .keyboardType(.numberPad)
                        .textContentType(.oneTimeCode)

                    Button("Verify") {
                        viewModel.verifyTOTP()
                    }
                    .disabled(viewModel.verifyCode.count != 6 || viewModel.isLoading)
                }
            }

            // Enrollment section
            if viewModel.authState != .authenticated {
                Section("Enrollment") {
                    if !viewModel.totpEnrolled {
                        Button {
                            viewModel.enrollTOTP()
                        } label: {
                            Label("Enroll TOTP Authenticator", systemImage: "qrcode")
                        }
                        .disabled(viewModel.isLoading)
                    }
                }
            }

            // TOTP enrollment details (shown during enrollment)
            if let uri = viewModel.totpURI, viewModel.authState == .totpPrompt {
                Section("Authenticator Setup") {
                    Text("Add this account to your authenticator app:")
                        .font(.caption)
                        .foregroundColor(.secondary)

                    if let secret = viewModel.totpSecret {
                        HStack {
                            Text("Secret")
                            Spacer()
                            Text(secret)
                                .font(.system(.caption, design: .monospaced))
                                .foregroundColor(.secondary)
                        }
                    }

                    Text(uri)
                        .font(.system(.caption2, design: .monospaced))
                        .foregroundColor(.secondary)
                        .lineLimit(3)

                    Button {
                        UIPasteboard.general.string = uri
                    } label: {
                        Label("Copy URI", systemImage: "doc.on.doc")
                    }
                }
            }

            // Error display
            if let error = viewModel.errorMessage {
                Section {
                    Text(error)
                        .foregroundColor(.red)
                        .font(.caption)
                }
            }
        }
        .navigationTitle("Authentication")
        .overlay {
            if viewModel.isLoading {
                ProgressView()
                    .scaleEffect(1.5)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(Color.black.opacity(0.1))
            }
        }
    }

    // MARK: - Computed Properties

    private var statusColor: Color {
        switch viewModel.authState {
        case .authenticated: return .green
        case .locked: return .red
        default: return .orange
        }
    }

    private var biometricIcon: String {
        switch viewModel.biometricType {
        case .faceID: return "faceid"
        case .touchID: return "touchid"
        case .opticID: return "opticid"
        default: return "person.badge.key"
        }
    }

    private var biometricLabel: String {
        switch viewModel.biometricType {
        case .faceID: return "Face ID"
        case .touchID: return "Touch ID"
        case .opticID: return "Optic ID"
        default: return "Biometric"
        }
    }
}
