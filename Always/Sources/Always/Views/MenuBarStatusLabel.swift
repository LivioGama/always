import SwiftUI

/// Label for `MenuBarExtra` — single menu bar entry (icon reflects daemon state).
struct MenuBarStatusLabel: View {
    @ObservedObject private var stateMonitor = StateMonitor.shared

    var body: some View {
        Image(systemName: symbolName)
            .accessibilityLabel("Always")
    }

    private var symbolName: String {
        StatusIconResolver.symbolName(
            isConnected: stateMonitor.isDaemonConnected,
            isDegraded: stateMonitor.isDaemonDegraded,
            isPaused: stateMonitor.isPaused,
            isTranscribing: stateMonitor.isTranscribing
        )
    }
}
