import SwiftUI
import AppKit

struct MenuBarView: View {
    // Daemon lifecycle is managed by the app — never by the user. The menu
    // shows live connection state from StateMonitor and exposes only
    // semantic toggles (pause = temporarily mute, not kill the daemon).
    @ObservedObject private var stateMonitor = StateMonitor.shared
    @Environment(\.openWindow) private var openWindow

    private var statusText: String {
        if !stateMonitor.isDaemonConnected {
            return stateMonitor.isDaemonDegraded ? "Reconnecting…" : "Connecting…"
        }
        if stateMonitor.isPaused { return "Paused" }
        return "Listening"
    }

    private var statusColor: Color {
        if !stateMonitor.isDaemonConnected { return .orange }
        if stateMonitor.isPaused { return .gray }
        return .green
    }

    private var statusIcon: String {
        if !stateMonitor.isDaemonConnected { return "exclamationmark.triangle.fill" }
        if stateMonitor.isPaused { return "pause.circle.fill" }
        return "mic.fill"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Image(systemName: statusIcon)
                    .foregroundColor(statusColor)
                Text(statusText)
                    .font(.headline)
                Spacer()
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)

            Divider()

            Button(action: { stateMonitor.togglePause() }) {
                Label(
                    stateMonitor.isPaused ? "Resume" : "Pause",
                    systemImage: stateMonitor.isPaused ? "play.circle" : "pause.circle"
                )
            }
            .disabled(!stateMonitor.isDaemonConnected)
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)

            Button(action: openSettings) {
                Label("Settings", systemImage: "gear")
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)

            Button(action: openTodaysLog) {
                Label("Open Today's Log", systemImage: "doc.text.magnifyingglass")
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)

            Divider()

            Button("Quit Always") {
                AppDelegate.killStaleDaemon()
                NSApplication.shared.terminate(nil)
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
        }
        .frame(width: 220)
    }

    private func openSettings() {
        openWindow(id: "settings")

        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
            NSApp.activate(ignoringOtherApps: true)
            if let window = NSApp.windows.first(where: { $0.title == "Always Settings" }) {
                window.makeKeyAndOrderFront(nil)
                window.orderFrontRegardless()
            }
        }
    }

    /// Open today's daemon log file in the default app (usually Console.app
    /// for plain text). Path: ~/Library/Logs/Always/always.YYYY-MM-DD
    private func openTodaysLog() {
        let formatter = DateFormatter()
        formatter.dateFormat = "yyyy-MM-dd"
        formatter.locale = Locale(identifier: "en_US_POSIX")
        let dateString = formatter.string(from: Date())

        let logURL = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library")
            .appendingPathComponent("Logs")
            .appendingPathComponent("Always")
            .appendingPathComponent("always.\(dateString)")

        if FileManager.default.fileExists(atPath: logURL.path) {
            NSWorkspace.shared.open(logURL)
        } else {
            // No log yet for today — reveal the parent directory instead.
            let logsDir = logURL.deletingLastPathComponent()
            NSWorkspace.shared.open(logsDir)
        }
    }
}
