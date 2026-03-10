import SwiftUI
import AVFoundation
import os.log

private let enrollLogger = Logger(subsystem: "io.tinyland.tcfs.ios", category: "enrollment")

/// Bootstrap config payload for QR-based device enrollment.
///
/// Format: JSON (optionally base64-wrapped or as `tcfs://bootstrap?data=...` deep link).
///
/// ```json
/// {
///   "type": "tcfs-bootstrap",
///   "s3_endpoint": "http://212.2.245.145:8333",
///   "s3_bucket": "tcfs",
///   "access_key": "...",
///   "s3_secret": "...",
///   "remote_prefix": "default",
///   "device_id": "iphone-jess",
///   "encryption_passphrase": "...",   // optional — enables E2EE
///   "encryption_salt": "..."          // optional — Argon2id KDF salt
/// }
/// ```
struct BootstrapConfig: Codable {
    let type: String?
    let s3_endpoint: String
    let s3_bucket: String
    let access_key: String
    let s3_secret: String
    let remote_prefix: String?
    let device_id: String?
    let encryption_passphrase: String?
    let encryption_salt: String?

    /// Try to parse from a scanned QR string.
    /// Supports: raw JSON, base64-encoded JSON, or `tcfs://bootstrap?data=<base64>` deep link.
    static func parse(_ raw: String) -> BootstrapConfig? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)

        // Deep link format
        if let dataParam = extractParam(from: trimmed, prefix: "tcfs://bootstrap?data=") {
            return decodeBase64(dataParam)
        }

        // Enrollment invite deep link (has storage_endpoint but no creds — skip)
        if trimmed.hasPrefix("tcfs://enroll?") {
            return nil
        }

        // Try raw JSON first
        if let config = decodeJSON(trimmed) {
            return config
        }

        // Try base64-encoded JSON
        return decodeBase64(trimmed)
    }

    private static func extractParam(from url: String, prefix: String) -> String? {
        guard url.hasPrefix(prefix) else { return nil }
        let data = String(url.dropFirst(prefix.count))
        // Strip any additional query params
        return data.components(separatedBy: "&").first
    }

    private static func decodeJSON(_ json: String) -> BootstrapConfig? {
        guard let data = json.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(BootstrapConfig.self, from: data)
    }

    private static func decodeBase64(_ encoded: String) -> BootstrapConfig? {
        // Try URL-safe base64 first, then standard
        let candidates = [encoded, encoded.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")]
        for candidate in candidates {
            // Pad if needed
            let padded = candidate.padding(toLength: ((candidate.count + 3) / 4) * 4, withPad: "=", startingAt: 0)
            if let data = Data(base64Encoded: padded),
               let config = try? JSONDecoder().decode(BootstrapConfig.self, from: data) {
                return config
            }
        }
        return nil
    }
}


// MARK: - QR Enrollment View

struct QREnrollmentView: View {
    @ObservedObject var viewModel: TCFSViewModel
    @ObservedObject var authViewModel: AuthViewModel
    @Environment(\.dismiss) private var dismiss

    @State private var session = AVCaptureSession()
    @State private var coordinator: QRScannerCoordinator?
    @State private var cameraPermission: QRScannerView.CameraPermission = .unknown
    @State private var scannedData: String?
    @State private var isProcessing = false
    @State private var errorMessage: String?
    @State private var successConfig: BootstrapConfig?

