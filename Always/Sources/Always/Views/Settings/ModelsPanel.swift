import SwiftUI

struct ModelsPanel: View {
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                ModelsSection()
            }
            .padding(20)
        }
    }
}
