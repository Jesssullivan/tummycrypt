import Foundation
import Observation
import os.log

private let monitorLog = Logger(subsystem: "io.tinyland.tcfs.status", category: "monitor")

/// Polls the daemon's state cache JSON file for conflict entries.
///
/// The state cache is a `HashMap<String, SyncState>` serialized as JSON by
/// `tcfs-sync`. We read it every 3 seconds, filter for `status == "conflict"`,
/// and publish the results for the SwiftUI view layer.
@Observable
final class ConflictMonitor {

    // MARK: - Published state

    var conflicts: [ConflictEntry] = []
    var totalFiles: Int = 0
    var syncedCount: Int = 0
    var lastError: String?

    var hasConflicts: Bool { !conflicts.isEmpty }

    // MARK: - Private

    private var timer: Timer?
    private var previousConflictIds: Set<String> = []
    private let notifications = NotificationManager()
    private let statePath: String

    init() {
        // Resolve state cache path: default matches Rust SyncConfig::default()
        // The JSON file is at {state_db}.json
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let configPath = "\(home)/.config/tcfs/config.toml"

        var resolved = "\(home)/.local/share/tcfsd/state.db.json"
        if let toml = try? String(contentsOfFile: configPath, encoding: .utf8) {
            // Simple TOML parse: find state_db = "..." line
            for line in toml.split(separator: "\n") {
                let trimmed = line.trimmingCharacters(in: .whitespaces)
                if trimmed.hasPrefix("state_db") {
                    if let quote1 = trimmed.firstIndex(of: "\""),
                       let quote2 = trimmed[trimmed.index(after: quote1)...].firstIndex(of: "\"") {
                        let value = String(trimmed[trimmed.index(after: quote1)..<quote2])
                        let expanded = value.replacingOccurrences(of: "~", with: home)
                        resolved = expanded.hasSuffix(".json") ? expanded : "\(expanded).json"
                    }
                }
            }
        }

        self.statePath = resolved
        monitorLog.info("State cache path: \(self.statePath)")
    }

    // MARK: - Lifecycle

    func start() {
        guard timer == nil else { return }
        notifications.requestAuthorization()
        poll() // immediate first read
        timer = Timer.scheduledTimer(withTimeInterval: 3.0, repeats: true) { [weak self] _ in
            self?.poll()
        }
        monitorLog.info("ConflictMonitor started (3s interval)")
    }

    func stop() {
        timer?.invalidate()
        timer = nil
    }

    // MARK: - Polling

    private func poll() {
        guard let data = FileManager.default.contents(atPath: statePath) else {
            // File doesn't exist yet — daemon may not have started
            lastError = nil
            conflicts = []
            totalFiles = 0
            syncedCount = 0
            return
        }

        do {
            let cache = try JSONDecoder().decode([String: SyncState].self, from: data)
            lastError = nil
            totalFiles = cache.count
            syncedCount = cache.values.filter { $0.status == "synced" }.count

            let newConflicts: [ConflictEntry] = cache.compactMap { (path, state) in
                guard state.status == "conflict", let info = state.conflict else { return nil }
                let filename = (path as NSString).lastPathComponent
                return ConflictEntry(id: path, filename: filename, info: info)
            }.sorted { $0.info.detectedAt > $1.info.detectedAt }

            // Detect newly appeared conflicts for notifications
            let newIds = Set(newConflicts.map(\.id))
            let fresh = newIds.subtracting(previousConflictIds)
            for entry in newConflicts where fresh.contains(entry.id) {
                notifications.postConflict(entry: entry)
            }
            previousConflictIds = newIds

            conflicts = newConflicts
        } catch {
            monitorLog.error("Failed to decode state cache: \(error.localizedDescription)")
            lastError = error.localizedDescription
        }
    }
}
