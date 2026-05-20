import SwiftUI

/// "Transcription Models" section — Handy-style catalog of local
/// engines plus the always-available remote Groq backend.
///
/// Visual hierarchy mirrors Handy 0.8.3:
///
/// * Downloaded models on top, with the active one badged.
/// * Available models below, each with size + download button.
/// * Backend toggle at the very top so users can flip to the remote
///   Groq API without scrolling past the catalog.
struct ModelsSection: View {
    @ObservedObject private var models: ModelManagerClient = .shared

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            sectionHeader

            backendBar

            if !models.downloadedModels.isEmpty {
                Text("Downloaded Models")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.top, 4)
                VStack(spacing: 8) {
                    ForEach(models.downloadedModels) { model in
                        ModelRow(model: model, isDownloaded: true)
                    }
                }
            }

            if !models.availableModels.isEmpty {
                Text("Available to Download")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.top, 8)
                VStack(spacing: 8) {
                    ForEach(models.availableModels) { model in
                        ModelRow(model: model, isDownloaded: false)
                    }
                }
            }

            if models.models.isEmpty {
                HStack(spacing: 8) {
                    ProgressView()
                        .controlSize(.small)
                    Text("Waiting for model catalog from daemon…")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .padding(.vertical, 6)
            }
        }
        .onAppear { models.requestModelsList() }
    }

    private var sectionHeader: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("Transcription Models")
                .font(.headline)
            Text("Select a transcription model or download additional models. Different models offer varying levels of accuracy and speed.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    /// Toggle between remote Groq and the currently-selected local
    /// model. Falls back to "no local model yet" when nothing is
    /// downloaded.
    private var backendBar: some View {
        HStack(spacing: 10) {
            Text("Active:")
                .font(.subheadline)
                .foregroundStyle(.secondary)
            Text(activeLabel)
                .font(.subheadline)
                .fontWeight(.semibold)
            Spacer()
            if models.activeBackend != "groq" {
                Button("Use Groq (Remote)") {
                    models.setActive(backend: "groq")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
        }
        .padding(.vertical, 6)
        .padding(.horizontal, 10)
        .background(
            RoundedRectangle(cornerRadius: 6)
                .fill(Color.secondary.opacity(0.08))
        )
    }

    private var activeLabel: String {
        if let id = models.activeModelId,
           let m = models.models.first(where: { $0.id == id }) {
            return m.name
        }
        if models.activeBackend == "groq" {
            return "Groq Whisper (Remote)"
        }
        return models.activeBackend
    }
}

// MARK: - Row

private struct ModelRow: View {
    let model: ModelInfo
    let isDownloaded: Bool
    @ObservedObject private var manager: ModelManagerClient = .shared

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(model.name)
                            .font(.subheadline)
                            .fontWeight(.semibold)
                        if isActive {
                            Text("Active")
                                .font(.caption2)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(
                                    RoundedRectangle(cornerRadius: 4)
                                        .fill(Color.accentColor.opacity(0.25))
                                )
                        } else if model.is_recommended {
                            Text("Recommended")
                                .font(.caption2)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(
                                    RoundedRectangle(cornerRadius: 4)
                                        .stroke(Color.secondary.opacity(0.4))
                                )
                        }
                    }
                    Text(model.description)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if !model.hidesScores {
                    scoreColumn
                }
            }

            // Metadata row — language label, translate badge, size.
            HStack(spacing: 8) {
                Label(model.languageLabel, systemImage: "globe")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                if let translate = model.translateLabel {
                    Label(translate, systemImage: "character.bubble")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if isDownloaded {
                    actionsForDownloaded
                } else {
                    actionsForAvailable
                }
            }

            // Progress + status — only shown while something is in flight.
            if let progress = manager.downloadProgress[model.id] {
                downloadStatusBar(progress: progress)
            } else if manager.verifying.contains(model.id) {
                infoLine(systemImage: "shield.checkered", text: "Verifying checksum…")
            } else if manager.extracting.contains(model.id) {
                infoLine(systemImage: "shippingbox", text: "Extracting archive…")
            } else if let error = manager.errors[model.id] {
                infoLine(
                    systemImage: "exclamationmark.triangle.fill",
                    text: error,
                    color: .orange
                )
            }
        }
        .padding(10)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .stroke(isActive ? Color.accentColor.opacity(0.6) : Color.secondary.opacity(0.2))
        )
    }

    private var isActive: Bool {
        manager.activeModelId == model.id
    }

    private var actionsForDownloaded: some View {
        HStack(spacing: 8) {
            if !isActive {
                Button("Use") {
                    manager.setActive(backend: "local:\(model.id)")
                }
                .controlSize(.small)
            }
            Button(role: .destructive) {
                manager.delete(model.id)
            } label: {
                Image(systemName: "trash")
            }
            .controlSize(.small)
            .help("Delete \(model.name)")
        }
    }

    @ViewBuilder
    private var actionsForAvailable: some View {
        let inFlight = manager.downloadProgress[model.id] != nil
            || manager.verifying.contains(model.id)
            || manager.extracting.contains(model.id)
        if inFlight {
            Button("Cancel") { manager.cancelDownload(model.id) }
                .controlSize(.small)
        } else {
            Button {
                manager.download(model.id)
            } label: {
                Label(model.sizeLabel, systemImage: "arrow.down.circle")
            }
            .controlSize(.small)
        }
    }

    /// Minimal accuracy/speed bars to echo Handy's visual at a fraction
    /// of the visual weight — we only have flat scores, not real
    /// benchmarks, so keep them compact.
    private var scoreColumn: some View {
        VStack(alignment: .trailing, spacing: 2) {
            scoreBar(label: "accuracy", value: model.accuracy_score)
            scoreBar(label: "speed", value: model.speed_score)
        }
    }

    private func scoreBar(label: String, value: Float) -> some View {
        HStack(spacing: 6) {
            Text(label)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .frame(width: 56, alignment: .trailing)
            ProgressView(value: Double(value))
                .progressViewStyle(.linear)
                .frame(width: 70)
                .tint(.accentColor.opacity(0.8))
        }
    }

    private func downloadStatusBar(progress: Double) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            ProgressView(value: progress, total: 100)
                .progressViewStyle(.linear)
            HStack {
                Text(String(format: "%.0f%%", progress))
                Spacer()
                if let (downloaded, total) = manager.downloadBytes[model.id],
                   total > 0 {
                    Text("\(byteString(downloaded)) / \(byteString(total))")
                }
            }
            .font(.caption2)
            .foregroundStyle(.secondary)
        }
    }

    private func infoLine(systemImage: String, text: String, color: Color = .secondary) -> some View {
        HStack(spacing: 6) {
            Image(systemName: systemImage)
            Text(text)
        }
        .font(.caption2)
        .foregroundStyle(color)
    }

    private func byteString(_ bytes: UInt64) -> String {
        let formatter = ByteCountFormatter()
        formatter.allowedUnits = [.useMB, .useGB]
        formatter.countStyle = .file
        return formatter.string(fromByteCount: Int64(bytes))
    }
}
