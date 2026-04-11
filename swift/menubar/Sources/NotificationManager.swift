import Foundation
import UserNotifications
import os.log

private let notifLog = Logger(subsystem: "io.tinyland.tcfs.status", category: "notifications")

/// Manages macOS user notifications for sync conflicts.
///
/// Posts a notification when a new conflict is detected, with the filename
/// and involved devices. Tapping the notification brings focus to the
/// menu bar popover.
final class NotificationManager: NSObject, UNUserNotificationCenterDelegate {

    private var authorized = false

    override init() {
        super.init()
        UNUserNotificationCenter.current().delegate = self
    }

    /// Request notification permission. Called once on monitor start.
    func requestAuthorization() {
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { granted, error in
            self.authorized = granted
            if let error {
                notifLog.error("Notification auth failed: \(error.localizedDescription)")
            } else {
                notifLog.info("Notification auth granted: \(granted)")
            }
        }

        // Register "Resolve" action category
        let resolveAction = UNNotificationAction(
            identifier: "RESOLVE_ACTION",
            title: "Show Conflicts",
            options: [.foreground]
        )
        let category = UNNotificationCategory(
            identifier: "TCFS_CONFLICT",
            actions: [resolveAction],
            intentIdentifiers: []
        )
        UNUserNotificationCenter.current().setNotificationCategories([category])
    }

    /// Post a notification for a newly detected conflict.
    func postConflict(entry: ConflictEntry) {
        guard authorized else { return }

        let content = UNMutableNotificationContent()
        content.title = "Sync Conflict"
        content.body = "\(entry.filename) — \(entry.info.localDevice) vs \(entry.info.remoteDevice)"
        content.sound = .default
        content.categoryIdentifier = "TCFS_CONFLICT"
        content.userInfo = ["path": entry.id]

        let request = UNNotificationRequest(
            identifier: "conflict-\(entry.id.hashValue)",
            content: content,
            trigger: nil // deliver immediately
        )

        UNUserNotificationCenter.current().add(request) { error in
            if let error {
                notifLog.error("Failed to post notification: \(error.localizedDescription)")
            }
        }
    }

    // MARK: - UNUserNotificationCenterDelegate

    /// Handle notification tap while app is in foreground.
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        return [.banner, .sound]
    }

    /// Handle notification action tap (e.g. "Show Conflicts").
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        // The menu bar popover will be shown by the app responding to activation.
        // No additional action needed — the app comes to foreground automatically.
        notifLog.info("Notification tapped for: \(response.notification.request.content.body)")
    }
}
