import AppKit
import Foundation
import Darwin

// NSApplicationDelegate for the Swift menu-bar app.
//
// Settings is deliberately NOT a SwiftUI scene anymore: macOS Settings is
// served by the Tauri app (`always-tauri` + `always-gui`) — the single
// surface every platform already shares, so panels (Snippets, Vocabulary,
// Update, …) exist exactly once. Every "open Settings" entry point this
// delegate owns — the Dock icon (`applicationShouldHandleReopen`) and the
// `always://settings` URL — launches or focuses that app; the menu bar's
// "Settings…" item does the same from `MenuBarView`. Everything else stays
// native here and untouched: single-instance guard, daemon bootstrap and
// lifecycle, onboarding trigger, overlay/permissions warm-up, keep-alive
// window, and termination policy.

class AppDelegate: NSObject, NSApplicationDelegate {
    private var cliService: CLIService?
    private var onboardingState: OnboardingState?
    /// Set by Quit menu / Cmd+Q so termination is honored.
    static var userInitiatedQuit = false

    private var stateMonitor: StateMonitor?
    /// Invisible window that prevents SwiftUI from tearing down the
    /// process when no visible window exists (macOS 26 silent exit).
    /// With the Settings scene retired the steady state is menu-bar-only,
    /// so this window is what keeps the process alive between Settings
    /// opens.
    private var keepAliveWindow: NSWindow?
    private var daemonBootstrapTask: Task<Void, Never>?
    private let singleInstanceGuard = SingleInstanceGuard.shared

    func setOnboardingState(_ state: OnboardingState) {
        onboardingState = state
    }

    func applicationWillFinishLaunching(_ notification: Notification) {
        // Flock + process sweep — covers com.always / com.always.v2 / stale
        // Desktop copies that bypass bundle-ID checks.
        if !singleInstanceGuard.acquireOrHandOff() {
            exit(0)
        }
        installKeepAliveWindow()
        // Subscribe before the first connect so the daemon's initial
        // `UpdateAvailable` frame is not missed (the menu item needs it even
        // if Settings is never opened).
        MainActor.assumeIsolated { _ = UpdateService.shared }
        StateMonitor.shared.beginBootstrap()
        StateMonitor.shared.connectToDaemon()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        OverlayDisplayMode.registerDefault()
        cliService = CLIService()
        NSLog("Always: MenuBarExtra installed")

        // Onboarding gating: if no saved Groq API key exists, the state
        // object opens the onboarding window itself once the async key
        // check resolves (see OnboardingState.checkAndShowOnboardingIfNeeded).
        // A `forceOnboarding` UserDefaults flag bypasses the pre-existing
        // install check for explicit re-triggering.
        if UserDefaults.standard.bool(forKey: "forceOnboarding") {
            UserDefaults.standard.set(false, forKey: "forceOnboarding")
            UserDefaults.standard.synchronize()
            onboardingState?.forceOnboarding()
        } else {
            onboardingState?.checkAndShowOnboardingIfNeeded()
        }

        // Permission flow lives in `PermissionsManager`. It seeds the
        // current status (silent — no prompts) and triggers the system
        // dialog for `notDetermined` cases. The Settings UI hosts a
        // banner that surfaces "denied" / "not trusted" states and
        // links the user to System Settings. We deliberately do NOT
        // open modal alerts at launch — the user just sees the banner
        // and can act when convenient.
        //
        // First run: onboarding walks through each permission one card at
        // a time, so nothing is requested here. Firing them all at launch
        // put the microphone prompt plus TWO System Settings panes
        // (Accessibility, Input Monitoring) on top of the welcome window
        // before the user had read a word.
        let perms = PermissionsManager.shared
        if OnboardingCompletion.isComplete {
            perms.requestMicrophoneIfNeeded()
            perms.requestAccessibilityIfNeeded()
            // Input Monitoring gates the daemon's global-hotkey tap
            // (listen-only CGEventTap). Requested from the GUI so "Always"
            // appears in the list and the prompt fires — the daemon's own
            // request never surfaces it.
            perms.requestInputMonitoringIfNeeded()
        }

        let cli = cliService
        // Connect immediately — UDS retries until the socket is live. Daemon
        // cleanup/spawn runs concurrently so a healthy daemon is never killed
        // just because the GUI relaunched.
        Task { @MainActor [weak self] in
            let monitor = StateMonitor.shared
            self?.stateMonitor = monitor
            StatusOverlayController.shared.prewarm()
            AudioOutputMonitor.shared.start(stateMonitor: monitor)
            AudioInputMonitor.shared.start(stateMonitor: monitor)
            FocusedAppMonitor.shared.start(stateMonitor: monitor)
        }

        daemonBootstrapTask = Task {
            guard !Task.isCancelled else { return }
            if Self.isHealthyDaemon() {
                NSLog("Always: healthy daemon detected — attaching without restart")
                await Self.reconcileDuplicateDaemons(cli: cli)
                return
            }

            await Task.detached(priority: .userInitiated) {
                Self.killStaleDaemon()
            }.value
            guard !Task.isCancelled else { return }

            do {
                _ = try await cli?.startDaemon()
                guard !Task.isCancelled else {
                    Self.killStaleDaemon()
                    return
                }
                await Self.reconcileDuplicateDaemons(cli: cli)
            } catch {
                await MainActor.run { StateMonitor.shared.endBootstrap() }
                NSLog("Always: daemon start failed: \(error.localizedDescription)")
            }
        }
    }

