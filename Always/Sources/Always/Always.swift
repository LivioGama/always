import SwiftUI
import AppKit
import ApplicationServices
import AVFoundation
import Darwin
@main
struct Always: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate
    @StateObject private var onboardingState = OnboardingState()

    var body: some Scene {
        // `Window` (not `WindowGroup`) — singleton settings scene.
        // WindowGroup allows multiple instances, which let the user
        // end up with two Settings panels open at once via Cmd+N,
        // repeated `openWindow(id:)` calls from the menu, or Dock
        // re-open paths. Switching to `Window` makes SwiftUI focus
        // the existing instance instead of creating a new one.
        Window("Always Settings", id: "settings") {
            SettingsWindow(cliService: CLIService())
        }
        // Explicit size — `.contentSize` under-reports height on macOS 26
        // (MenuBarExtra host), which left Settings clipped after Models.
        .defaultSize(
            width: SettingsWindowMetrics.width,
            height: SettingsWindowMetrics.height
        )
        .windowResizability(.automatic)
        .commands {
            CommandGroup(replacing: .appTermination) {
                Button("Quit Always") {
                    AppDelegate.userInitiatedQuit = true
                    NSApp.terminate(nil)
                }
                .keyboardShortcut("q")
            }
        }

        Window("Welcome to Always", id: "onboarding") {
            OnboardingView()
        }
        .defaultSize(width: 500, height: 400)

        // Single menu bar entry — MenuBarExtra only (no NSStatusItem duplicate).
        MenuBarExtra {
            MenuBarView()
        } label: {
            MenuBarStatusLabel()
        }
        .menuBarExtraStyle(.menu)
    }
    
    init() {
        // Dock icon + MenuBarExtra: reachable Settings when Control Center
        // hides the status item. Keep-alive window prevents silent exit.
        NSApplication.shared.setActivationPolicy(.regular)
        // Do NOT call disableAutomaticTermination here — it blocks Dock Quit / Cmd+Q
        // until enableAutomaticTermination is called, and the keep-alive window plus
        // applicationShouldTerminateAfterLastWindowClosed(false) already prevent the
        // macOS 26 silent-exit bug.

        // Inject onboarding state into AppDelegate. SwiftUI guarantees that
        // by the time `init()` runs, `@NSApplicationDelegateAdaptor` has
        // already produced the delegate instance, so we can wire it
        // synchronously instead of leaning on a 100ms timer that could
        // miss its window if `applicationDidFinishLaunching` fires first.
        appDelegate.setOnboardingState(onboardingState)
    }
}

class OnboardingState: ObservableObject {
    @Published var showOnboarding = false
    
    func checkAndShowOnboardingIfNeeded() {
        Task {
            let config = try? await CLIService().getConfig()
            let hasAPIKey = !(config?.groqApiKey?.isEmpty ?? true)
            await MainActor.run {
                if !hasAPIKey {
                    showOnboarding = true
                }
            }
        }
    }
}

class AppDelegate: NSObject, NSApplicationDelegate {
    private var cliService: CLIService?
    private var onboardingState: OnboardingState?
    /// Set by Quit menu / Cmd+Q so termination is honored.
    static var userInitiatedQuit = false

    private var stateMonitor: StateMonitor?
    /// Invisible window that prevents SwiftUI from tearing down the process
    /// when no Settings window is open (macOS 26 silent exit).
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
        StateMonitor.shared.beginBootstrap()
        StateMonitor.shared.connectToDaemon()
        NSLog("Always: MenuBarExtra installed")
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        cliService = CLIService()

        // Onboarding gating: if no saved Groq API key exists,
        // surface the onboarding window. The scene id "onboarding"
        // must match the `Window(id:)` registration in App.body.
        onboardingState?.checkAndShowOnboardingIfNeeded()
        if onboardingState?.showOnboarding == true,
           let window = NSApp.windows.first(where: { $0.identifier?.rawValue == "onboarding" }) {
            window.makeKeyAndOrderFront(nil)
        }

