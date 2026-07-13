import SwiftUI

enum SettingsPanel: String, CaseIterable {
    case general = "General"
    case models = "Models"
    case myVoice = "My Voice"
    case permissions = "Permissions"
    case behavior = "Behavior"
    case shortcuts = "Shortcuts"
    case vocabulary = "Vocabulary"
    case snippets = "Snippets"
    case history = "History"
    case about = "About"

    var symbol: String {
        switch self {
        case .general:    return "app.badge.checkmark"
        case .models:     return "cpu"
        case .myVoice:    return "person.wave.2"
        case .permissions: return "checkmark.shield"
        case .behavior:   return "slider.horizontal.3"
        case .shortcuts:  return "command"
        case .vocabulary: return "character.book.closed"
        case .snippets:   return "text.insert"
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
        Button {
            selectedPanel = panel
        } label: {
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
            // Make the ENTIRE row tappable, not just the text glyphs. A
            // `.plain` Button only hit-tests its opaque content, so the
            // Spacer + padding (the empty area right of the label) ignored
            // clicks. An explicit rectangular content shape claims the full
            // padded row as the hit target.
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 8)
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
