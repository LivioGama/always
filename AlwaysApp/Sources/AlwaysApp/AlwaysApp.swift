import SwiftUI
import AppKit

@main
struct AlwaysApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate
    @StateObject private var onboardingState = OnboardingState()

    var body: some Scene {
        MenuBarExtra("Always", systemImage: "mic.fill") {
            MenuBarView()
        }
        .menuBarExtraStyle(.menu)

        Window("Always Settings", id: "settings") {
            SettingsWindow(cliService: CLIService())
        }
        // `.contentSize` makes the window grow to exactly fit its SwiftUI
        // content and disables manual resize handles. The settings view
        // is laid out to fit on a 14" laptop without any scrolling.
        .windowResizability(.contentSize)

        Window("Welcome to Always", id: "onboarding") {
            OnboardingView()
        }
        .defaultSize(width: 500, height: 400)
    }
    
    init() {
        // CRITICAL: set accessory policy BEFORE SwiftUI evaluates scenes.
        // If we wait until applicationDidFinishLaunching, AppKit/SwiftUI on
        // macOS 26 may have already decided the app should quit because no
        // Window scene is currently presented (MenuBarExtra alone doesn't
        // count). Setting it here makes the process status-bar-only from
        // the very first runloop turn.
        NSApplication.shared.setActivationPolicy(.accessory)
        // Refuse sudden/auto termination at the framework level too —
        // belt-and-suspenders alongside the Info.plist keys.
        ProcessInfo.processInfo.disableSuddenTermination()
        ProcessInfo.processInfo.disableAutomaticTermination("AlwaysApp must keep its status bar item alive")

        // Inject onboarding state into appDelegate after initialization
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) { [self] in
            appDelegate.setOnboardingState(onboardingState)
        }
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
    /// Set true by the Quit menu item so we know a termination request is
    /// user-initiated and should be honored. Anything else (system idle
    /// reaper, SwiftUI scene lifecycle, etc.) is refused.
    static var userInitiatedQuit = false

    func setOnboardingState(_ state: OnboardingState) {
        onboardingState = state
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        cliService = CLIService()

        // Check if onboarding is needed
        onboardingState?.checkAndShowOnboardingIfNeeded()

        // Show onboarding window if needed
        if onboardingState?.showOnboarding == true {
            if let window = NSApp.windows.first(where: { $0.identifier?.rawValue == "AlwaysOnboarding" }) {
                window.makeKeyAndOrderFront(nil)
            }
        }

        // Kill any stale daemon from a previous session (crash, force-quit, etc.)
        // before starting fresh. This prevents the broken-pipe bug where a new
        // Mac app connects to an old daemon with stale UDS state.
        Self.killStaleDaemon()

        // Bootstrap the singleton — touching .shared lazily creates it,
        // which connects to the daemon over UDS and wires the overlay
        // subscription. Without this access nothing else triggers it.
        let monitor = StateMonitor.shared

        // System audio output watcher — auto-pauses the daemon when
        // any app starts producing sound. Idempotent: start() is
        // safe to call multiple times.
        AudioOutputMonitor.shared.start(stateMonitor: monitor)
        // Push the frontmost app's bundle id to the daemon so per-app
        // settings overlay applies from the first paste.
        FocusedAppMonitor.shared.start(stateMonitor: monitor)

        Task {
            _ = try? await cliService?.startDaemon()
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
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

    /// Reject every termination request that wasn't initiated by the Quit
    /// menu item. On macOS 26 SwiftUI's MenuBarExtra appears to send a
    /// terminate: right after `NSStatusItemChangeVisibilityAction`, which
    /// kills the app moments after launch. Honoring only user-initiated
    /// quits prevents that.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if Self.userInitiatedQuit {
            return .terminateNow
        }
        NSLog("AlwaysApp: refusing non-user terminate request")
        return .terminateCancel
    }

    /// Read the daemon PID file and send SIGTERM. Synchronous so it works
    /// in applicationWillTerminate (no time for async). Also removes the
    /// PID file so the next launch sees a clean slate.
    static func killStaleDaemon() {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let pidPath = home
            .appendingPathComponent("Library/Application Support/always/always.pid")
            .path

        guard let pidString = try? String(contentsOfFile: pidPath, encoding: .utf8)
                .trimmingCharacters(in: .whitespacesAndNewlines),
              let pid = pid_t(pidString) else { return }

        // Check if the process is actually running before killing
        if kill(pid, 0) == 0 {
            kill(pid, SIGTERM)
            // Give it a moment to clean up, then force-kill if still alive
            usleep(200_000) // 200ms
            if kill(pid, 0) == 0 {
                kill(pid, SIGKILL)
            }
        }

        // Remove PID file regardless — it's stale either way
        try? FileManager.default.removeItem(atPath: pidPath)

        // Remove socket file so daemon starts with clean socket
        let sockPath = home
            .appendingPathComponent("Library/Caches/Always/always.sock")
            .path
        try? FileManager.default.removeItem(atPath: sockPath)
    }
}
