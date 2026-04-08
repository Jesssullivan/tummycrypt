// ConflictNotifier — posts macOS notifications when sync conflicts are detected.
//
// Monitors StatusCache for status transitions TO "conflict" and fires
// a local notification so the user knows to resolve the conflict.

import Foundation
import UserNotifications
import os.log

private let logger = Logger(
    subsystem: "io.tinyland.tcfs.findersync",
    category: "conflict-notifier"
)

class ConflictNotifier {
    private var knownConflicts: Set<String> = []

    init() {
        UNUserNotificationCenter.current().requestAuthorization(
            options: [.alert, .sound]
        ) { granted, error in
            if let error = error {
                logger.error("notification auth failed: \(error.localizedDescription)")
            } else if granted {
                logger.info("notification auth granted")
            }
        }
    }

    /// Check for new conflicts by comparing current state against known set.
    /// Call this after every StatusCache reload.
    func checkForNewConflicts(entries: [String: String]) {
        for (path, status) in entries {
            guard status == "conflict" else { continue }

            let filename = (path as NSString).lastPathComponent
            if knownConflicts.contains(path) { continue }

            knownConflicts.insert(path)
            postConflictNotification(filename: filename, path: path)
        }

        // Remove resolved conflicts from known set
        knownConflicts = knownConflicts.filter { entries[$0] == "conflict" }
    }

    private func postConflictNotification(filename: String, path: String) {
        let content = UNMutableNotificationContent()
        content.title = "Sync Conflict"
        content.body = "\(filename) was modified on another device"
        content.sound = .default
        content.categoryIdentifier = "TCFS_CONFLICT"

        let request = UNNotificationRequest(
            identifier: "tcfs-conflict-\(filename)-\(Int(Date().timeIntervalSince1970))",
            content: content,
            trigger: nil
        )

        UNUserNotificationCenter.current().add(request) { error in
            if let error = error {
                logger.error("failed to post notification: \(error.localizedDescription)")
            } else {
                logger.info("conflict notification posted for \(filename)")
            }
        }
    }
}