    func application(_ application: NSApplication, open urls: [URL]) {
        for url in urls where url.scheme == "always" {
            if url.host == "settings" || url.path == "/settings" {
                openSettingsWindow()
            }
        }
    }

    /// 1×1 off-screen window so AppKit keeps the process alive when no
    /// window is open.
    private func installKeepAliveWindow() {
        let window = NSWindow(
            contentRect: NSRect(x: -20000, y: -20000, width: 1, height: 1),
            styleMask: .borderless,
            backing: .buffered,
            defer: false
        )
        window.isReleasedWhenClosed = false
        window.alphaValue = 0
        window.backgroundColor = .clear
        window.level = .normal
        window.collectionBehavior = [.transient, .ignoresCycle]
        window.orderBack(nil)
        keepAliveWindow = window
        NSLog("Always: keep-alive window installed")
    }

    /// Open (or focus) the Settings window — served by the Tauri app
    /// (`always-tauri` + `always-gui`), never by a SwiftUI scene here.
    /// Called from the `always://settings` URL and from
    /// `applicationShouldHandleReopen` when the user clicks the Dock icon.
    ///
    /// If an `always-tauri` belonging to *this* app instance (prod vs
    /// `Always Dev`) is already running we simply activate it — its
    /// windows come forward with the app. Otherwise the bundled binary is
    /// spawned detached; a fresh launch shows Settings (or Onboarding
    /// first, until completion is recorded) by itself.
    private func openSettingsWindow() {
        if let running = Self.runningTauriApp() {
            running.activate(options: [.activateAllWindows])
            return
        }
        launchTauriSettings()
    }

    /// A running `always-tauri` app belonging to this same instance.
    /// Dev (`Always Dev.app`) and production must never focus each other's
    /// settings window — `AppInstance.isSameInstance` keeps the split — and
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
    /// must outlive this menu-bar app and never hold pipes back to it.
    private func launchTauriSettings() {
        guard let binary = Self.tauriBinaryPath() else {
            NSLog("openSettingsWindow: always-tauri binary not found (bundled or dev target dir)")
            return
        }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: binary)
        var env = ProcessInfo.processInfo.environment
        // Match the instance this app belongs to so the Tauri app's
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
            NSLog("openSettingsWindow: failed to launch \(binary): \(error)")
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
    /// executable — the same `Contents/MacOS` layout `CLIService` uses for
    /// `always-daemon`. Development fallback covers `swift run` builds,
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

        // Always/Sources/Always/AppDelegate.swift → up 4 = repo root,
        // where the crate lives. (CLIService.swift reaches the same root with
        // one more hop because it sits in Services/.)
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

