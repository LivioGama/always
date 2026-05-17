import SwiftUI
import AppKit

struct MenuBarView: View {
    // Daemon lifecycle is managed by the app — never by the user. The menu
    // shows live connection state from StateMonitor and exposes only
    // semantic toggles (pause = temporarily mute, not kill the daemon).
    @ObservedObject private var stateMonitor = StateMonitor.shared
    @ObservedObject private var updateService = UpdateService.shared
    @ObservedObject private var focusedApp = FocusedAppMonitor.shared
    @Environment(\.openWindow) private var openWindow

    /// Top-line status text. When effectively paused, surface the
    /// focused app so the user can answer "why am I paused?" without
    /// opening Settings.
    private var statusText: String {
        if !stateMonitor.isDaemonConnected {
            return stateMonitor.isDaemonDegraded ? "Reconnecting…" : "Connecting…"
        }
        if stateMonitor.isPaused {
            if let name = focusedApp.currentAppName, !name.isEmpty {
                return "Paused for \(name)"
            }
            return "Paused"
        }
        if stateMonitor.isTranscribing { return "Transcribing" }
        if let name = focusedApp.currentAppName, !name.isEmpty {
            return "Active in \(name)"
        }
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

    /// Whether the focused app is on the user's resumed allowlist
    /// (override has `paused: false`).
    private var focusedAppIsResumed: Bool {
        guard let bundle = focusedApp.currentBundleId else { return false }
        return stateMonitor.resumedBundleIds.contains(bundle)
    }

    /// Label for the per-app toggle row. Distinguishes between
    /// "no app focused", "this app is on the allowlist", "this app
    /// would resume if you add it".
    private var appToggleLabel: String {
        guard let name = focusedApp.currentAppName, !name.isEmpty else {
            return "No focused app"
        }
        return focusedAppIsResumed
            ? "Remove \(name) from allowlist"
            : "Resume Always for \(name)"
    }

    private var appToggleIcon: String {
        focusedAppIsResumed ? "minus.circle" : "checkmark.circle"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: statusIcon)
                    .foregroundColor(statusColor)
                Text(statusText)
                    .font(.headline)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)

            Divider()

            // The master pause toggle lives in Settings → Voice Typing
            // Allowlist now. Keeping the menu bar focused on the
            // most-common action: opt the focused app in / out of the
            // allowlist.
            MenuRow(
                label: appToggleLabel,
                systemImage: appToggleIcon,
                isDisabled: !stateMonitor.isDaemonConnected
                    || focusedApp.currentBundleId == nil,
                action: { toggleFocusedAppAllowlist() }
            )

            Divider()

            MenuRow(
                label: "Settings",
                systemImage: "gear",
                isDisabled: false,
                action: openSettings
            )

            MenuRow(
                label: "Open Today's Log",
                systemImage: "doc.text.magnifyingglass",
                isDisabled: false,
                action: openTodaysLog
            )

            Divider()

            MenuRow(
                label: "Check for Updates…",
                systemImage: "arrow.down.circle",
                isDisabled: !updateService.canCheckForUpdates,
                action: { updateService.checkForUpdates() }
            )

            Divider()

            MenuRow(
                label: "Quit Always",
                systemImage: "power",
                isDisabled: false,
                action: {
                    AppDelegate.killStaleDaemon()
                    AppDelegate.userInitiatedQuit = true
                    NSApplication.shared.terminate(nil)
                }
            )
        }
        .padding(.vertical, 4)
        .frame(width: 260)
    }

    private func toggleFocusedAppAllowlist() {
        guard let bundle = focusedApp.currentBundleId else { return }
        // On the allowlist → remove (paused: nil falls back to default-paused).
        // Off the allowlist → add as resumed.
        let newPaused: Bool? = focusedAppIsResumed ? nil : false
        stateMonitor.setAppPaused(bundleId: bundle, paused: newPaused)
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
    /// The binary is named `always-daemon` (not `always`) — macOS APFS is
    /// case-insensitive by default and the GUI binary at `MacOS/Always`
    /// would silently overwrite a `MacOS/always` file at bundle time.
    /// Falls back to `always` on PATH for `swift run` development builds
    /// where the binary lives elsewhere.
    private static func bundledDaemonPath() -> String {
        let bundled = Bundle.main.bundleURL
            .appendingPathComponent("Contents")
            .appendingPathComponent("MacOS")
            .appendingPathComponent("always-daemon")
            .path
        return FileManager.default.fileExists(atPath: bundled) ? bundled : "always"
    }
}

/// Native-feeling menu row: the entire width (icon + label + trailing
/// whitespace) is clickable, not just the Label glyph. Replicates
/// AppKit `NSMenuItem` ergonomics inside an `NSPopover`-hosted SwiftUI
/// view — the previous `Button(...) { Label }` rows registered clicks
/// only on the visible label area, which felt broken to anyone used to
/// system menus.
private struct MenuRow: View {
    let label: String
    let systemImage: String
    let isDisabled: Bool
    let action: () -> Void

    @State private var isHovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Image(systemName: systemImage)
                    .frame(width: 16, alignment: .center)
                Text(label)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: 0)
            }
            // Stretch to the full popover width and make the hit area
            // include the trailing whitespace. `.contentShape` is the
            // missing piece — without it SwiftUI Buttons only respond
            // to clicks landing on the rendered HStack contents.
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .contentShape(Rectangle())
            .background(
                isHovering && !isDisabled
                    ? Color.accentColor.opacity(0.18)
                    : Color.clear
            )
            .foregroundColor(isDisabled ? .secondary : .primary)
        }
        .buttonStyle(.plain)
        .disabled(isDisabled)
        .onHover { hovering in
            isHovering = hovering && !isDisabled
        }
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
