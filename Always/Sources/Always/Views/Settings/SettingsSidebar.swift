import SwiftUI

enum SettingsPanel: String, CaseIterable {
    case general = "General"
    case models = "Models"
    case advanced = "Advanced"
    case about = "About"
}

struct SettingsSidebar: View {
    @Binding var selectedPanel: SettingsPanel

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(SettingsPanel.allCases, id: \.self) { panel in
                sidebarItem(for: panel)
            }
            Spacer()
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
            HStack {
                Text(panel.rawValue)
                    .font(.body)
                    .foregroundColor(selectedPanel == panel ? .white : .primary)
                Spacer()
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
            .background(
                selectedPanel == panel
                    ? Color.accentColor
                    : Color.clear
            )
            .cornerRadius(6)
        }
        .buttonStyle(.plain)
    }
}
