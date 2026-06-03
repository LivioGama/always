import SwiftUI

enum SettingsPanel: String, CaseIterable {
    case general = "General"
    case models = "Models"
    case behavior = "Behavior"
    case shortcuts = "Shortcuts"
    case vocabulary = "Vocabulary"
    case history = "History"
    case about = "About"

    var symbol: String {
        switch self {
        case .general:    return "app.badge.checkmark"
        case .models:     return "cpu"
        case .behavior:   return "slider.horizontal.3"
        case .shortcuts:  return "command"
        case .vocabulary: return "character.book.closed"
        case .history:    return "clock.fill"
        case .about:      return "info.circle"
        }
    }
}

struct SettingsSidebar: View {
    @Binding var selectedPanel: SettingsPanel
    @ObservedObject var stateMonitor: StateMonitor

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(SettingsPanel.allCases, id: \.self) { panel in
                sidebarItem(for: panel)
            }
            Spacer()
            Divider().padding(.horizontal, 12)
            statusFooter
        }
        .padding(.vertical, 20)
        .frame(width: 200)
        .background(Color(NSColor.windowBackgroundColor))
    }

    @ViewBuilder
    private func sidebarItem(for panel: SettingsPanel) -> some View {
        HStack(spacing: 10) {
            Image(systemName: panel.symbol)
                .frame(width: 18)
                .foregroundColor(selectedPanel == panel ? .white : .secondary)
            Text(panel.rawValue)
                .font(.body)
                .foregroundColor(selectedPanel == panel ? .white : .primary)
            Spacer()
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .background(
            selectedPanel == panel
                ? Color.accentColor
                : Color.clear
        )
        .cornerRadius(6)
        .padding(.horizontal, 8)
        // `contentShape` makes the entire row hit-testable — without it
        // SwiftUI only catches taps on the Image/Text glyphs, not the
        // Spacer or padding area, which the user perceives as dead zones.
        .contentShape(Rectangle())
        .onTapGesture {
            selectedPanel = panel
        }
    }

    private var statusFooter: some View {
        let isRunning = stateMonitor.isDaemonConnected
        let isDegraded = stateMonitor.isDaemonDegraded
        let label: String
        let color: Color
        let symbol: String
        if isRunning && !isDegraded {
            label = "Running"
            color = .green
            symbol = "checkmark.circle.fill"
        } else if isDegraded {
            label = "Reconnecting…"
            color = .orange
            symbol = "arrow.triangle.2.circlepath"
        } else {
            label = "Disconnected"
            color = .orange
            symbol = "exclamationmark.triangle.fill"
        }
        return HStack(spacing: 8) {
            Image(systemName: symbol)
                .foregroundColor(color)
            Text(label)
                .font(.caption)
                .foregroundColor(color)
            Spacer()
        }
        .padding(.horizontal, 16)
        .padding(.top, 4)
    }
}