        // Permission flow lives in `PermissionsManager`. It seeds the
        // current status (silent — no prompts) and triggers the system
        // dialog for `notDetermined` cases. The Settings UI hosts a
        // banner that surfaces "denied" / "not trusted" states and
        // links the user to System Settings. We deliberately do NOT
        // open modal alerts at launch — the user just sees the banner
        // and can act when convenient.
        let perms = PermissionsManager.shared
        perms.requestMicrophoneIfNeeded()
        perms.requestAccessibilityIfNeeded()

        let cli = cliService
        // Connect immediately — UDS retries until the socket is live. Daemon
        // cleanup/spawn runs concurrently so a healthy daemon is never killed
        // just because the GUI relaunched.
        Task { @MainActor [weak self] in
            let monitor = StateMonitor.shared
            self?.stateMonitor = monitor
            StatusOverlayController.shared.prewarm()
            AudioOutputMonitor.shared.start(stateMonitor: monitor)
            FocusedAppMonitor.shared.start(stateMonitor: monitor)
        }

        daemonBootstrapTask = Task {
            if Self.isHealthyDaemon() {
                NSLog("Always: healthy daemon detected — attaching without restart")
                await Self.reconcileDuplicateDaemons(cli: cli)
                return
            }

            await Task.detached(priority: .userInitiated) {
                Self.killStaleDaemon()
            }.value

            do {
                _ = try await cli?.startDaemon()
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

    /// 1×1 off-screen window so AppKit keeps the process alive when only
    /// MenuBarExtra is visible (no Settings window open).
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

    /// Open (or focus) the Settings window. Called on launch and
    /// from `applicationShouldHandleReopen` when the user clicks
    /// the Dock icon. SwiftUI auto-instantiates the `Window(id:)`
    /// scene's content the first time we surface a window via the
    /// app activation path.
    private func openSettingsWindow() {
        NSApp.activate(ignoringOtherApps: true)
        // Bring any matching window forward. With the scene declared
        // as `Window` (singleton), SwiftUI keeps a single instance
        // alive across the app's lifetime — focusing it is enough; no
        // need to synthesise a "new window" action (which used to
        // spawn duplicates under WindowGroup).
        if let existing = NSApp.windows.first(where: {
            $0.title == "Always Settings" || $0.identifier?.rawValue == "settings"
        }) {
            SettingsWindowMetrics.apply(to: existing)
            existing.makeKeyAndOrderFront(nil)
            existing.orderFrontRegardless()
        }
    }

    /// Dock-icon click handler (only when activation policy is `.regular`).
    /// clicking the Dock icon while the app has no visible window
    /// fires this — open Settings as the canonical entry point.
    func applicationShouldHandleReopen(
        _ sender: NSApplication, hasVisibleWindows: Bool
    ) -> Bool {
        if !hasVisibleWindows {
            openSettingsWindow()
        }
        return true
    }

    func applicationWillTerminate(_ notification: Notification) {
        stateMonitor?.prepareForQuit()
        // Leave the voice daemon running so the next GUI launch can attach
        // instantly over the existing UDS socket. Stale daemons are reaped
        // by the orphan watchdog in uds_server.rs or by explicit restart.
        singleInstanceGuard.release()
    }

    /// Menu-bar (LSUIElement) apps must NOT quit when the last window
    /// closes — they live in the status bar. The default value is true,
    /// which caused the GUI to auto-quit moments after launch on macOS
    /// 26.x: SwiftUI briefly considers all `Window` scenes "closed"
    /// during the MenuBarExtra-only steady state, and AppKit honors the
    /// default by terminating the process. Returning false keeps the
    /// status-bar item alive.
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
    /// can match its own argv and hang).
    static func killStaleDaemon() {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let pidPath = home
            .appendingPathComponent("Library/Application Support/always/always.pid")
            .path

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

        let sockPath = home
            .appendingPathComponent("Library/Caches/Always/always.sock")
            .path
        try? FileManager.default.removeItem(atPath: sockPath)
    }

    /// PIDs whose argv is `always-daemon run` (bundled or dev).
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
    }
}
