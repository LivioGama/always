import SwiftUI
import AppKit

struct AboutPanel: View {
    private var appVersion: String {
        Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "unknown"
    }
    let repositoryURL = "https://github.com/LivioGama/always"
    let donateURL = "https://donate.stripe.com/aFa7sN1jZdHD6b82dY4Ja00"
    let license = "AGPL-3.0"

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                // App Info
                VStack(alignment: .leading, spacing: 8) {
                    Text("Always")
                        .font(.system(size: 32, weight: .bold))
                    Text("The first speech-to-text software designed to be always on and free your hands from the affordance")
                        .font(.title3)
                        .foregroundColor(.secondary)
                    Text("Version \(appVersion)")
                        .font(.subheadline)
                        .foregroundColor(.secondary)
                }
                .padding(.bottom, 10)

                Divider()

                // Updates — daemon-driven (SPEC §12a)
                UpdateSection()

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

                    HStack(spacing: 10) {
                        LinkButton(title: "GitHub Repository", systemImage: "globe", url: repositoryURL)
                        LinkButton(title: "Documentation", systemImage: "book", url: "\(repositoryURL)#readme")
                        LinkButton(title: "Issues", systemImage: "ant", url: "\(repositoryURL)/issues")
                    }
                }

                Divider()

                // Support Development
                VStack(alignment: .leading, spacing: 8) {
                    Text("Support Development")
                        .font(.headline)
                    Text("Always is free and open source. If it saves you time, consider supporting its development.")
                        .font(.body)
                        .foregroundColor(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    LinkButton(title: "Donate", systemImage: "heart.fill", url: donateURL, prominent: true)
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

                Spacer()
            }
            .padding(20)
        }
    }

    private struct LinkButton: View {
        let title: String
        let systemImage: String
        let url: String
        var prominent: Bool = false

        @ViewBuilder
        private var button: some View {
            Button(action: openLink) {
                Label(title, systemImage: systemImage)
            }
        }

        var body: some View {
            if prominent {
                button
                    .buttonStyle(.borderedProminent)
                    .controlSize(.regular)
            } else {
                button
                    .buttonStyle(.bordered)
                    .controlSize(.regular)
            }
        }

        private func openLink() {
            if let url = URL(string: url) {
                NSWorkspace.shared.open(url)
            }
        }
    }
}
