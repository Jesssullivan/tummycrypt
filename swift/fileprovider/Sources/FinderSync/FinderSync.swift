// TCFSFinderSync — Finder Sync Extension for file status badges.
//
// Monitors ~/Library/CloudStorage/TCFSProvider-TCFS/ and renders
// badges on files based on their sync status from the daemon's
// state cache.
//
// Badge states:
//   synced  (checkmark)  — content matches remote
//   syncing (arrows)     — actively uploading/downloading
//   locked  (lock)       — locked by another operation
//   conflict (warning)   — vector clock conflict detected

import FinderSync
import Foundation
import os.log

private let logger = Logger(
    subsystem: "io.tinyland.tcfs.findersync",
    category: "badges"
)

class TCFSFinderSync: FIFinderSync {
    // Deferred init — avoid filesystem access during extension load
    // which can deadlock with Finder's coordination locks.
    private var statusCache: StatusCache?

    override init() {
        super.init()

        // Register badge images (system symbols as placeholders)
        let controller = FIFinderSyncController.default()

        // Use SF Symbols rendered as NSImage for badges
        if let syncedImage = NSImage(systemSymbolName: "checkmark.circle.fill", accessibilityDescription: "Synced") {
            controller.setBadgeImage(syncedImage, label: "Synced", forBadgeIdentifier: "synced")
        }
        if let syncingImage = NSImage(systemSymbolName: "arrow.triangle.2.circlepath", accessibilityDescription: "Syncing") {
            controller.setBadgeImage(syncingImage, label: "Syncing", forBadgeIdentifier: "syncing")
        }
        if let lockedImage = NSImage(systemSymbolName: "lock.fill", accessibilityDescription: "Locked") {
            controller.setBadgeImage(lockedImage, label: "Locked", forBadgeIdentifier: "locked")
        }
        if let conflictImage = NSImage(systemSymbolName: "exclamationmark.triangle.fill", accessibilityDescription: "Conflict") {
            controller.setBadgeImage(conflictImage, label: "Conflict", forBadgeIdentifier: "conflict")
        }
        if let excludedImage = NSImage(systemSymbolName: "slash.circle", accessibilityDescription: "Excluded") {
            controller.setBadgeImage(excludedImage, label: "Excluded", forBadgeIdentifier: "excluded")
        }
        if let pinnedImage = NSImage(systemSymbolName: "pin.fill", accessibilityDescription: "Pinned") {
            controller.setBadgeImage(pinnedImage, label: "Pinned", forBadgeIdentifier: "pinned")
        }

        // Monitor the FPFS CloudStorage volume
        let fpfsPath = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/CloudStorage/TCFSProvider-TCFS")
        controller.directoryURLs = [fpfsPath]

        logger.info("FinderSync initialized, monitoring \(fpfsPath.path)")
    }

    // MARK: - FIFinderSync Protocol

    override func beginObservingDirectory(at url: URL) {
        logger.info("beginObserving: \(url.path)")
        // Initialize StatusCache on first observation (safe — extension fully loaded)
        if statusCache == nil {
            statusCache = StatusCache()
        }
        statusCache?.startPolling()
    }

    override func endObservingDirectory(at url: URL) {
        logger.info("endObserving: \(url.path)")
        statusCache?.stopPolling()
    }

    override func requestBadgeIdentifier(for url: URL) {
        let filename = url.lastPathComponent

        // Look up status in the daemon's state cache
        if let status = statusCache?.statusByName(filename) {
            let badgeId: String
            switch status {
            case "synced":
                badgeId = "synced"
            case "active":
                badgeId = "syncing"
            case "locked":
                badgeId = "locked"
            case "conflict":
                badgeId = "conflict"
            default:
                badgeId = "" // not_synced or unknown — no badge
            }

            if !badgeId.isEmpty {
                FIFinderSyncController.default().setBadgeIdentifier(badgeId, for: url)
            }
        }
        // If not in cache, no badge (file is a placeholder or untracked)
    }
}
