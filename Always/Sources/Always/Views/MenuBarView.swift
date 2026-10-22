import SwiftUI
import AppKit

struct MenuBarView: View {
    // Daemon lifecycle is managed by the app — never by the user. The menu
    // shows live connection state from StateMonitor and exposes only
    // semantic toggles (pause = temporarily mute, not kill the daemon).
    @ObservedObject private var stateMonitor = StateMonitor.shared
    @ObservedObject private var updateService = UpdateService.shared
    @ObservedObject private var focusedApp = FocusedAppMonitor.shared

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

    /// Top-line status text. When effectively paused, surface the
    /// focused app so the user can answer "why am I paused?".
    private var statusText: String {
        if !stateMonitor.isDaemonConnected {
            return stateMonitor.isDaemonDegraded ? "Reconnecting…" : "Connecting…"
        }
        if stateMonitor.isPaused {
            if stateMonitor.isMasterPaused { return "Paused everywhere" }
            if stateMonitor.isIdleAutoPaused { return "Idle pause (speak or switch app)" }
            if let name = focusedApp.currentAppName, !name.isEmpty { return "Paused for \(name)" }
            return "Paused"
        }
        if stateMonitor.isTranscribing { return "Transcribing" }
        if let name = focusedApp.currentAppName, !name.isEmpty { return "Active in \(name)" }
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
            isTranscribing: stateMonitor.isTranscribing,
            isVoiceActive: stateMonitor.isVoiceActivity
        )
    }

    /// The single updates row: "Install update to vY…" once the daemon has
    /// announced a newer release, otherwise a plain "Check for Updates…".
    private var updateRow: (label: String, systemImage: String, isDisabled: Bool, action: () -> Void) {
        switch updateService.state {
        case .available(let offer):
            return ("Install update to v\(offer.latest)…", "arrow.down.circle.fill", false,
                    { UpdateInstallPrompt.present(for: offer) })
        case .failed where updateService.offer != nil:
            let offer = updateService.offer!
            return ("Install update to v\(offer.latest)…", "arrow.down.circle.fill",
                    !updateService.canInstall, { UpdateInstallPrompt.present(for: offer) })
        case .checking:
            return ("Checking for updates…", "arrow.down.circle", true, {})
        case .installing:
            return ("Installing update…", "arrow.down.circle", true, {})
        default:
            return ("Check for Updates…", "arrow.down.circle", !updateService.canCheck,
                    { updateService.checkNow() })
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            MenuStatusRow(
                label: statusText,
                systemImage: statusIcon,
                iconColor: statusColor
            )

            if stateMonitor.isMicUnavailable {
                Divider()

                MenuRow(
                    label: "Mic stuck — Fix",
                    systemImage: "exclamationmark.triangle.fill",
                    isDisabled: false,
                    action: { stateMonitor.fixStuckMic() }
                )
            }

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
                label: "Settings…",
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
                label: updateRow.label,
                systemImage: updateRow.systemImage,
                isDisabled: updateRow.isDisabled,
                action: updateRow.action
            )

            Divider()

            MenuRow(
                label: "Quit Always",
                systemImage: "power",
                isDisabled: false,
                action: {
                    AppDelegate.userInitiatedQuit = true
                    NSApp.terminate(nil)
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

    /// Open the Settings window served by the Tauri app (`always-tauri` +
    /// `always-gui`). The SwiftUI Settings scene is retired — macOS gets
    /// exactly one settings surface, the one the daemon's other platforms
    /// already use, so panels (Snippets, Vocabulary, Update, …) are never
    /// implemented twice.
    ///
    /// If a Tauri instance of *this* app instance (prod vs `Always Dev`)
    /// is already running we simply activate it — the settings window
    /// comes forward with the app. Otherwise the bundled binary is
    /// spawned detached; a fresh launch shows Settings by itself once
    /// onboarding is done (always-gui/src/App.tsx).
    private func openSettings() {
        if let running = Self.runningTauriApp() {
            running.activate(options: [.activateAllWindows])
            return
        }
        launchTauriSettings()
    }

    /// A running `always-tauri` app belonging to this same instance.
    /// Dev (`Always Dev.app`) and production must never focus each other's
    /// settings window — AppInstance.isSameInstance keeps the split — and
    /// binaries launched straight from a cargo `target/` dir (dev-only)
    /// are always accepted.
    private static func runningTauriApp() -> NSRunningApplication? {
        return NSWorkspace.shared.runningApplications.first { app in
            guard let url = app.executableURL,
                  url.lastPathComponent == "always-tauri" else { return false }
            guard url.path.contains(".app/Contents/MacOS/") else { return true }
            return AppInstance.isSameInstance(command: url.path)
        }
    }

    /// Spawn `always-tauri` detached and bring it forward once it has a
    /// window. Null stdio mirrors CLIService's daemon spawn: the child
    /// must outlive this view and never hold pipes back to the menu bar.
    private func launchTauriSettings() {
        guard let binary = Self.tauriBinaryPath() else {
            NSLog("openSettings: always-tauri binary not found (bundled or dev target dir)")
            return
        }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: binary)
        var env = ProcessInfo.processInfo.environment
        // Match the instance this menu bar belongs to so the Tauri app's
        // config dir / daemon plumbing never crosses the prod↔dev split.
        if AppInstance.isDev {
            env["ALWAYS_INSTANCE"] = "dev"
        } else {
            env.removeValue(forKey: "ALWAYS_INSTANCE")
        }
        process.environment = env
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice

        do {
            try process.run()
        } catch {
            NSLog("openSettings: failed to launch \(binary): \(error)")
            return
        }

        // The Tauri app registers with the WindowServer only once its first
        // window exists, so activation needs a few patient attempts rather
        // than a single fixed sleep.
        let pid = process.processIdentifier
        for delay in [0.3, 0.6, 1.0, 1.5] {
            DispatchQueue.main.asyncAfter(deadline: .now() + delay) {
                guard NSRunningApplication(processIdentifier: pid)?.isActive != true else { return }
                NSRunningApplication(processIdentifier: pid)?
                    .activate(options: [.activateAllWindows])
            }
        }
    }

    /// Path to the bundled `always-tauri` binary next to this app's
    /// executable (same `Contents/MacOS` layout CLIService uses for
    /// `always-daemon`). Development fallbacks cover `swift run` builds,
    /// where the binary lives in the crate's cargo target directory.
    private static func tauriBinaryPath() -> String? {
        let bundled = Bundle.main.bundleURL
            .appendingPathComponent("Contents")
            .appendingPathComponent("MacOS")
            .appendingPathComponent("always-tauri")
            .path
        if FileManager.default.fileExists(atPath: bundled) {
            return bundled
        }

        // Always/Sources/Always/Views → repo root, where the crate lives.
        let repoRoot = URL(fileURLWithPath: #file)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        for profile in ["debug", "release"] {
            let dev = repoRoot
                .appendingPathComponent("crates/always_tauri/target")
                .appendingPathComponent(profile)
                .appendingPathComponent("always-tauri")
                .path
            if FileManager.default.fileExists(atPath: dev) {
                return dev
            }
        }
        return nil
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

private struct MenuStatusRow: View {
    let label: String
    let systemImage: String
    let iconColor: Color

    var body: some View {
        Button(action: {}) {
            HStack(spacing: 8) {
                Image(systemName: systemImage)
                    .frame(width: 16, alignment: .center)
                    .foregroundColor(iconColor)
                Text(label)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: 0)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .contentShape(Rectangle())
            .foregroundColor(.secondary)
        }
        .buttonStyle(.plain)
        .disabled(true)
    }
}

/// Shared resolver for the SF Symbol that represents the current daemon
/// state. Used by `MenuBarView`, `MenuBarStatusLabel`, and any other
/// status surfaces so they stay visually consistent.
enum StatusIconResolver {
    static func symbolName(
        isConnected: Bool,
        isDegraded: Bool,
        isPaused: Bool,
        isTranscribing: Bool,
        isVoiceActive: Bool = false
    ) -> String {
        if isDegraded { return "exclamationmark.triangle.fill" }
        if !isConnected { return "exclamationmark.triangle" }
        if isPaused { return "pause.circle.fill" }
        if isTranscribing { return "waveform.circle.fill" }
        // Live voice (verified speech in progress): the menu bar itself
        // signals "hearing you" so the state stays visible even when the
        // status overlay is set to Compact or Hidden.
        if isVoiceActive { return "mic.fill" }
        return "waveform"
    }
}
