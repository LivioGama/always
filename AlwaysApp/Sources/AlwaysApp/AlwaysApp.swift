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
        .defaultSize(width: 800, height: 750)
        
        Window("Welcome to Always", id: "onboarding") {
            OnboardingView()
        }
        .defaultSize(width: 500, height: 400)
    }
    
    init() {
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
            if let window = NSApp.windows.first(where: { $0.title == "Welcome to Always" }) {
                window.makeKeyAndOrderFront(nil)
            }
        }

        // Bootstrap the singleton — touching .shared lazily creates it,
        // which connects to the daemon over UDS and wires the overlay
        // subscription. Without this access nothing else triggers it.
        _ = StateMonitor.shared

        Task {
            _ = try? await cliService?.startDaemon()
        }
    }
}
