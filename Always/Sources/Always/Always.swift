import SwiftUI
import AppKit
import Combine
import os.log

@main
struct Always: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate
    @StateObject private var onboardingState = OnboardingState()

    var body: some Scene {
        // WindowGroup (not Window) so SwiftUI auto-opens the
        // primary window on launch for `.regular` activation
        // policy. Single-window UX is enforced by `.commandsRemoved`
        // (no File→New) and the menubar control having no
        // "New Settings Window" item.
        WindowGroup("Always Settings", id: "settings") {
            SettingsWindow(cliService: CLIService())
        }
        // `.contentSize` makes the window grow to exactly fit its SwiftUI
        // content and disables manual resize handles. The settings view
        // is laid out to fit on a 14" laptop without any scrolling.
        .windowResizability(.contentSize)
        .commandsRemoved()

        Window("Welcome to Always", id: "onboarding") {
            OnboardingView()
        }
        .defaultSize(width: 500, height: 400)
    }
    
    init() {
        // CRITICAL — start as `.accessory` so the status item registers
        // correctly. Switching to `.regular` synchronously here is a
        // confirmed macOS Tahoe 26 bug (Stats #3120, Maccy #1224, Ice
        // #711, AeroSpace #1968, Apple Forums 650270): the status item
        // gets created in com.apple.controlcenter.statusitems but never
        // renders in the visible menu bar. The AppDelegate upgrades the
        // policy to `.regular` asynchronously AFTER the status item is
        // installed, so we still get the Dock running-dot.
        NSApplication.shared.setActivationPolicy(.accessory)
        // Refuse sudden/auto termination at the framework level too —
        // belt-and-suspenders alongside the Info.plist keys.
        ProcessInfo.processInfo.disableSuddenTermination()
        ProcessInfo.processInfo.disableAutomaticTermination("Always must keep its status bar item alive")

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
    /// State monitor for updating the status bar icon dynamically.
    private var stateMonitor: StateMonitor?
    private var cancellables = Set<AnyCancellable>()
    private let logger = OSLog(subsystem: "com.always.app", category: "status-bar-icon")

    func setOnboardingState(_ state: OnboardingState) {
        onboardingState = state
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        cliService = CLIService()

        // Install the status item under .accessory (set in App.init())
        // and STAY .accessory permanently. Confirmed Tahoe 26 bug: any
        // later upgrade to .regular leaves the status item un-slotted
        // in the menu bar so it renders at screen origin (0,0 → bottom
        // left) instead of the top right. User verified this by
        // clicking the "invisible" icon and seeing the popover appear
        // at the bottom-left corner of the screen.
        //
        // Trade-off: no Dock icon. Settings WindowGroup still
        // auto-opens for both .accessory and .regular, so the user
        // still gets an undeniable visible window on launch.
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
        stateMonitor = monitor

        // System audio output watcher — auto-pauses the daemon when
        // any app starts producing sound. Idempotent: start() is
        // safe to call multiple times.
        AudioOutputMonitor.shared.start(stateMonitor: monitor)
        // Push the frontmost app's bundle id to the daemon so per-app
        // settings overlay applies from the first paste.
        FocusedAppMonitor.shared.start(stateMonitor: monitor)

        // Subscribe to state changes to update the status bar icon
        setupStatusBarIconUpdates()

        Task {
            _ = try? await cliService?.startDaemon()
        }

        // Force-open Settings on launch so the user has an
        // undeniable entry point even if the menu-bar item is
        // hidden by a third-party menu-bar manager or macOS
        // overflow logic. The Dock icon + Settings window are now
        // the primary surface; the status item is a bonus.
        DispatchQueue.main.async {
            self.openSettingsWindow()
        }
    }

    /// Open (or focus) the Settings window. Called on launch and
    /// from `applicationShouldHandleReopen` when the user clicks
    /// the Dock icon. SwiftUI auto-instantiates the `Window(id:)`
    /// scene's content the first time we surface a window via the
    /// app activation path.
    private func openSettingsWindow() {
        NSApp.activate(ignoringOtherApps: true)
        // Bring any matching window forward.
        if let existing = NSApp.windows.first(where: {
            $0.title == "Always Settings" || $0.identifier?.rawValue == "settings"
        }) {
            existing.makeKeyAndOrderFront(nil)
            return
        }
        // No window yet — synthesise the standard "show new window"
        // action AppKit binds to Cmd+N. SwiftUI's Window scene
        // intercepts this and creates the first instance.
        NSApp.sendAction(#selector(NSApplication.newWindowForTab(_:)), to: nil, from: nil)
        // Retry the lookup after a frame so the window is in
        // NSApp.windows by the time we activate it.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
            NSApp.windows.first(where: {
                $0.title == "Always Settings" || $0.identifier?.rawValue == "settings"
            })?.makeKeyAndOrderFront(nil)
        }
    }

    /// Dock-icon click handler. With `.regular` activation policy,
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

    /// With `.regular` activation policy we don't suffer from the
    /// SwiftUI MenuBarExtra phantom-terminate bug, so accept legitimate
    /// quit requests: explicit Quit menu / Cmd+Q from the foreground
    /// app, plus the userInitiatedQuit flag used by our own menu items.
    /// Only refuse "ghost" termination requests that arrive with no
    /// active user event AND no visible window — that pattern is the
    /// macOS idle reaper trying to kill a backgrounded app.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if Self.userInitiatedQuit {
            return .terminateNow
        }
        // User-driven quit (Cmd+Q, menu, Dock right-click → Quit) always
        // arrives with a current event. Honor it.
        if NSApp.currentEvent != nil {
            return .terminateNow
        }
        // System idle reaper: no event, no visible window — refuse.
        let visibleWindows = NSApp.windows.contains { $0.isVisible }
        if !visibleWindows {
            NSLog("Always: refusing background terminate (no event, no window)")
            return .terminateCancel
        }
        return .terminateNow
    }

    /// Create the NSStatusItem and wire up the click handler that toggles
    /// the MenuBarView popover.
    ///
    /// Kept deliberately minimal. Standard status-bar apps (Slack,
    /// Discord, Zoom) use the bare-bones recipe below and stay visible
    /// reliably when a third-party menu bar manager is not hiding them.
    private func installStatusItem() {
        NSLog("Always.installStatusItem: called")
        // variableLength per the Tahoe 26 research: fixed lengths
        // (60, squareLength) are more frequently dropped by ControlCenter
        // on 26.x — Stats devs flagged the same issue.
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        NSLog("Always.installStatusItem: item length=\(item.length) visible=\(item.isVisible)")
        if let button = item.button {
            // Initial icon is the listening/armed default; the Combine
            // subscription in setupStatusBarIconUpdates() will swap it
            // to the correct state-aware symbol once StateMonitor emits.
            let symbol = NSImage(systemSymbolName: "waveform", accessibilityDescription: "Always")
            symbol?.isTemplate = true
            button.image = symbol
            button.imagePosition = .imageOnly
            button.title = ""
            button.toolTip = "Always — voice activation"
            button.target = self
            button.action = #selector(statusItemClicked(_:))
            button.sendAction(on: [.leftMouseUp, .rightMouseUp])
            NSLog("Always.installStatusItem: button frame=\(button.frame) image=\(button.image != nil) title='\(button.title)'")
        }
        item.isVisible = true
        statusItem = item
        NSLog("Always.installStatusItem: done visible=\(item.isVisible) length=\(item.length)")

        // Pre-build the popover. NSHostingController hosts SwiftUI content.
        let popover = NSPopover()
        popover.behavior = .transient
        popover.contentSize = NSSize(width: 240, height: 320)
        popover.contentViewController = NSHostingController(rootView: MenuBarView())
        menuPopover = popover
    }

    /// Subscribe to StateMonitor changes to update the status bar icon
    /// dynamically. Match the canonical Tahoe-working pattern (MeetingBar,
    /// TahoeMenuDemo, HelloStatus): subscribe ONCE via Combine, update
    /// the image ONLY when state actually changes via `removeDuplicates`.
    /// NO periodic timer, NO force-update DispatchQueue.asyncAfter, NO
    /// `isVisible` toggling. Those were the bug — Tahoe 26 interprets
    /// each as a state change and re-runs the menu-bar slot allocation,
    /// which never converges. Proven by the minimal HELLO test app
    /// rendering correctly while Always didn't.
    private func setupStatusBarIconUpdates() {
        guard let monitor = stateMonitor else { return }
        // Fold all four state inputs into the SF Symbol name and dedupe on
        // that. Critical for the Tahoe 26 slot-churn fix — we MUST NOT
        // touch button.image unless the resolved icon actually changed.
        // Mapping many bool transitions to a single string and applying
        // `removeDuplicates` collapses redundant signals (e.g. transcribing
        // ticks while paused still resolves to "pause.circle.fill") into a
        // single assignment.
        Publishers.CombineLatest4(
            monitor.$isDaemonConnected,
            monitor.$isPaused,
            monitor.$isDaemonDegraded,
            monitor.$isTranscribing
        )
        .map { isConnected, isPaused, isDegraded, isTranscribing in
            StatusIconResolver.symbolName(
                isConnected: isConnected,
                isDegraded: isDegraded,
                isPaused: isPaused,
                isTranscribing: isTranscribing
            )
        }
        .removeDuplicates()
        .receive(on: DispatchQueue.main)
        .sink { [weak self] iconName in
            self?.applyStatusBarIcon(named: iconName)
        }
        .store(in: &cancellables)
    }

    /// Nuclear option: completely remove and recreate the status item
    private func recreateStatusItem() {
        guard let monitor = stateMonitor else { return }

        // Remove old status item
        if let oldItem = statusItem {
            NSStatusBar.system.removeStatusItem(oldItem)
            os_log("Removed old status item", log: logger, type: .info)
        }

        // Create new status item with unique autosave name to force macOS to treat it as new
        let timestamp = Int(Date().timeIntervalSince1970)
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.autosaveName = "Always-\(timestamp)"
        os_log("Created new status item with autosave name", log: logger, type: .info)

        // Create new status item
        let iconName = StatusIconResolver.symbolName(
            isConnected: monitor.isDaemonConnected,
            isDegraded: monitor.isDaemonDegraded,
            isPaused: monitor.isPaused,
            isTranscribing: monitor.isTranscribing
        )

        os_log("Setting icon to '%{public}@'", log: logger, type: .info, iconName)

        if let button = item.button {
            // Try using a custom view instead of just an image
            let config = NSImage.SymbolConfiguration(pointSize: 18, weight: .medium)
            let symbol = NSImage(systemSymbolName: iconName, accessibilityDescription: "Always")?
                .withSymbolConfiguration(config)
            symbol?.isTemplate = true

            button.image = symbol
            button.imagePosition = .imageOnly  // Remove title, show only icon
            button.imageScaling = .scaleProportionallyUpOrDown
            button.toolTip = "Always — voice activation"
            button.target = self
            button.action = #selector(statusItemClicked(_:))
            button.sendAction(on: [.leftMouseUp, .rightMouseUp])

            // Force button to update
            button.needsDisplay = true
            button.layout()

            // Force CALayer
            button.wantsLayer = true
            button.layer?.setNeedsDisplay()
        }

        item.isVisible = true
        statusItem = item

        // Recreate popover
        let popover = NSPopover()
        popover.behavior = .transient
        popover.contentSize = NSSize(width: 240, height: 320)
        popover.contentViewController = NSHostingController(rootView: MenuBarView())
        menuPopover = popover

        os_log("Recreated status item and popover", log: logger, type: .info)
    }

    /// Apply a resolved SF Symbol name to the status bar button. Strictly
    /// minimal, matching the MeetingBar / TahoeMenuDemo / HelloStatus
    /// pattern: just assign the image. Don't touch `isVisible`,
    /// `needsDisplay`, `layout()`, `window.update()` — Tahoe 26 treats
    /// each as a state change and re-runs slot allocation which never
    /// settles. Dedup is enforced upstream in `setupStatusBarIconUpdates`
    /// (Combine `removeDuplicates` on the resolved name), so by the time
    /// we land here the icon really has changed.
    private func applyStatusBarIcon(named iconName: String) {
        guard let button = statusItem?.button else { return }
        let image = NSImage(systemSymbolName: iconName, accessibilityDescription: "Always")
        image?.isTemplate = true
        button.image = image
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
    ///
    /// Belt-and-suspenders: after the PID-file-based kill, also sweep
    /// any process whose argv matches the daemon path in the app bundle
    /// ("MacOS/always run"). This catches orphans whose pid file was
    /// deleted (e.g. user manually rm'd it) but whose process is still
    /// alive holding the mic + UDS socket. The path-anchored pattern
    /// avoids matching unrelated commands that happen to contain
    /// "always run" in their argv.
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
            // Give it up to 2s to clean up, polling every 50ms. The
            // daemon's signal handler removes pid + socket files
            // before exiting, so we want to wait for that — yanking
            // it with SIGKILL too fast leaves stale files.
            for _ in 0..<40 {
                usleep(50_000) // 50ms
                if kill(pid, 0) != 0 { break }
            }
            if kill(pid, 0) == 0 {
                kill(pid, SIGKILL)
                usleep(100_000)
            }
        }

        // Remove PID file regardless — it's stale either way
        try? FileManager.default.removeItem(atPath: pidPath)

        // Belt-and-suspenders pkill in case pid file was missing or
        // stale-but-the-process-is-actually-elsewhere.
        let pkill = Process()
        pkill.executableURL = URL(fileURLWithPath: "/usr/bin/pkill")
        pkill.arguments = ["-TERM", "-f", "MacOS/always run"]
        pkill.standardOutput = FileHandle.nullDevice
        pkill.standardError = FileHandle.nullDevice
        try? pkill.run()
        pkill.waitUntilExit()
        usleep(200_000)

        // Remove socket file so daemon starts with clean socket
        let sockPath = home
            .appendingPathComponent("Library/Caches/Always/always.sock")
            .path
        try? FileManager.default.removeItem(atPath: sockPath)
    }
}
