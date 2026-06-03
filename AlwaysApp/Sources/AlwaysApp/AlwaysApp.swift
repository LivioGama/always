import SwiftUI
import AppKit

@main
struct AlwaysApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    var body: some Scene {
        MenuBarExtra("Always", systemImage: "mic.fill") {
            MenuBarView()
        }
        .menuBarExtraStyle(.menu)

        Window("Always Settings", id: "settings") {
            SettingsWindow(cliService: CLIService())
        }
        .defaultSize(width: 800, height: 750)
    }
}

class AppDelegate: NSObject, NSApplicationDelegate {
    private var cliService: CLIService?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        cliService = CLIService()

        // Bootstrap the singleton — touching .shared lazily creates it,
        // which connects to the daemon over UDS and wires the overlay
        // subscription. Without this access nothing else triggers it.
        _ = StateMonitor.shared

        Task {
            _ = try? await cliService?.startDaemon()
        }
    }
}
