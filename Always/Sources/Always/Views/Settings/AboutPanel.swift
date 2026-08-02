import SwiftUI
import AppKit

struct AboutPanel: View {
    private var appVersion: String {
        Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "unknown"
    }
    let repositoryURL = "https://github.com/LivioGama/always"
    let license = "Apache-2.0"

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                // App Info
                VStack(alignment: .leading, spacing: 8) {
                    Text("Always")
                        .font(.system(size: 32, weight: .bold))
                    Text("Always-on voice activation daemon")
                        .font(.title3)
                        .foregroundColor(.secondary)
                    Text("Version \(appVersion)")
                        .font(.subheadline)
                        .foregroundColor(.secondary)
                }
                .padding(.bottom, 10)

                Divider()

                // Description
                VStack(alignment: .leading, spacing: 8) {
                    Text("About")
                        .font(.headline)
                    Text("Always provides continuous voice transcription using Groq's STT API with intelligent post-processing. It runs as a background daemon and integrates with your macOS workflow.")
                        .font(.body)
                        .foregroundColor(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }

                Divider()

                // Links
                VStack(alignment: .leading, spacing: 8) {
                    Text("Links")
                        .font(.headline)

                    LinkButton(title: "GitHub Repository", url: repositoryURL)
                    LinkButton(title: "Documentation", url: "\(repositoryURL)#readme")
                    LinkButton(title: "Issues", url: "\(repositoryURL)/issues")
                }

                Divider()

                // License
                VStack(alignment: .leading, spacing: 8) {
                    Text("License")
                        .font(.headline)
                    Text(license)
                        .font(.body)
                        .foregroundColor(.secondary)
                }

                Divider()

                // Updates
                VStack(alignment: .leading, spacing: 8) {
                    Text("Updates")
                        .font(.headline)
                    Text("Always uses Sparkle for automatic updates. Check for updates in the app menu.")
                        .font(.body)
                        .foregroundColor(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }

                Spacer()
            }
            .padding(20)
        }
    }

    private struct LinkButton: View {
        let title: String
        let url: String

        var body: some View {
            Button(action: openLink) {
                HStack {
                    Text(title)
                        .foregroundColor(.accentColor)
                    Image(systemName: "arrow.up.right.square")
                        .font(.caption)
                        .foregroundColor(.accentColor)
                }
            }
            .buttonStyle(.plain)
        }

        private func openLink() {
            if let url = URL(string: url) {
                NSWorkspace.shared.open(url)
            }
        }
    }
}
