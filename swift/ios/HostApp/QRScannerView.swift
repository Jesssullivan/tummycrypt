import SwiftUI
import AVFoundation
import os.log

private let scannerLogger = Logger(subsystem: "io.tinyland.tcfs.ios", category: "scanner")

// MARK: - QR Scanner Coordinator

class QRScannerCoordinator: NSObject, AVCaptureMetadataOutputObjectsDelegate {
    var onScan: (String) -> Void

    init(onScan: @escaping (String) -> Void) {
        self.onScan = onScan
    }

    func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        guard let object = metadataObjects.first as? AVMetadataMachineReadableCodeObject,
              object.type == .qr,
              let value = object.stringValue else {
            return
        }

        // Debounce: only fire once
        onScan(value)
    }
}

// MARK: - Camera Preview (UIViewRepresentable)

struct CameraPreview: UIViewRepresentable {
    let session: AVCaptureSession

    func makeUIView(context: Context) -> UIView {
        let view = UIView()
        let previewLayer = AVCaptureVideoPreviewLayer(session: session)
        previewLayer.videoGravity = .resizeAspectFill
        previewLayer.frame = view.bounds
        view.layer.addSublayer(previewLayer)

        DispatchQueue.main.async {
            previewLayer.frame = view.bounds
        }

        return view
    }

    func updateUIView(_ uiView: UIView, context: Context) {
        if let layer = uiView.layer.sublayers?.first as? AVCaptureVideoPreviewLayer {
            layer.frame = uiView.bounds
        }
    }
}

// MARK: - QR Scanner View

struct QRScannerView: View {
    @ObservedObject var authViewModel: AuthViewModel
    var viewModel: TCFSViewModel?
    @Environment(\.dismiss) private var dismiss

    @State private var session = AVCaptureSession()
    @State private var coordinator: QRScannerCoordinator?
    @State private var cameraPermission: CameraPermission = .unknown
    @State private var scannedData: String?
    @State private var isProcessing = false
    @State private var errorMessage: String?

    enum CameraPermission {
        case unknown, granted, denied
    }

    var body: some View {
        ZStack {
            if cameraPermission == .granted && scannedData == nil {
                CameraPreview(session: session)
                    .ignoresSafeArea()

                // Overlay with scan guide
                VStack {
                    Spacer()
                    RoundedRectangle(cornerRadius: 12)
                        .stroke(Color.white, lineWidth: 2)
                        .frame(width: 250, height: 250)
                    Spacer()
                    Text("Point camera at enrollment QR code")
                        .foregroundColor(.white)
                        .padding()
                        .background(Color.black.opacity(0.6))
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
            } else if let data = scannedData {
                VStack(spacing: 16) {
                    if isProcessing {
                        ProgressView("Processing invite...")
                    } else if let error = errorMessage {
                        Image(systemName: "xmark.circle")
                            .font(.system(size: 48))
                            .foregroundColor(.red)
                        Text("Enrollment Failed")
                            .font(.headline)
                        Text(error)
                            .foregroundColor(.secondary)
                            .multilineTextAlignment(.center)
                        Button("Try Again") {
                            scannedData = nil
                            errorMessage = nil
                            startScanning()
                        }
                    } else {
                        Image(systemName: "checkmark.circle")
                            .font(.system(size: 48))
                            .foregroundColor(.green)
                        Text("Device Enrolled")
                            .font(.headline)
                        Button("Done") {
                            dismiss()
                        }
                    }
                }
                .padding()
            } else {
                ProgressView("Starting camera...")
            }
        }
        .navigationTitle("Scan QR Code")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            checkCameraPermission()
        }
        .onDisappear {
            stopScanning()
        }
    }

    // MARK: - Camera Management

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
                scannerLogger.error("Failed to create camera input")
                return
            }

            let output = AVCaptureMetadataOutput()
            let scanCoordinator = QRScannerCoordinator { [self] value in
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
            scannerLogger.info("QR scanner started")
        }
    }

    private func stopScanning() {
        if session.isRunning {
            session.stopRunning()
        }
    }

    private func handleScan(_ value: String) {
        // Only process once
        guard scannedData == nil else { return }

        stopScanning()
        scannedData = value
        isProcessing = true

        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        scannerLogger.info("QR scanned: \(trimmed.prefix(30))...")

        // Route based on payload type to avoid sending raw JSON to base64 decoder.
        // Bootstrap configs (raw JSON or tcfs://bootstrap deep links) must be parsed
        // as BootstrapConfig, not as enrollment invites.

        // 1. Bootstrap deep link
        if trimmed.hasPrefix("tcfs://bootstrap") {
            handleBootstrapPayload(trimmed)
            return
        }

        // 2. Raw JSON — try bootstrap config parse first
        if trimmed.hasPrefix("{") || trimmed.hasPrefix("[") {
            if let config = BootstrapConfig.parse(trimmed) {
                handleBootstrapConfig(config)
            } else {
                DispatchQueue.main.async {
                    self.isProcessing = false
                    self.errorMessage = "QR contains JSON but is not a valid TCFS bootstrap config or enrollment invite.\n\nExpected fields: s3_endpoint, s3_bucket, access_key, s3_secret."
                }
            }
            return
        }

        // 3. Enrollment invite deep link
        if trimmed.hasPrefix("tcfs://enroll?data=") {
            let inviteData = String(trimmed.dropFirst("tcfs://enroll?data=".count))
            processEnrollmentInvite(inviteData)
            return
        }

        // 4. Opaque string — could be base64 enrollment invite or base64 bootstrap config.
        //    Try bootstrap first (non-destructive), then enrollment invite.
        if let config = BootstrapConfig.parse(trimmed) {
            handleBootstrapConfig(config)
            return
        }

        // Fall through to enrollment invite processing
        processEnrollmentInvite(trimmed)
    }

    private func handleBootstrapPayload(_ value: String) {
        if let config = BootstrapConfig.parse(value) {
            handleBootstrapConfig(config)
        } else {
            DispatchQueue.main.async {
                self.isProcessing = false
                self.errorMessage = "Invalid bootstrap QR code. Could not decode configuration data."
            }
        }
    }

    private func handleBootstrapConfig(_ config: BootstrapConfig) {
        guard let vm = viewModel else {
            scannerLogger.warning("Bootstrap config scanned but no TCFSViewModel available — use the enrollment screen instead")
            DispatchQueue.main.async {
                self.isProcessing = false
                self.errorMessage = "This is a bootstrap QR code. Please use the enrollment screen on the main page to scan it."
            }
            return
        }

        let deviceId = config.device_id ?? "ios-\(UIDevice.current.name.lowercased().replacingOccurrences(of: " ", with: "-"))"

        vm.saveConfig(
            endpoint: config.s3_endpoint,
            bucket: config.s3_bucket,
            accessKey: config.access_key,
            s3Secret: config.s3_secret,
            remotePrefix: config.remote_prefix ?? "default",
            deviceId: deviceId,
            passphrase: config.encryption_passphrase ?? "",
            salt: config.encryption_salt ?? ""
        )

        scannerLogger.info("Bootstrap config saved via scanner (endpoint=\(config.s3_endpoint))")

        DispatchQueue.main.async {
            self.isProcessing = false
            self.errorMessage = nil
        }
    }

    private func processEnrollmentInvite(_ inviteData: String) {
        authViewModel.processInviteData(inviteData)

        // Check result after a brief delay for processing
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
            self.isProcessing = false
            if self.authViewModel.authState == .authenticated {
                self.errorMessage = nil
            } else {
                self.errorMessage = self.authViewModel.errorMessage ?? "Unknown enrollment error"
            }
        }
    }
}
