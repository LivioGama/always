import SwiftUI
import AppKit

struct MenuBarView: View {
    // Daemon lifecycle is managed by the app — never by the user. The menu
    // shows live connection state from StateMonitor and exposes only
    // semantic toggles (pause = temporarily mute, not kill the daemon).
    @ObservedObject private var stateMonitor = StateMonitor.shared
    @ObservedObject private var updateService = UpdateService.shared
    @Environment(\.openWindow) private var openWindow

    private var statusText: String {
        if !stateMonitor.isDaemonConnected {
            return stateMonitor.isDaemonDegraded ? "Reconnecting…" : "Connecting…"
        }
        if stateMonitor.isPaused { return "Paused" }
        if stateMonitor.isTranscribing { return "Transcribing" }
        return "Listening"
    }

    private var statusColor: Color {
        if !stateMonitor.isDaemonConnected { return .orange }
        if stateMonitor.isPaused { return .gray }
        if stateMonitor.isTranscribing { return .blue }
        return .green
    }

    private var statusIcon: String {
        StatusIconResolver.symbolName(
            isConnected: stateMonitor.isDaemonConnected,
            isDegraded: stateMonitor.isDaemonDegraded,
            isPaused: stateMonitor.isPaused,
            isTranscribing: stateMonitor.isTranscribing
        )
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

            Button(action: { updateService.checkForUpdates() }) {
                Label("Check for Updates…", systemImage: "arrow.down.circle")
            }
            .disabled(!updateService.canCheckForUpdates)
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)

            Divider()

            Button("Quit Always") {
                AppDelegate.killStaleDaemon()
                AppDelegate.userInitiatedQuit = true
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

    /// Open Terminal.app and stream today's daemon log with the bundled
    /// CLI's pretty renderer. The previous implementation tried to hand
    /// the raw JSON file to LaunchServices which usually picked Xcode or
    /// nothing at all; running `always logs --pretty` in a real terminal
    /// is what users actually want.
    private func openTodaysLog() {
        let alwaysCLI = Self.bundledDaemonPath()
        // Use single quotes around the path so spaces in the bundle path
        // (e.g. "Always.app") don't break the AppleScript-driven
        // shell. Escape any single quotes inside the path defensively.
        let escapedCLI = alwaysCLI.replacingOccurrences(of: "'", with: "'\\''")
        let command = "'\(escapedCLI)' logs --pretty"

        let script = """
        tell application "Terminal"
            activate
            do script "\(command)"
        end tell
        """

        if let osa = NSAppleScript(source: script) {
            var err: NSDictionary?
            osa.executeAndReturnError(&err)
            if let err {
                NSLog("openTodaysLog AppleScript failed: \(err)")
            }
        }
    }

    /// Path to the daemon CLI bundled inside this app's Contents/MacOS.
    /// Falls back to `always` on PATH for `swift run` development builds
    /// where the binary lives elsewhere.
    private static func bundledDaemonPath() -> String {
        let bundled = Bundle.main.bundleURL
            .appendingPathComponent("Contents")
            .appendingPathComponent("MacOS")
            .appendingPathComponent("always")
            .path
        return FileManager.default.fileExists(atPath: bundled) ? bundled : "always"
    }
}

/// Shared resolver for the SF Symbol that represents the current daemon
/// state. Used by both `MenuBarView` (the in-menu status row) and the
/// `AppDelegate` status-item icon so they stay visually consistent and
/// any future state additions only need one update site.
enum StatusIconResolver {
    static func symbolName(
        isConnected: Bool,
        isDegraded: Bool,
        isPaused: Bool,
        isTranscribing: Bool
    ) -> String {
        if isDegraded { return "exclamationmark.triangle.fill" }
        if !isConnected { return "exclamationmark.triangle" }
        if isPaused { return "pause.circle.fill" }
        if isTranscribing { return "waveform.circle.fill" }
        return "waveform"
    }
}
