import SwiftUI
import AppKit

@main
struct AlwaysApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate
    @StateObject private var onboardingState = OnboardingState()

    var body: some Scene {
        // Status-bar item is created manually in AppDelegate via NSStatusBar
        // (see installStatusItem). SwiftUI's MenuBarExtra was unreliable on
        // macOS 26 — the icon would disappear or get hidden behind the notch
        // moments after launch. NSStatusItem is the production-grade path.

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
        // Run as a regular app so the Dock shows our icon with the
        // standard "running" indicator dot AND we keep the status bar
        // item (installed manually by AppDelegate). `.regular` is the
        // default for non-LSUIElement apps but we set it explicitly to
        // override any inherited state from the previous LSUIElement
        // build.
        NSApplication.shared.setActivationPolicy(.regular)
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

    /// Status bar item — pinned to a strong reference so AppKit doesn't
    /// dealloc it. macOS 26 broke SwiftUI's MenuBarExtra in subtle ways
    /// (icon hidden behind notch / Control Center overflow); managing
    /// the NSStatusItem directly is reliable.
    private var statusItem: NSStatusItem?
    /// Popover that hosts the SwiftUI MenuBarView when the icon is clicked.
    private var menuPopover: NSPopover?
    /// Monitor that closes the popover when the user clicks outside it.
    private var popoverDismissMonitor: Any?

    func setOnboardingState(_ state: OnboardingState) {
        onboardingState = state
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        // `.regular` activation policy was already applied in App.init();
        // we don't re-set it here so the policy decided pre-runloop
        // stays the single source of truth.
        cliService = CLIService()

        // Manually install the status bar item. Must run on the main thread
        // and happens here (post-launch) rather than in init so NSApp is
        // fully wired.
        installStatusItem()

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

    /// Create the NSStatusItem and wire up the click handler that toggles
    /// the MenuBarView popover.
    private func installStatusItem() {
        NSLog("AlwaysApp: installStatusItem starting")
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        // CRITICAL: assign an `autosaveName` BEFORE setting isVisible.
        // Without this, macOS Sequoia+ auto-assigns the item to a generic
        // "Item-N" slot whose visibility key may already be 0 (the user
        // hid some unrelated status item previously), so the icon never
        // appears. Use a fully-qualified, app-unique name so it can't
        // collide with anything else in com.apple.controlcenter.
        item.autosaveName = "com.alwaysapp.statusitem"
        NSLog("AlwaysApp: got NSStatusItem visible=\(item.isVisible) length=\(item.length)")
        // Force visibility for first-run and to override any stale "hidden"
        // pref from previous installs.
        item.isVisible = true
        item.behavior = []

        if let button = item.button {
            // Configure the SF Symbol with explicit point size — without
            // this, some macOS 26 builds render an empty image that doesn't
            // take any space in the menu bar, making the item invisible.
            let symbolConfig = NSImage.SymbolConfiguration(pointSize: 16, weight: .regular)
            let symbol = NSImage(systemSymbolName: "mic.fill", accessibilityDescription: "Always")?
                .withSymbolConfiguration(symbolConfig)
            symbol?.isTemplate = true

            button.image = symbol
            // Always set a title fallback — if the image fails to render
            // we want SOMETHING in the bar so the icon is discoverable.
            if symbol == nil {
                NSLog("AlwaysApp: mic.fill failed to load, falling back to text title")
                button.title = "AL"
                button.imagePosition = .noImage
            } else {
                button.imagePosition = .imageOnly
            }
            button.toolTip = "Always — voice activation"
            button.target = self
            button.action = #selector(statusItemClicked(_:))
            button.sendAction(on: [.leftMouseUp, .rightMouseUp])
            NSLog("AlwaysApp: status button configured image=\(symbol != nil) frame=\(button.frame)")
        } else {
            NSLog("AlwaysApp: ERROR — NSStatusItem.button is nil")
        }
        statusItem = item
        NSLog("AlwaysApp: installStatusItem complete; visible=\(item.isVisible)")

        // Pre-build the popover. NSHostingController hosts SwiftUI content.
        let popover = NSPopover()
        popover.behavior = .transient
        popover.contentSize = NSSize(width: 240, height: 320)
        popover.contentViewController = NSHostingController(rootView: MenuBarView())
        menuPopover = popover
    }

    @objc private func statusItemClicked(_ sender: NSStatusBarButton) {
        guard let popover = menuPopover else { return }
        if popover.isShown {
            popover.performClose(nil)
            removeDismissMonitor()
        } else {
            popover.show(relativeTo: sender.bounds, of: sender, preferredEdge: .minY)
            // Auto-close when the user clicks outside the popover.
            popoverDismissMonitor = NSEvent.addGlobalMonitorForEvents(
                matching: [.leftMouseDown, .rightMouseDown]
            ) { [weak self] _ in
                self?.menuPopover?.performClose(nil)
                self?.removeDismissMonitor()
            }
            // Bring popover to front so the SwiftUI controls receive events.
            NSApp.activate(ignoringOtherApps: true)
        }
    }

    private func removeDismissMonitor() {
        if let monitor = popoverDismissMonitor {
            NSEvent.removeMonitor(monitor)
            popoverDismissMonitor = nil
        }
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
