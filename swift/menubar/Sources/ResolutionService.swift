import Foundation
import os.log

private let resolveLog = Logger(subsystem: "io.tinyland.tcfs.status", category: "resolve")

/// Resolution strategies matching the CLI `tcfs resolve --strategy` flag.
enum ResolutionStrategy: String, CaseIterable {
    case keepLocal = "keep-local"
    case keepRemote = "keep-remote"
    case keepBoth = "keep-both"
    case defer_ = "defer"

    var displayName: String {
        switch self {
        case .keepLocal: return "Keep Local"
        case .keepRemote: return "Keep Remote"
        case .keepBoth: return "Keep Both"
        case .defer_: return "Defer"
        }
    }

    var iconName: String {
        switch self {
        case .keepLocal: return "laptopcomputer"
        case .keepRemote: return "cloud"
        case .keepBoth: return "doc.on.doc"
        case .defer_: return "clock"
        }
    }
}

/// Resolves conflicts by shelling out to `tcfs resolve <path> --strategy <strategy>`.
///
/// This avoids pulling in a gRPC dependency — the CLI already connects to
/// the daemon over its Unix socket and sends the ResolveConflict RPC.
enum ResolutionService {

    /// Result of a resolve attempt.
    struct ResolveResult {
        let success: Bool
        let output: String
    }

    /// Find the `tcfs` binary. Checks common locations.
    private static func findBinary() -> String {
        let candidates = [
            "/usr/local/bin/tcfs",
            "/opt/homebrew/bin/tcfs",
            "\(FileManager.default.homeDirectoryForCurrentUser.path)/.cargo/bin/tcfs",
            "\(FileManager.default.homeDirectoryForCurrentUser.path)/.nix-profile/bin/tcfs",
            "/run/current-system/sw/bin/tcfs",
        ]
        for path in candidates {
            if FileManager.default.isExecutableFile(atPath: path) {
                return path
            }
        }
        // Fallback: rely on PATH
        return "tcfs"
    }

    /// Resolve a conflict asynchronously.
    ///
    /// - Parameters:
    ///   - path: Absolute path to the conflicted file (state cache key).
    ///   - strategy: Resolution strategy (keep-local, keep-remote, keep-both, defer).
    /// - Returns: Result with success flag and CLI output.
    static func resolve(path: String, strategy: ResolutionStrategy) async -> ResolveResult {
        let binary = findBinary()
        resolveLog.info("Resolving \(path) with strategy \(strategy.rawValue) via \(binary)")

        return await withCheckedContinuation { continuation in
            let process = Process()
            let pipe = Pipe()

            process.executableURL = URL(fileURLWithPath: binary)
            process.arguments = ["resolve", path, "--strategy", strategy.rawValue]
            process.standardOutput = pipe
            process.standardError = pipe

            // Inherit environment so the CLI can find the daemon socket
            process.environment = ProcessInfo.processInfo.environment

            do {
                try process.run()
                process.waitUntilExit()

                let data = pipe.fileHandleForReading.readDataToEndOfFile()
                let output = String(data: data, encoding: .utf8) ?? ""
                let success = process.terminationStatus == 0

                if success {
                    resolveLog.info("Resolved \(path): \(output)")
                } else {
                    resolveLog.error("Failed to resolve \(path) (exit \(process.terminationStatus)): \(output)")
                }

                continuation.resume(returning: ResolveResult(success: success, output: output.trimmingCharacters(in: .whitespacesAndNewlines)))
            } catch {
                resolveLog.error("Failed to launch tcfs: \(error.localizedDescription)")
                continuation.resume(returning: ResolveResult(success: false, output: error.localizedDescription))
            }
        }
    }
}
