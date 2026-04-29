import SwiftUI
import AppKit
import Combine
import ApplicationServices

@main
struct AlwaysApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    var body: some Scene {
        MenuBarExtra("Always", systemImage: "mic.fill") {
            MenuBarView()
        }
        .menuBarExtraStyle(.window)

        Window("Always Settings", id: "settings") {
            SettingsWindow(cliService: CLIService())
        }
        .defaultSize(width: 800, height: 750)
    }
}

class AppDelegate: NSObject, NSApplicationDelegate {
    private var stateMonitor: StateMonitor?
    private var overlayController: OverlayController?
    private var cancellables = Set<AnyCancellable>()

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Prevent app from appearing in Dock
        NSApp.setActivationPolicy(.accessory)

        // Set up overlay controller
        overlayController = OverlayController()

        // Set up state monitoring
        stateMonitor = StateMonitor()

        // Combine all states for responsive overlay updates
        if let monitor = stateMonitor {
            Publishers.CombineLatest4(monitor.$isListening, monitor.$isProcessing, monitor.$isPaused, monitor.$isAutoEnter)
                .sink { [weak self] isListening, isProcessing, isPaused, isAutoEnter in
                    self?.updateOverlayState(isListening: isListening, isProcessing: isProcessing, isPaused: isPaused, isAutoEnter: isAutoEnter)
                }
                .store(in: &cancellables)

            // Subscribe to notification trigger
            monitor.$showNotification
                .sink { [weak self] shouldShow in
                    if shouldShow {
                        self?.overlayController?.showNotification()
                    }
                }
                .store(in: &cancellables)
        }
    }

    private func updateOverlayState(isListening: Bool, isProcessing: Bool, isPaused: Bool, isAutoEnter: Bool) {
        if isPaused {
            overlayController?.setState(.paused)
        } else if isAutoEnter {
            overlayController?.setState(.autoEnter)
        } else if isListening {
            overlayController?.setState(.listening)
        } else if isProcessing {
            overlayController?.setState(.processing)
        } else {
            overlayController?.setState(.hidden)
        }
    }
}
