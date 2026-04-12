import SwiftUI

/// TCFSStatus — menu bar helper for TCFS sync conflict monitoring.
///
/// Sits in the macOS menu bar showing sync status. When conflicts are
/// detected (via polling the daemon's state cache JSON), the icon changes
/// and macOS notifications are posted. Users can resolve conflicts
/// directly from the popover without touching the terminal.
@main
struct TCFSStatusApp: App {
    @State private var monitor = ConflictMonitor()

    var body: some Scene {
        MenuBarExtra {
            MenuBarView(monitor: monitor)
        } label: {
            Image(systemName: monitor.hasConflicts
                  ? "exclamationmark.icloud"
                  : "checkmark.icloud")
        }
        .menuBarExtraStyle(.window)
    }

    init() {
        // Defer start to next run loop tick so Timer scheduling works
        DispatchQueue.main.async { [monitor] in
            monitor.start()
        }
    }
}
