import SwiftUI

/// About panel → "Updates". Renders `UpdateService.state`, the daemon's
/// update state, with "Check now" and "Install update" buttons.
struct UpdateSection: View {
    @ObservedObject private var updates = UpdateService.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Updates")
                .font(.headline)
            content
        }
    }

    @ViewBuilder
    private var content: some View {
        switch updates.state {
        case .unknown:
            statusRow(
                icon: "arrow.triangle.2.circlepath",
                color: .secondary,
                text: updates.isDaemonConnected
                    ? "Always checks GitHub for new releases once a day."
                    : "Waiting for the Always daemon…"
            )

        case .checking:
            HStack(spacing: 8) {
                ProgressView()
                    .controlSize(.small)
                Text("Checking for updates…")
                    .font(.body)
                    .foregroundColor(.secondary)
            }

        case .upToDate(let version):
            statusRow(
                icon: "checkmark.circle.fill",
                color: .green,
                text: "Always is up to date (v\(version))."
            )

        case .noRelease:
            statusRow(icon: "info.circle", color: .secondary, text: "No release published yet.")

        case .checkFailed(let reason):
            statusRow(
                icon: "exclamationmark.circle",
                color: .secondary,
                text: "Couldn't check for updates (\(reason))."
            )

        case .available(let offer):
            availableCard(offer)

        case .installing(let phase):
            installingCard(phase)

        case .failed(_, let message):
            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .top, spacing: 8) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundColor(.orange)
                    Text(message)
                        .font(.caption)
                        .foregroundColor(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                HStack(spacing: 8) {
                    if let offer = updates.offer, updates.canInstall {
                        installButton(offer, title: "Try again")
                    }
                    checkButton
                }
            }
        }
    }

    // MARK: Rows and cards

    private func statusRow(icon: String, color: Color, text: String) -> some View {
        HStack(spacing: 8) {
            Image(systemName: icon)
                .foregroundColor(color)
            Text(text)
                .font(.body)
                .foregroundColor(color == .secondary ? .secondary : .primary)
            Spacer()
            checkButton
        }
    }

    private var checkButton: some View {
        Button("Check now") { updates.checkNow() }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(!updates.canCheck)
    }

    private func installButton(_ offer: UpdateOffer, title: String = "Install update") -> some View {
        Button {
            UpdateInstallPrompt.present(for: offer)
        } label: {
            Label(title, systemImage: "arrow.down.circle.fill")
        }
        .buttonStyle(.borderedProminent)
        .controlSize(.small)
        .disabled(!updates.canInstall)
    }

    private func availableCard(_ offer: UpdateOffer) -> some View {
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
                    Text("Update available — v\(offer.current) → v\(offer.latest)")
                        .font(.body.weight(.medium))
                    if !offer.canApply, !offer.applyHint.isEmpty {
                        Text(offer.applyHint)
                            .font(.caption)
                            .foregroundColor(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    if let url = offer.releaseURL {
                        Link("Release notes", destination: url)
                            .font(.caption)
                    }
                }
                Spacer()
            }

            HStack(spacing: 8) {
                installButton(offer)
                checkButton
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
    private func installingCard(_ phase: UpdateInstallPhase) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            switch phase {
            case .downloading(let percent):
                Text(percent.map { "Downloading… \($0)%" } ?? "Downloading…")
                    .font(.body.weight(.medium))
                if let percent {
                    ProgressView(value: Double(percent), total: 100)
                        .progressViewStyle(.linear)
                } else {
                    ProgressView()
                        .progressViewStyle(.linear)
                }
            case .verifying:
                spinnerRow("Verifying signature…")
            case .handoff:
                spinnerRow("Installing… Always will relaunch shortly.")
            }
        }
        .padding(12)
        .background(
            RoundedRectangle(cornerRadius: 10)
                .fill(Color.secondary.opacity(0.06))
        )
    }

    private func spinnerRow(_ text: String) -> some View {
        HStack(spacing: 8) {
            ProgressView()
                .controlSize(.small)
            Text(text)
                .font(.body.weight(.medium))
        }
    }
}
