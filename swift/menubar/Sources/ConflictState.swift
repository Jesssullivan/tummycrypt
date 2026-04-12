import Foundation

// MARK: - Codable structs mirroring Rust tcfs-sync state cache

/// Vector clock: device ID → logical timestamp.
/// Maps to `tcfs_sync::conflict::VectorClock`.
struct VectorClock: Codable, Equatable {
    var clocks: [String: UInt64]

    init(clocks: [String: UInt64] = [:]) {
        self.clocks = clocks
    }
}

/// Conflict metadata attached to a state entry when vector clocks diverge.
/// Maps to `tcfs_sync::conflict::ConflictInfo`.
struct ConflictInfo: Codable, Equatable {
    let relPath: String
    let localVclock: VectorClock
    let remoteVclock: VectorClock
    let localBlake3: String
    let remoteBlake3: String
    let localDevice: String
    let remoteDevice: String
    let detectedAt: UInt64

    enum CodingKeys: String, CodingKey {
        case relPath = "rel_path"
        case localVclock = "local_vclock"
        case remoteVclock = "remote_vclock"
        case localBlake3 = "local_blake3"
        case remoteBlake3 = "remote_blake3"
        case localDevice = "local_device"
        case remoteDevice = "remote_device"
        case detectedAt = "detected_at"
    }
}

/// Per-file sync state. Maps to `tcfs_sync::state::SyncState`.
/// Only fields needed by the menu bar app are decoded; extras are ignored.
struct SyncState: Codable {
    let blake3: String
    let size: UInt64
    let mtime: UInt64
    let chunkCount: Int
    let remotePath: String
    let lastSynced: UInt64
    let deviceId: String
    let status: String
    let conflict: ConflictInfo?

    enum CodingKeys: String, CodingKey {
        case blake3, size, mtime
        case chunkCount = "chunk_count"
        case remotePath = "remote_path"
        case lastSynced = "last_synced"
        case deviceId = "device_id"
        case status, conflict
    }
}

/// View model for a single conflict, used by MenuBarView.
struct ConflictEntry: Identifiable, Equatable {
    let id: String       // absolute local path (state cache key)
    let filename: String // last path component
    let info: ConflictInfo

    /// Human-readable age since detection (e.g. "3m ago", "2h ago").
    var age: String {
        let now = UInt64(Date().timeIntervalSince1970)
        let delta = now > info.detectedAt ? now - info.detectedAt : 0
        if delta < 60 { return "\(delta)s ago" }
        if delta < 3600 { return "\(delta / 60)m ago" }
        if delta < 86400 { return "\(delta / 3600)h ago" }
        return "\(delta / 86400)d ago"
    }
}
