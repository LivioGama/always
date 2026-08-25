import SwiftUI

private enum LanguageFilter: String, CaseIterable {
    case all = "All"
    case englishOnly = "English"
    case multilingual = "Multilingual"
}

private enum SortOrder: String, CaseIterable {
    case `default` = "Default"
    case best = "Best"
    case accuracy = "Accuracy"
    case speed = "Speed"
}

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
    @Binding var config: Config

    @State private var languageFilter: LanguageFilter = .all
    @State private var translationOnly = false
    @State private var fastOnly = false
    @State private var streamingOnly = false
    @State private var sortOrder: SortOrder = .default

    private var filtersActive: Bool {
        languageFilter != .all || translationOnly || fastOnly || streamingOnly
    }

    private func filtered(_ list: [ModelInfo]) -> [ModelInfo] {
        let filtered = list.filter { model in
            switch languageFilter {
            case .all: break
            case .englishOnly: if model.supportsMultipleLanguages { return false }
            case .multilingual: if !model.supportsMultipleLanguages { return false }
            }
            if translationOnly && !model.supports_translation { return false }
            if fastOnly && model.speed_score < 0.75 { return false }
            if streamingOnly && !model.supports_streaming { return false }
            return true
        }
        switch sortOrder {
        case .default: return filtered
        case .best: return filtered.sorted { ($0.accuracy_score + $0.speed_score) > ($1.accuracy_score + $1.speed_score) }
        case .accuracy: return filtered.sorted { $0.accuracy_score > $1.accuracy_score }
        case .speed: return filtered.sorted { $0.speed_score > $1.speed_score }
        }
    }

    private var filterBar: some View {
        HStack(spacing: 8) {
            Picker("", selection: $languageFilter) {
                ForEach(LanguageFilter.allCases, id: \.self) { f in
                    Text(f.rawValue).tag(f)
                }
            }
            .pickerStyle(.segmented)
            .fixedSize()

            Divider().frame(height: 16)

            Toggle("Translation", isOn: $translationOnly)
                .toggleStyle(.button)
                .controlSize(.small)

            Toggle("Fast", isOn: $fastOnly)
                .toggleStyle(.button)
                .controlSize(.small)

            Toggle("Streaming", isOn: $streamingOnly)
                .toggleStyle(.button)
                .controlSize(.small)

            Divider().frame(height: 16)

            Picker("Sort", selection: $sortOrder) {
                ForEach(SortOrder.allCases, id: \.self) { o in
                    Text(o == .default ? "Sort" : "↓ \(o.rawValue)").tag(o)
                }
            }
            .pickerStyle(.menu)
            .labelsHidden()
            .controlSize(.small)
            .foregroundStyle(sortOrder == .default ? .secondary : .primary)

            if filtersActive || sortOrder != .default {
                Spacer()
                Button("Clear") {
                    languageFilter = .all
                    translationOnly = false
                    fastOnly = false
                    streamingOnly = false
                    sortOrder = .default
                }
                .buttonStyle(.plain)
                .font(.caption)
                .foregroundStyle(.secondary)
            }
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            sectionHeader

            backendBar

            languagePickerSection

            filterBar

            let downloaded = filtered(models.downloadedModels)
            let available = filtered(models.availableModels)

            if !downloaded.isEmpty {
                Text("Downloaded Models")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.top, 4)
                VStack(spacing: 8) {
                    ForEach(downloaded) { model in
                        ModelRow(model: model, isDownloaded: true)
                    }
                }
            }

            if !available.isEmpty {
                Text("Available to Download")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.top, 8)
                VStack(spacing: 8) {
                    ForEach(available) { model in
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
            } else if filtersActive && downloaded.isEmpty && available.isEmpty {
                Text("No models match the current filters.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
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

    /// The catalog entry for the currently active local model, or `nil`
    /// when the active backend is Groq or the catalog hasn't loaded it yet.
    private var activeModel: ModelInfo? {
        guard let id = models.activeModelId else { return nil }
        return models.models.first(where: { $0.id == id })
    }

    /// Target-language picker — only appears for an engine that can bake a
    /// fixed language into its decode (`supports_language_selection`) and
    /// actually offers more than one option. Mirrors `ModelInfo.languageLabel`'s
    /// gating condition.
    @ViewBuilder
    private var languagePickerSection: some View {
        if let model = activeModel,
           model.supports_language_selection,
           model.supported_languages.count > 1 {
            HStack(spacing: 10) {
                Text("Language:")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                Picker("", selection: languageBinding) {
                    Text("Auto-detect").tag(Optional<String>.none)
                    ForEach(model.supported_languages, id: \.self) { code in
                        Text(languageDisplayName(for: code)).tag(Optional(code))
                    }
                }
                .pickerStyle(.menu)
                .labelsHidden()
                .frame(width: 220)
                Spacer()
            }
            .padding(.vertical, 2)
        }
    }

    /// `nil` selection means auto-detect, mirrored on the wire as `"auto"`
    /// (see `Config.lang`'s doc comment and `StateMonitor.setLanguage`).
    /// Setting immediately pushes the daemon's live `SetLanguage` command —
    /// there's no separate save step, the daemon persists it itself.
    private var languageBinding: Binding<String?> {
        Binding(
            get: {
                let lang = config.lang
                return (lang == nil || lang == "auto") ? nil : lang
            },
            set: { newValue in
                config.lang = newValue ?? "auto"
                StateMonitor.shared.setLanguage(newValue)
            }
        )
    }

    /// Localized language name for a plain ISO 639-1 code ("en" → "English").
    /// Falls back to the uppercased code if the current locale has no name
    /// for it.
    private func languageDisplayName(for code: String) -> String {
        Locale.current.localizedString(forLanguageCode: code)?.capitalized(with: .current) ?? code.uppercased()
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
                if let streaming = model.streamingLabel {
                    Label(streaming, systemImage: "dot.radiowaves.left.and.right")
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
            } else if manager.verifying.contains(model.id) || model.isVerifying {
                infoLine(systemImage: "shield.checkered", text: "Verifying checksum…")
            } else if manager.extracting.contains(model.id) {
                infoLine(systemImage: "shippingbox", text: "Extracting archive…")
            } else if manager.installing.contains(model.id) {
                infoLine(systemImage: "arrow.down.app.fill", text: "Installing…")
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
            if isActive {
                Text("Active")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            } else {
                let isLoading = manager.loadingBackend == "local:\(model.id)"
                if isLoading {
                    HStack(spacing: 4) {
                        ProgressView()
                            .controlSize(.small)
                        Text("Loading…")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                } else {
                    Button("Use") {
                        manager.setActive(backend: "local:\(model.id)")
                    }
                    .controlSize(.small)
                    .disabled(manager.loadingBackend != nil)
                }
            }
            Button(role: .destructive) {
                manager.delete(model.id)
            } label: {
                Image(systemName: "trash")
            }
            .controlSize(.small)
            // Can't delete the model you're currently transcribing with —
            // it would yank the active engine out from under the daemon.
            // Switch to another model (or Groq) first.
            .disabled(isActive || manager.loadingBackend != nil)
            .help(isActive
                ? "Switch to another model before deleting \(model.name)"
                : "Delete \(model.name)")
        }
    }

    @ViewBuilder
    private var actionsForAvailable: some View {
        let isInstalling = manager.installing.contains(model.id)
        let inFlight = manager.downloadProgress[model.id] != nil
            || manager.verifying.contains(model.id)
            || manager.extracting.contains(model.id)
        if isInstalling {
            ProgressView()
                .controlSize(.small)
        } else if inFlight {
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
                // On macOS 26 `.tint(.accentColor.opacity(...))` triggers
                // infinite recursion inside Color.AccentColorProvider.resolve
                // when the row is laid out. Plain `.accentColor` is safe.
                .tint(.accentColor)
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