    var body: some View {
        ZStack {
            if cameraPermission == .granted && scannedData == nil {
                CameraPreview(session: session)
                    .ignoresSafeArea()

                VStack {
                    Spacer()
                    RoundedRectangle(cornerRadius: 12)
                        .stroke(Color.white, lineWidth: 2)
                        .frame(width: 250, height: 250)
                    Spacer()

                    VStack(spacing: 8) {
                        Text("Scan enrollment QR code")
                            .foregroundColor(.white)
                            .font(.headline)
                        Text("Generate one with: tcfs device invite --qr")
                            .foregroundColor(.white.opacity(0.7))
                            .font(.caption)
                    }
                    .padding()
                    .background(Color.black.opacity(0.7))
                    .cornerRadius(8)
                    .padding(.bottom, 40)
                }
            } else if cameraPermission == .denied {
                VStack(spacing: 16) {
                    Image(systemName: "camera.fill")
                        .font(.system(size: 48))
                        .foregroundColor(.secondary)
                    Text("Camera Access Required")
                        .font(.headline)
                    Text("Enable camera access in Settings to scan enrollment QR codes.")
                        .multilineTextAlignment(.center)
                        .foregroundColor(.secondary)
                    Button("Open Settings") {
                        if let url = URL(string: UIApplication.openSettingsURLString) {
                            UIApplication.shared.open(url)
                        }
                    }
                }
                .padding()
            } else if isProcessing {
                VStack(spacing: 16) {
                    ProgressView()
                        .scaleEffect(1.5)
                    Text("Configuring device...")
                        .font(.headline)
                }
            } else if let config = successConfig {
                VStack(spacing: 20) {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 64))
                        .foregroundColor(.green)

                    Text("Device Configured!")
                        .font(.title2.bold())

                    VStack(alignment: .leading, spacing: 8) {
                        configRow("Endpoint", config.s3_endpoint)
                        configRow("Bucket", config.s3_bucket)
                        configRow("Device", config.device_id ?? "auto")
                    }
                    .padding()
                    .background(Color(.systemGray6))
                    .cornerRadius(12)
                    .padding(.horizontal)

                    Button {
                        dismiss()
                    } label: {
                        Text("Continue")
                            .font(.headline)
                            .frame(maxWidth: .infinity)
                            .padding()
                            .background(Color.accentColor)
                            .foregroundColor(.white)
                            .cornerRadius(12)
                    }
                    .padding(.horizontal, 32)
                }
            } else if let error = errorMessage {
                VStack(spacing: 16) {
                    Image(systemName: "xmark.circle")
                        .font(.system(size: 48))
                        .foregroundColor(.red)
                    Text("Invalid QR Code")
                        .font(.headline)
                    Text(error)
                        .foregroundColor(.secondary)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal)
                    Button("Try Again") {
                        scannedData = nil
                        errorMessage = nil
                        startScanning()
                    }
                }
                .padding()
            } else {
                ProgressView("Starting camera...")
            }
        }
        .navigationTitle("Enroll Device")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            checkCameraPermission()
        }
        .onDisappear {
            stopScanning()
        }
    }

    private func configRow(_ label: String, _ value: String) -> some View {
        HStack {
            Text(label)
                .foregroundColor(.secondary)
                .frame(width: 80, alignment: .leading)
            Text(value)
                .font(.system(.body, design: .monospaced))
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .font(.subheadline)
    }

    // MARK: - Camera

    private func checkCameraPermission() {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            cameraPermission = .granted
            startScanning()
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { granted in
                DispatchQueue.main.async {
                    cameraPermission = granted ? .granted : .denied
                    if granted { startScanning() }
                }
            }
        default:
            cameraPermission = .denied
        }
    }

    private func startScanning() {
        guard !session.isRunning else { return }

        DispatchQueue.global(qos: .userInitiated).async {
            guard let device = AVCaptureDevice.default(for: .video),
                  let input = try? AVCaptureDeviceInput(device: device) else {
                enrollLogger.error("Failed to create camera input")
                return
            }

            let output = AVCaptureMetadataOutput()
            let scanCoordinator = QRScannerCoordinator { value in
                handleScan(value)
            }

            session.beginConfiguration()
            if session.canAddInput(input) { session.addInput(input) }
            if session.canAddOutput(output) { session.addOutput(output) }
            output.setMetadataObjectsDelegate(scanCoordinator, queue: .main)
            output.metadataObjectTypes = [.qr]
            session.commitConfiguration()

            DispatchQueue.main.async {
                self.coordinator = scanCoordinator
            }

            session.startRunning()
            enrollLogger.info("Enrollment scanner started")
        }
    }

    private func stopScanning() {
        if session.isRunning {
            session.stopRunning()
        }
    }

    // MARK: - Handle Scan

    private func handleScan(_ value: String) {
        guard scannedData == nil else { return }

        stopScanning()
        scannedData = value
        isProcessing = true

        enrollLogger.info("QR scanned: \(value.prefix(50))...")

        guard let config = BootstrapConfig.parse(value) else {
            DispatchQueue.main.async {
                isProcessing = false
                errorMessage = "QR code is not a valid TCFS bootstrap config.\n\nExpected JSON with s3_endpoint, s3_bucket, access_key, s3_secret."
            }
            return
        }

        // Generate device ID if not provided
        let deviceId = config.device_id ?? "ios-\(UIDevice.current.name.lowercased().replacingOccurrences(of: " ", with: "-"))"

        // Write config to keychain
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

        enrollLogger.info("Bootstrap config saved to keychain (endpoint=\(config.s3_endpoint), bucket=\(config.s3_bucket))")

        DispatchQueue.main.async {
            isProcessing = false
            successConfig = config
        }
    }
}
