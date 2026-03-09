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

        scannerLogger.info("QR scanned: \(value.prefix(30))...")

        // Extract invite data from deep link or raw base64
        let inviteData: String
        if value.hasPrefix("tcfs://enroll?data=") {
            inviteData = String(value.dropFirst("tcfs://enroll?data=".count))
        } else {
            inviteData = value
        }

        authViewModel.processInviteData(inviteData)

        // Check result after a brief delay for processing
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
            isProcessing = false
            if authViewModel.authState == .authenticated {
                errorMessage = nil
            } else {
                errorMessage = authViewModel.errorMessage ?? "Unknown enrollment error"
            }
        }
    }
}
