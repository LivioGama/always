import SwiftUI
import AppKit

struct MenuBarView: View {
    // Daemon lifecycle is managed by the app — never by the user. The menu
    // shows live connection state from StateMonitor and exposes only
    // semantic toggles (pause = temporarily mute, not kill the daemon).
    @ObservedObject private var stateMonitor = StateMonitor.shared
    @ObservedObject private var updateService = UpdateService.shared
    @ObservedObject private var corrections = CorrectionsCenter.shared
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

            // Glossary corrections section. The submenu is always shown
            // (so the badge count is visible at a glance); when empty
            // it's disabled with a "No pending corrections" placeholder
            // so users don't fall into an empty submenu by accident.
            correctionsMenu

            Button(action: { corrections.captureNow() }) {
                Label("Capture Selection Now", systemImage: "scope")
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .disabled(!stateMonitor.isDaemonConnected)
            .help("Diff your current selection against the last paste and add the correction to the glossary.")

            // Inline toast (visible only when a correction was just
            // logged). Lives inside the menu — that's where the user is
            // already looking when they open the menu-extra; a separate
            // floating popover would be more code for similar value.
            if let logged = corrections.lastLogged {
                CorrectionToast(logged: logged)
            }

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
                NSApplication.shared.terminate(nil)
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
        }
        .frame(width: 220)
    }

    /// Submenu that lists every pending correction with Approve/Reject
    /// buttons. Each entry uses the daemon-issued UUID as `id` so the
    /// approve/reject calls round-trip cleanly even when wrong/right
    /// strings collide across multiple candidates.
    @ViewBuilder
    private var correctionsMenu: some View {
        if corrections.pending.isEmpty {
            // Disabled placeholder so the menu structure is stable —
            // users always know where this section lives.
            Label("No pending corrections", systemImage: "checkmark.seal")
                .foregroundStyle(.secondary)
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
        } else {
            Menu("Pending Corrections (\(corrections.pending.count))") {
                ForEach(corrections.pending) { item in
                    Menu("\(item.wrong) → \(item.right)") {
                        Button("Approve") { corrections.approve(item.id) }
                        Button("Reject") { corrections.reject(item.id) }
                    }
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
        }
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
        // (e.g. "AlwaysApp.app") don't break the AppleScript-driven
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

/// Inline confirmation banner shown after the daemon auto-applies a
/// correction. Self-dismissing after 2.5s — same duration as a
/// StatusOverlay flash, for consistency with other transient feedback.
///
/// Why inline (not a separate window/popover): the menu-extra is the
/// natural surface where the user already sees pending corrections, so
/// confirming a logged one in the same surface keeps the cognitive
/// model coherent. A floating window would also fight macOS focus
/// rules when triggered from a menu open.
private struct CorrectionToast: View {
    let logged: CorrectionsCenter.Logged
    @State private var visible: Bool = true

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "checkmark.circle.fill")
                .foregroundStyle(.green)
            Text("Saved: \(logged.wrong) → \(logged.right)")
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer()
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .opacity(visible ? 1 : 0)
        .animation(.easeOut(duration: 0.25), value: visible)
        // Re-fire the auto-hide timer whenever a fresh correction
        // arrives during an open menu (cheap; menu views recreate on
        // each open anyway).
        .task(id: logged) {
            try? await Task.sleep(nanoseconds: 2_500_000_000)
            visible = false
            // Small grace period so the fade animation finishes before
            // we clear the published state and the row collapses.
            try? await Task.sleep(nanoseconds: 250_000_000)
            CorrectionsCenter.shared.dismissLastLogged()
        }
    }
}
