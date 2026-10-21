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
        // Tauri settings window (`always-tauri`): no SwiftUI settings scene
        // anymore — every "open Settings" entry point (menu bar, Dock,
        // always://settings) activates or spawns `always-tauri` (see
        // AppDelegate.swift / MenuBarView.swift).

        Window("Welcome to Always", id: "onboarding") {
            OnboardingView()
        }
        .defaultSize(width: 580, height: 640)
        .commands {
            CommandGroup(replacing: .appTermination) {
                Button("Quit Always") {
                    AppDelegate.userInitiatedQuit = true
                    NSApp.terminate(nil)
                }
                .keyboardShortcut("q")
            }
        }

        // Known-good menu-bar lineage: SwiftUI owns the status item so the
        // system positions it in the real menu bar. A manual NSStatusItem
        // does not place on this macOS 26 install (it lands at the bottom-left
        // fallback coordinate), and the floating-NSPanel workaround that tried
        // to compensate rendered a stray badge over the window — both removed.
        MenuBarExtra {
            MenuBarView()
        } label: {
            MenuBarStatusLabel()
        }
        .menuBarExtraStyle(.menu)
    }
    
    init() {
        // Regular policy (Dock icon) keeps the app alive on macOS 26 —
        // `.accessory` triggers the SwiftUI silent-exit bug here.
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
    private var hostedWindow: NSWindow?

    /// Show first-run setup exactly once, to someone who has never been
    /// through it.
    ///
    /// This used to key off "is a Groq API key saved?", which is wrong in
    /// both directions. A user running a local model needs no key, so the
    /// welcome window reopened on every single launch, forever, with no
    /// way to make it stop. And a user who already had a key never saw
    /// onboarding at all, so they never got walked through permissions or
    /// My Voice. Completion is now its own recorded fact rather than
    /// something inferred from unrelated config.
    ///
    /// Existing installs are treated as already-onboarded: someone with a
    /// working setup must not be shown a welcome window on upgrade. The
    /// signal is a saved API key or a recorded voice profile — either
    /// proves they have used the app before this flag existed.
    func checkAndShowOnboardingIfNeeded() {
        if OnboardingCompletion.isComplete {
            return
        }
        Task {
            let config = try? await CLIService().getConfig()
            let hasAPIKey = config?.groqKeySaved ?? false
            let hasVoiceProfile = UserDefaults.standard.bool(
                forKey: "voiceEnrollmentIsEnrolled"
            )
            await MainActor.run {
                if hasAPIKey || hasVoiceProfile {
                    // Pre-existing install — retro-mark so this check never
                    // runs again for them.
                    OnboardingCompletion.markComplete()
                    return
                }
                self.showOnboarding = true
                // Must happen HERE, after the async check resolved — the
                // old call site checked `showOnboarding` synchronously
                // right after kicking this Task off, so the window never
                // opened for a fresh install.
                self.openOnboardingWindow()
            }
        }
    }

    /// Force the onboarding tutorial to show, bypassing the pre-existing
    /// install check. Used for testing or when the user explicitly wants
    /// to re-watch the tutorial.
    @MainActor
    func forceOnboarding() {
        OnboardingCompletion.reset()
        showOnboarding = true
        openOnboardingWindow()
    }

    @MainActor
    private func openOnboardingWindow() {
        NSApp.activate(ignoringOtherApps: true)
        if let window = NSApp.windows.first(where: {
            $0.identifier?.rawValue == "AlwaysOnboarding"
                || $0.identifier?.rawValue == "onboarding"
                || $0.title == "Welcome to Always"
        }) {
            window.makeKeyAndOrderFront(nil)
            return
        }
        // SwiftUI `Window` scenes aren't instantiated until first opened and
        // AppKit has no handle to open them — host the view directly.
        if hostedWindow == nil {
            let hosting = NSHostingController(rootView: OnboardingView())
            let window = NSWindow(contentViewController: hosting)
            window.title = "Welcome to Always"
            window.identifier = NSUserInterfaceItemIdentifier("AlwaysOnboarding")
            window.isReleasedWhenClosed = false
            window.center()
            hostedWindow = window
        }
        hostedWindow?.makeKeyAndOrderFront(nil)
    }
}

