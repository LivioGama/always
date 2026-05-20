import SwiftUI
import AppKit
import ApplicationServices
import AVFoundation
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
        let hasAPIKey = checkGroqAPIKey()
        if !hasAPIKey {
            showOnboarding = true
            // Note: openWindow needs to be called from a View context
            // We'll handle this differently
        }
    }
    
    private func checkGroqAPIKey() -> Bool {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "com.always.daemon",
            kSecAttrAccount as String: "groq_api_key",
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]
        
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        
        return status == errSecSuccess && item != nil
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

    func setOnboardingState(_ state: OnboardingState) {
        onboardingState = state
    }

    func applicationWillFinishLaunching(_ notification: Notification) {
        // Another Always.app already owns the menu-bar slot — hand focus to it
        // and exit before we install a second (invisible / orphaned) status item.
        if !Self.enforceSingleInstance() {
            exit(0)
        }
        installKeepAliveWindow()
        NSLog("Always: MenuBarExtra installed")
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        cliService = CLIService()

        // Onboarding gating: if no Groq API key is in the keychain,
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

        Self.killStaleDaemon()

        let cli = cliService
        daemonBootstrapTask = Task {
            do {
                _ = try await cli?.startDaemon()
            } catch {
                NSLog("Always: daemon start failed: \(error.localizedDescription)")
            }
            await MainActor.run { [weak self] in
                let monitor = StateMonitor.shared
                self?.stateMonitor = monitor
                monitor.connectToDaemon()
                AudioOutputMonitor.shared.start(stateMonitor: monitor)
                FocusedAppMonitor.shared.start(stateMonitor: monitor)
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
        Self.killStaleDaemon()
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

    /// If another Always.app is already running, activate it and return false
    /// so the caller can exit before creating a duplicate menu-bar item.
    @discardableResult
    static func enforceSingleInstance() -> Bool {
        guard let bundleId = Bundle.main.bundleIdentifier else { return true }
        let myPid = ProcessInfo.processInfo.processIdentifier
        let others = NSRunningApplication.runningApplications(withBundleIdentifier: bundleId)
            .filter { $0.processIdentifier != myPid }
        guard let existing = others.first else { return true }

        NSLog("Always: another instance is running (pid %d) — activating it and exiting",
              existing.processIdentifier)
        existing.activate(options: [.activateAllWindows])
        return false
    }

    /// Read the daemon PID file and send SIGTERM. Also sweeps bundled
    /// `always-daemon run` orphans and removes pid + socket files.
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

        let pkill = Process()
        pkill.executableURL = URL(fileURLWithPath: "/usr/bin/pkill")
        pkill.arguments = ["-TERM", "-f", "always-daemon run"]
        pkill.standardOutput = FileHandle.nullDevice
        pkill.standardError = FileHandle.nullDevice
        try? pkill.run()
        pkill.waitUntilExit()
        usleep(200_000)

        let sockPath = home
            .appendingPathComponent("Library/Caches/Always/always.sock")
            .path
        try? FileManager.default.removeItem(atPath: sockPath)
    }
}
