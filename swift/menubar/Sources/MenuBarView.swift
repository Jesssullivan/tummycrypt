import SwiftUI

/// Main popover content for the TCFSStatus menu bar extra.
struct MenuBarView: View {
    let monitor: ConflictMonitor
    @State private var resolvingPaths: Set<String> = []
    @State private var resolveErrors: [String: String] = [:]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            headerSection
            Divider()

            if monitor.hasConflicts {
                conflictList
            } else {
                allClearSection
            }

            Divider()
            footerSection
        }
        .frame(width: 340)
    }

    // MARK: - Header

    private var headerSection: some View {
        HStack {
            Image(systemName: monitor.hasConflicts ? "exclamationmark.triangle.fill" : "checkmark.circle.fill")
                .foregroundStyle(monitor.hasConflicts ? .yellow : .green)
                .font(.title3)

            VStack(alignment: .leading, spacing: 2) {
                Text("TCFS Sync")
                    .font(.headline)
                Text("\(monitor.syncedCount)/\(monitor.totalFiles) files synced")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Spacer()

            if monitor.hasConflicts {
                Text("\(monitor.conflicts.count)")
                    .font(.caption.bold())
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(.red.opacity(0.8))
                    .foregroundStyle(.white)
                    .clipShape(Capsule())
            }
        }
        .padding(12)
    }

    // MARK: - Conflict List

    private var conflictList: some View {
        ScrollView {
            LazyVStack(spacing: 0) {
                ForEach(monitor.conflicts) { entry in
                    conflictRow(entry)
                    if entry.id != monitor.conflicts.last?.id {
                        Divider().padding(.leading, 12)
                    }
                }
            }
        }
        .frame(maxHeight: 300)
    }

    private func conflictRow(_ entry: ConflictEntry) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            // File info
            HStack(spacing: 6) {
                Image(systemName: "doc.badge.gearshape")
                    .foregroundStyle(.orange)
                    .font(.body)
                VStack(alignment: .leading, spacing: 1) {
                    Text(entry.filename)
                        .font(.system(.body, design: .monospaced))
                        .lineLimit(1)
                    Text("\(entry.info.localDevice) vs \(entry.info.remoteDevice) · \(entry.age)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
            }

            // Error message if resolution failed
            if let error = resolveErrors[entry.id] {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .lineLimit(2)
            }

            // Resolution buttons
            if resolvingPaths.contains(entry.id) {
                HStack {
                    ProgressView()
                        .controlSize(.small)
                    Text("Resolving...")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            } else {
                HStack(spacing: 4) {
                    ForEach(ResolutionStrategy.allCases, id: \.rawValue) { strategy in
                        Button {
                            resolveConflict(entry: entry, strategy: strategy)
                        } label: {
                            Label(strategy.displayName, systemImage: strategy.iconName)
                                .font(.caption)
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    }
                }
            }
        }
        .padding(12)
    }

    // MARK: - All Clear

    private var allClearSection: some View {
        VStack(spacing: 8) {
            Image(systemName: "checkmark.icloud.fill")
                .font(.largeTitle)
                .foregroundStyle(.green)
            Text("All files in sync")
                .font(.body)
                .foregroundStyle(.secondary)
            if let error = monitor.lastError {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .multilineTextAlignment(.center)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(24)
    }

    // MARK: - Footer

    private var footerSection: some View {
        HStack {
            Button("Open TCFS Folder") {
                let url = URL(fileURLWithPath: NSHomeDirectory())
                    .appendingPathComponent("Library/CloudStorage/TCFSProvider-TCFS")
                NSWorkspace.shared.open(url)
            }
            .buttonStyle(.borderless)
            .font(.caption)

            Spacer()

            Button("Quit") {
                NSApplication.shared.terminate(nil)
            }
            .buttonStyle(.borderless)
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        .padding(12)
    }

    // MARK: - Resolution

    private func resolveConflict(entry: ConflictEntry, strategy: ResolutionStrategy) {
        resolvingPaths.insert(entry.id)
        resolveErrors.removeValue(forKey: entry.id)

        Task {
            let result = await ResolutionService.resolve(path: entry.id, strategy: strategy)
            await MainActor.run {
                resolvingPaths.remove(entry.id)
                if !result.success {
                    resolveErrors[entry.id] = result.output.isEmpty ? "Resolution failed" : result.output
                }
                // On success, the conflict will disappear on next poll cycle (3s)
            }
        }
    }
}
