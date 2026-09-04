import SwiftUI
import AppKit

struct AboutPanel: View {
    @ObservedObject private var updateService = UpdateService.shared

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

                // Updates — inline checker
                updateSection

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

    // MARK: - Update section

    @ViewBuilder
    private var updateSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Updates")
                .font(.headline)

            if !updateService.canCheckForUpdates {
                // Sparkle not configured (missing SUPublicEDKey in dev builds)
                Text("Automatic updates are not configured in this build.")
                    .font(.body)
                    .foregroundColor(.secondary)
            } else {
                updateCheckerUI
            }
        }
    }

    @ViewBuilder
    private var updateCheckerUI: some View {
        switch updateService.checkState {
        case .idle:
            HStack(spacing: 10) {
                Button {
                    updateService.checkForUpdates()
                } label: {
                    Label("Check for Updates", systemImage: "arrow.clockwise")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Text("Last checked automatically by Sparkle.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

        case .checking:
            HStack(spacing: 8) {
                ProgressView()
                    .controlSize(.small)
                Text("Checking for updates…")
                    .font(.body)
                    .foregroundColor(.secondary)
            }

        case .upToDate:
            HStack(spacing: 8) {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundColor(.green)
                Text("Always is up to date.")
                    .font(.body)
                Spacer()
                Button("Check Again") { updateService.checkForUpdates() }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            }
            .transition(.opacity)

        case .updateAvailable(let version, let notes):
            updateAvailableCard(version: version, notes: notes)

        case .downloading(let progress):
            downloadProgressCard(progress: progress, label: "Downloading")

        case .extracting(let progress):
            downloadProgressCard(progress: progress, label: "Extracting")

        case .readyToInstall:
            HStack(spacing: 10) {
                Image(systemName: "arrow.triangle.2.circlepath.circle.fill")
                    .foregroundColor(.accentColor)
                Text("Update ready to install.")
                    .font(.body)
                Spacer()
                Button("Install & Relaunch") { updateService.installUpdate() }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
            }

        case .installing:
            HStack(spacing: 8) {
                ProgressView()
                    .controlSize(.small)
                Text("Installing… Always will relaunch shortly.")
                    .font(.body)
                    .foregroundColor(.secondary)
            }

        case .installed:
            HStack(spacing: 8) {
                Image(systemName: "checkmark.seal.fill")
                    .foregroundColor(.green)
                Text("Update installed successfully.")
                    .font(.body)
            }

        case .error(let message):
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundColor(.orange)
                Text(message)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer()
                Button("Retry") { updateService.checkForUpdates() }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            }
        }
    }

    @ViewBuilder
    private func updateAvailableCard(version: String, notes: String?) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                ZStack {
                    Circle()
                        .fill(Color.accentColor.opacity(0.12))
                        .frame(width: 36, height: 36)
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.system(size: 18))
                        .foregroundColor(.accentColor)
                }
                VStack(alignment: .leading, spacing: 2) {
                    Text("Update available — \(version)")
                        .font(.body.weight(.medium))
                    if let notes, !notes.isEmpty {
                        Text(notes)
                            .font(.caption)
                            .foregroundColor(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer()
            }

            HStack(spacing: 8) {
                Button {
                    updateService.installUpdate()
                } label: {
                    Label("Download & Install", systemImage: "arrow.down.circle.fill")
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)

                Button("Later") { updateService.dismissUpdate() }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            }
        }
        .padding(12)
        .background(
            RoundedRectangle(cornerRadius: 10)
                .fill(Color.accentColor.opacity(0.06))
                .overlay(
                    RoundedRectangle(cornerRadius: 10)
                        .stroke(Color.accentColor.opacity(0.15), lineWidth: 1)
                )
        )
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    @ViewBuilder
    private func downloadProgressCard(progress: Double, label: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("\(label)… \(Int(progress * 100))%")
                    .font(.body.weight(.medium))
                Spacer()
            }
            ProgressView(value: progress)
                .progressViewStyle(.linear)
        }
        .padding(12)
        .background(
            RoundedRectangle(cornerRadius: 10)
                .fill(Color.secondary.opacity(0.06))
        )
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
