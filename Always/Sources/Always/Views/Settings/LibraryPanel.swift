import SwiftUI

/// Merged home for the user-curated text libraries: glossary terms
/// (vocabulary bias) and voice snippets (text expansion). A segmented
/// control at the top switches between the two existing panels, which
/// keep their own scroll and persistence logic unchanged.
enum LibraryTab: String, CaseIterable {
    case vocabulary = "Vocabulary"
    case snippets = "Snippets"
}

struct LibraryPanel: View {
    @State private var selectedTab: LibraryTab = .vocabulary

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Spacer()
                Picker("", selection: $selectedTab) {
                    ForEach(LibraryTab.allCases, id: \.self) { tab in
                        Text(tab.rawValue).tag(tab)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(maxWidth: 360)
                Spacer()
            }
            .padding(.top, 16)
            .padding(.bottom, 4)

            Group {
                switch selectedTab {
                case .vocabulary:
                    VocabularyPanel()
                case .snippets:
                    SnippetsPanel()
                }
            }
        }
    }
}
