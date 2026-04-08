// StatusCache — reads the tcfsd state cache JSON for badge lookups.
//
// The daemon persists sync state at ~/.local/share/tcfsd/state.db.json.
// This cache reader loads it periodically and provides O(1) lookups
// for badge requests (must respond within Finder's ~200ms timeout).

import Foundation
import os.log

private let logger = Logger(
    subsystem: "io.tinyland.tcfs.findersync",
    category: "status-cache"
)

/// Minimal representation of a SyncState entry from the daemon's JSON cache.
struct CachedSyncState: Codable {
    let blake3: String
    let size: UInt64
    let last_synced: UInt64
    let status: String?  // "synced", "active", "locked", "conflict", "not_synced"

    // Allow unknown fields (forward compat)
    enum CodingKeys: String, CodingKey {
        case blake3, size, last_synced, status
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        blake3 = try c.decode(String.self, forKey: .blake3)
        size = try c.decodeIfPresent(UInt64.self, forKey: .size) ?? 0
        last_synced = try c.decodeIfPresent(UInt64.self, forKey: .last_synced) ?? 0
        status = try c.decodeIfPresent(String.self, forKey: .status)
    }
}

/// Thread-safe status cache that periodically reloads from the daemon's JSON file.
class StatusCache {
    private let cachePath: String
    private var entries: [String: CachedSyncState] = [:]
    private let lock = NSLock()
    private var pollTimer: Timer?
    private let conflictNotifier = ConflictNotifier()

    init() {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        cachePath = "\(home)/.local/share/tcfsd/state.db.json"
        reload()
    }

    /// Reload the state cache from disk.
    func reload() {
        guard let data = try? Data(contentsOf: URL(fileURLWithPath: cachePath)) else {
            logger.warning("status cache not found at \(self.cachePath)")
            return
        }
        do {
            let decoded = try JSONDecoder().decode([String: CachedSyncState].self, from: data)
            lock.lock()
            entries = decoded
            lock.unlock()

            // Check for new conflicts and notify user
            let statusMap = decoded.compactMapValues { $0.status }
            conflictNotifier.checkForNewConflicts(entries: statusMap)
        } catch {
            logger.error("failed to parse status cache: \(error.localizedDescription)")
        }
    }

    /// Look up sync status for a file by its absolute path.
    func status(for path: String) -> String? {
        lock.lock()
        defer { lock.unlock() }
        // State cache keys are absolute local paths
        return entries[path]?.status
    }

    /// Look up sync status by filename (searches all entries).
    func statusByName(_ name: String) -> String? {
        lock.lock()
        defer { lock.unlock() }
        for (key, entry) in entries {
            if key.hasSuffix("/\(name)") || key == name {
                return entry.status
            }
        }
        return nil
    }

    /// Start periodic polling (every 2 seconds).
    func startPolling() {
        stopPolling()
        pollTimer = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] _ in
            self?.reload()
        }
    }

    /// Stop periodic polling.
    func stopPolling() {
        pollTimer?.invalidate()
        pollTimer = nil
    }
}
