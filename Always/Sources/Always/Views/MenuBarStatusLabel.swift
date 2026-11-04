import SwiftUI
import AppKit

/// Label for `MenuBarExtra` — single menu bar entry (icon reflects daemon state).
struct MenuBarStatusLabel: View {
    @ObservedObject private var stateMonitor = StateMonitor.shared

    var body: some View {
        if AppInstance.isDev {
            Image(nsImage: Self.devImage(symbolName))
                .accessibilityLabel("Always Dev")
        } else {
            Image(systemName: symbolName)
                .accessibilityLabel("Always")
        }
    }

    /// The menu bar draws label images as monochrome templates, ignoring
    /// SwiftUI colors; a non-template image keeps the dev icon orange.
    private static func devImage(_ name: String) -> NSImage {
        let config = NSImage.SymbolConfiguration(hierarchicalColor: .systemOrange)
        let image = NSImage(systemSymbolName: name, accessibilityDescription: "Always Dev")?
            .withSymbolConfiguration(config) ?? NSImage()
        image.isTemplate = false
        return image
    }

    private var symbolName: String {
        StatusIconResolver.symbolName(
            isConnected: stateMonitor.isDaemonConnected,
            isDegraded: stateMonitor.isDaemonDegraded,
            isPaused: stateMonitor.isPaused,
            isTranscribing: stateMonitor.isTranscribing,
            isVoiceActive: stateMonitor.isVoiceActivity
        )
    }
}