    /// Dock-icon click handler (only when activation policy is `.regular`).
    /// clicking the Dock icon while the app has no visible window
    /// fires this — open Settings (the Tauri window) as the canonical
    /// entry point.
    func applicationShouldHandleReopen(
        _ sender: NSApplication, hasVisibleWindows: Bool
    ) -> Bool {
        if !hasVisibleWindows {
            openSettingsWindow()
        }
        return true
    }

    func applicationWillTerminate(_ notification: Notification) {
        stopDaemonForAppTermination(killDaemon: Self.killStaleDaemon)
    }

    func stopDaemonForAppTermination(killDaemon: () -> Void) {
        daemonBootstrapTask?.cancel()
        daemonBootstrapTask = nil
        stateMonitor?.prepareForQuit()
        killDaemon()
        singleInstanceGuard.release()
    }

    /// Menu-bar app: keep running when the onboarding window is closed —
    /// it lives in the status bar via `MenuBarExtra`. Returning false
    /// prevents the macOS 26 silent-exit bug where SwiftUI briefly
    /// considers all `Window` scenes closed during the MenuBarExtra-only
    /// steady state.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if Self.userInitiatedQuit {
            return .terminateNow
        }
        if NSApp.currentEvent != nil {
            return .terminateNow
        }
        let visibleWindows = NSApp.windows.contains { $0.isVisible }
        if !visibleWindows {
            NSLog("Always: refusing background terminate (no event, no window)")
            return .terminateCancel
        }
        return .terminateNow
    }

    /// If multiple voice daemons are running they both paste the same
    /// utterance — reconcile by killing all and starting one fresh copy.
    static func reconcileDuplicateDaemons(cli: CLIService?) async {
        let pids = listDaemonProcessIDs()
        guard pids.count > 1 else { return }
        NSLog("Always: WARNING — \(pids.count) daemon processes (\(pids)) — reconciling")
        killStaleDaemon()
        try? await Task.sleep(nanoseconds: 400_000_000)
        do {
            _ = try await cli?.startDaemon()
        } catch {
            NSLog("Always: daemon reconcile restart failed: \(error.localizedDescription)")
        }
    }

    /// Default UDS socket path — shared with `UDSClient`.
    static func daemonSocketPath() -> String {
        UDSClient.defaultSocketPath()
    }

    /// True when `always.sock` accepts a connection (mirrors Rust `socket_is_live`).
    static func isDaemonSocketLive() -> Bool {
        let path = daemonSocketPath()
        guard FileManager.default.fileExists(atPath: path) else { return false }
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return false }
        defer { close(fd) }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = path.utf8
        guard pathBytes.count < MemoryLayout.size(ofValue: addr.sun_path) else { return false }
        _ = withUnsafeMutablePointer(to: &addr.sun_path) { pathPtr in
            memcpy(pathPtr, path, pathBytes.count)
        }
        let result = withUnsafeBytes(of: &addr) { addrBytes in
            Darwin.connect(
                fd,
                addrBytes.baseAddress!.assumingMemoryBound(to: sockaddr.self),
                socklen_t(MemoryLayout<sockaddr_un>.size)
            )
        }
        return result == 0
    }

    /// Live UDS socket and no duplicate `always run` processes.
    static func isHealthyDaemon() -> Bool {
        guard isDaemonSocketLive() else { return false }
        return listDaemonProcessIDs().count <= 1
    }

    /// Kill voice daemons via pid file + ps sweep (never `pkill -f`, which
    /// can match its own argv and hang). Instance-scoped: the dev app only
    /// touches its own daemon, never the production one.
    static func killStaleDaemon() {
        let pidPath = AppInstance.pidPath

        if let pidString = try? String(contentsOfFile: pidPath, encoding: .utf8)
            .trimmingCharacters(in: .whitespacesAndNewlines),
           let pid = pid_t(pidString),
           kill(pid, 0) == 0 {
            kill(pid, SIGTERM)
            for _ in 0..<40 {
                usleep(50_000)
                if kill(pid, 0) != 0 { break }
            }
            if kill(pid, 0) == 0 {
                kill(pid, SIGKILL)
                usleep(100_000)
            }
        }

        try? FileManager.default.removeItem(atPath: pidPath)

        for pid in listDaemonProcessIDs() {
            kill(pid, SIGTERM)
        }
        usleep(200_000)
        for pid in listDaemonProcessIDs() {
            kill(pid, SIGKILL)
        }
        usleep(100_000)

        // The daemon's death (SIGKILL included) reparents its recorder to
        // launchd, still holding the mic. Reap those before a new daemon
        // spawns its own.
        reapOrphanedAudioChildren()

        try? FileManager.default.removeItem(atPath: AppInstance.socketPath)
    }

    /// SIGKILL audio children orphaned by a dead daemon (ppid 1): our
    /// `always-capture` helper (any `.app/Contents/MacOS/always-capture`) and
    /// SoX `rec`/`sox` run with the daemon's exact argument signature. A
    /// user's own `sox` never matches. SoX has no parent watchdog, so it
    /// otherwise outlives the daemon forever.
    static func reapOrphanedAudioChildren() {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/bin/ps")
        proc.arguments = ["-axo", "pid=,ppid=,command="]
        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = FileHandle.nullDevice
        try? proc.run()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        proc.waitUntilExit()
        let text = String(data: data, encoding: .utf8) ?? ""
        for line in text.split(whereSeparator: \.isNewline) {
            let fields = line.split(maxSplits: 2, whereSeparator: { $0.isWhitespace })
            guard fields.count == 3, let pid = pid_t(fields[0]), pid_t(fields[1]) == 1 else { continue }
            let command = fields[2].trimmingCharacters(in: .whitespaces)
            if isOurAudioChildCommand(command) {
                kill(pid, SIGKILL)
            }
        }
    }

    private static func isOurAudioChildCommand(_ command: String) -> Bool {
        let helper = "/always-capture"
        if let range = command.range(of: helper) {
            let exe = String(command[..<range.upperBound])
            let rest = command[range.upperBound...]
            if (rest.isEmpty || rest.hasPrefix(" ")) && exe.hasPrefix("/")
                && exe.hasSuffix(".app/Contents/MacOS/always-capture") {
                return true
            }
        }
        let argv0 = command.split(whereSeparator: { $0.isWhitespace }).first.map(String.init) ?? ""
        let base = URL(fileURLWithPath: argv0).lastPathComponent
        return (base == "rec" || base == "sox")
            && command.contains(" --no-show-progress --buffer ")
            && command.contains(" remix - rate 16000 channels 1")
    }

    /// PIDs whose argv is `always-daemon run` for THIS instance — a dev
    /// GUI never sees the production daemon and vice versa.
    static func listDaemonProcessIDs() -> [pid_t] {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/bin/ps")
        proc.arguments = ["-ax", "-o", "pid=,command="]
        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = FileHandle.nullDevice
        try? proc.run()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        proc.waitUntilExit()
        let text = String(data: data, encoding: .utf8) ?? ""
        var pids: [pid_t] = []
        for line in text.split(whereSeparator: \.isNewline) {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            guard let space = trimmed.firstIndex(where: { $0.isWhitespace }) else { continue }
            let pidStr = trimmed[..<space]
            let command = trimmed[space...].trimmingCharacters(in: .whitespaces)
            guard let pid = pid_t(pidStr) else { continue }
            if isDaemonRunCommand(command) {
                pids.append(pid)
            }
        }
        return pids
    }

    private static func isDaemonRunCommand(_ command: String) -> Bool {
        let parts = command.split(whereSeparator: { $0.isWhitespace })
        guard parts.count >= 2 else { return false }
        let executable = String(parts[0] as Substring)
        let name = URL(fileURLWithPath: executable).lastPathComponent
        return (name == "always" || name == "always-daemon") && parts[1] == "run"
            && AppInstance.isSameInstance(command: command)
    }
}