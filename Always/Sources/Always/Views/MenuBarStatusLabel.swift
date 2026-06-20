import SwiftUI

/// Label for `MenuBarExtra` — single menu bar entry (icon reflects daemon state).
struct MenuBarStatusLabel: View {
    @ObservedObject private var stateMonitor = StateMonitor.shared

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: symbolName)
            Text("Always")
                .font(.system(size: 12, weight: .medium))
        }
        .lineLimit(1)
        .fixedSize()
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
