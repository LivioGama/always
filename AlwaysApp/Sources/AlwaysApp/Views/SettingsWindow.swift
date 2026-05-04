import SwiftUI
import AppKit

// MARK: - Shortcut formatting helpers

/// Format `"ctrl+alt+p"` → `"⌃⌥P"` for display.
func formatShortcut(_ s: String) -> String {
    let symbolMap: [String: String] = [
        "ctrl": "⌃", "control": "⌃",
        "alt": "⌥", "option": "⌥",
        "shift": "⇧",
        "meta": "⌘", "cmd": "⌘", "command": "⌘"
    ]
    let parts = s.lowercased().split(separator: "+").map(String.init)
    return parts.map { symbolMap[$0] ?? $0.uppercased() }.joined()
}

// MARK: - Key capture button

struct KeyCaptureButton: View {
    let label: String
    @Binding var shortcut: String
    let onSave: (String) async -> Void

    @State private var isRecording = false
    @State private var monitor: Any?

    var body: some View {
        HStack {
            Text(label)
            Spacer()
            Button(action: toggleRecording) {
                Text(isRecording ? "Press keys…" : formatShortcut(shortcut))
                    .monospacedDigit()
                    .foregroundColor(isRecording ? .orange : .secondary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .background(isRecording ? Color.orange.opacity(0.08) : Color.clear)
                    .overlay(
                        RoundedRectangle(cornerRadius: 5)
                            .stroke(
                                isRecording ? Color.orange : Color.secondary.opacity(0.35),
                                lineWidth: 1
                            )
                    )
                    .cornerRadius(5)
            }
            .buttonStyle(.plain)
        }
    }

    private func toggleRecording() {
        if isRecording { stopRecording(); return }
        isRecording = true
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            let mods = event.modifierFlags
            var parts: [String] = []
            if mods.contains(.control) { parts.append("ctrl") }
            if mods.contains(.option)  { parts.append("alt") }
            if mods.contains(.shift)   { parts.append("shift") }
            if mods.contains(.command) { parts.append("meta") }

            let keyChar = event.charactersIgnoringModifiers?.lowercased() ?? ""
            // require at least one modifier + a single printable key
            if !keyChar.isEmpty, keyChar.count == 1, !parts.isEmpty {
                let newShortcut = (parts + [keyChar]).joined(separator: "+")
                shortcut = newShortcut
                Task { await onSave(newShortcut) }
            }
            stopRecording()
            return nil
        }
    }

    private func stopRecording() {
        isRecording = false
        if let m = monitor { NSEvent.removeMonitor(m); monitor = nil }
    }
}

// MARK: - Sensitivity preset
//
// Mirrors `SensitivityPreset` in `src/always/config.rs`. The threshold
// pairs MUST stay in sync — both sides write the same daemon
// preferences. Adding a new variant requires updating both files.

enum SensitivityPreset: String, CaseIterable, Identifiable {
    case high
    case normal
    case low

    var id: String { rawValue }

    var label: String {
        switch self {
        case .high:   return "High"
        case .normal: return "Normal"
        case .low:    return "Low"
        }
    }

    /// `(stt_energy_threshold, hear_energy_threshold)`.
    var thresholds: (stt: Double, hear: Double) {
        switch self {
        case .high:   return (0.005, 0.0005)
        case .normal: return (0.012, 0.001)
        case .low:    return (0.025, 0.002)
        }
    }

    /// Reverse-lookup: which preset (if any) corresponds to the given
    /// raw thresholds. Returns `nil` for custom values so the picker
    /// can fall through to "Custom".
    static func from(stt: Double, hear: Double) -> SensitivityPreset? {
        for p in SensitivityPreset.allCases {
            let (s, h) = p.thresholds
            if abs(s - stt) < 1e-6 && abs(h - hear) < 1e-6 {
                return p
            }
        }
        return nil
    }
}

// MARK: - Numeric setting row

/// One sensitivity setting laid out as a single row:
///   Label                                  [Number ⊕]  Default: x.xxxx  ⤺
///
/// Replaces the old slider style with a typed number input plus a reset
/// button. The recommended default is shown alongside so users know what
/// the safe value is. `recommended` is the value users should prefer
/// (== `default` here, but kept separate so a future PR can recommend a
/// per-environment override without touching the type-level default).
struct NumericSettingRow<T: Numeric & LosslessStringConvertible>: View where T: Comparable {
    let title: String
    let help: String
    let unit: String
    let formatter: NumberFormatter
    @Binding var value: T
    let defaultValue: T
    let range: ClosedRange<T>?

    init(
        title: String,
        help: String = "",
        unit: String = "",
        formatter: NumberFormatter,
        value: Binding<T>,
        defaultValue: T,
        range: ClosedRange<T>? = nil
    ) {
        self.title = title
        self.help = help
        self.unit = unit
        self.formatter = formatter
        self._value = value
        self.defaultValue = defaultValue
        self.range = range
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                    .font(.body)
                if !help.isEmpty {
                    Text(help)
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
            }
            Spacer(minLength: 8)
            HStack(spacing: 4) {
                TextField("", value: $value, formatter: formatter)
                    .textFieldStyle(.roundedBorder)
                    .multilineTextAlignment(.trailing)
                    .frame(width: 80)
                    .onChange(of: value) { _, new in
                        if let range, !range.contains(new) {
                            value = min(max(new, range.lowerBound), range.upperBound)
                        }
                    }
                if !unit.isEmpty {
                    Text(unit)
                        .font(.caption)
                        .foregroundColor(.secondary)
                        .frame(width: 22, alignment: .leading)
                }
            }
            Text("Default: \(formatter.string(for: defaultValue) ?? "")")
                .font(.caption2)
                .foregroundColor(.secondary)
                .frame(width: 100, alignment: .trailing)
            Button {
                value = defaultValue
            } label: {
                Image(systemName: "arrow.counterclockwise")
            }
            .buttonStyle(.borderless)
            .help("Reset to recommended default")
        }
    }
}

// MARK: - Settings window

struct SettingsWindow: View {
    @ObservedObject var cliService: CLIService
    @ObservedObject private var stateMonitor: StateMonitor = .shared
    @State private var config: Config = Config.defaultConfig
    @State private var status: DaemonStatus?
    @State private var isLoading = false
    @State private var apiKey: String = ""
    @State private var showApiKey = false
    @State private var isSavingApiKey = false
    @FocusState private var focusedField: Field?
    @State private var vocabularyTermCount: Int = 0
    @State private var vocabularyPath: String = ""

    enum Field {
        case apiKey
    }

    // ----- Number formatters used by the sensitivity rows. -----
    private static let energyFormatter: NumberFormatter = {
        let f = NumberFormatter()
        f.numberStyle = .decimal
        f.minimumFractionDigits = 4
        f.maximumFractionDigits = 4
        f.minimum = 0
        f.maximum = 1
        return f
    }()

    private static let secondsFormatter: NumberFormatter = {
        let f = NumberFormatter()
        f.numberStyle = .decimal
        f.minimumFractionDigits = 1
        f.maximumFractionDigits = 2
        f.minimum = 0
        f.maximum = 10
        return f
    }()

    private static let intFormatter: NumberFormatter = {
        let f = NumberFormatter()
        f.numberStyle = .none
        f.minimum = 0
        f.maximum = 60_000
        return f
    }()

    private static let probabilityFormatter: NumberFormatter = {
        let f = NumberFormatter()
        f.numberStyle = .decimal
        f.minimumFractionDigits = 2
        f.maximumFractionDigits = 2
        f.minimum = 0
        f.maximum = 1
        return f
    }()

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            statusAndPauseRow
            Divider()
            behaviorRow
            Divider()
            apiSection
            Divider()
            vocabularySection
            Divider()
            sensitivitySection
            Divider()
            shortcutsSection
        }
        .padding(20)
        .frame(width: 620)
        .fixedSize(horizontal: false, vertical: true)
        .onAppear {
            focusedField = nil
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
                focusedField = nil
            }
            refreshVocabularyInfo()
            Task {
                await loadConfig()
                await refreshStatus()
            }
        }
        .onChange(of: config.sttEnergyThreshold) { _, _ in saveConfig() }
        .onChange(of: config.hearEnergyThreshold) { _, _ in saveConfig() }
        .onChange(of: config.sttSilence) { _, _ in saveConfig() }
        .onChange(of: config.sttCooldownMs) { _, _ in saveConfig() }
        .onChange(of: config.sileroThreshold) { _, _ in saveConfig() }
        .onChange(of: config.postprocessEnabled) { _, _ in saveConfig() }
    }

    // MARK: Sections

    private var statusAndPauseRow: some View {
        HStack(spacing: 12) {
            Image(
                systemName: status?.isRunning == true
                    ? "checkmark.circle.fill"
                    : "exclamationmark.triangle.fill"
            )
            .foregroundColor(status?.isRunning == true ? .green : .orange)
            Text(status?.isRunning == true ? "Running" : "Reconnecting…")
                .font(.headline)
                .foregroundColor(status?.isRunning == true ? .green : .orange)
            Spacer()
            Button {
                stateMonitor.togglePause()
            } label: {
                Label(
                    stateMonitor.isPaused ? "Resume" : "Pause",
                    systemImage: stateMonitor.isPaused
                        ? "play.circle.fill"
                        : "pause.circle.fill"
                )
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(status?.isRunning != true)
        }
    }

    private var behaviorRow: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Toggle(
                    "Auto-Enter After Paste",
                    isOn: Binding(
                        get: { stateMonitor.isAutoEnter },
                        set: { _ in stateMonitor.toggleAutoEnter() }
                    )
                )
                Spacer()
            }
            HStack {
                Toggle(
                    "Smart Postprocess (LLM grammar + glossary fixes)",
                    isOn: $config.postprocessEnabled
                )
                Spacer()
            }
        }
    }

    private var apiSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("API Configuration").font(.headline)
            HStack {
                ZStack(alignment: .trailing) {
                    Group {
                        if showApiKey {
                            TextField("Groq API Key", text: $apiKey)
                                .textFieldStyle(.roundedBorder)
                        } else {
                            SecureField("Groq API Key", text: $apiKey)
                                .textFieldStyle(.roundedBorder)
                        }
                    }
                    .focused($focusedField, equals: .apiKey)
                    .onChange(of: apiKey) { _, newValue in
                        config.groqApiKey = newValue.isEmpty ? nil : newValue
                    }
                    if isSavingApiKey {
                        ProgressView()
                            .scaleEffect(0.6)
                            .frame(width: 14, height: 14)
                            .padding(.trailing, 8)
                    }
                }

                Button {
                    showApiKey.toggle()
                    focusedField = nil
                } label: {
                    Image(systemName: showApiKey ? "eye.slash" : "eye")
                }
                .buttonStyle(.borderless)

                Button {
                    saveApiKey()
                } label: {
                    Image(systemName: "arrow.down.doc")
                }
                .buttonStyle(.borderless)
                .help("Save API Key")
                .disabled(isSavingApiKey)
            }
            Text("Required for transcription")
                .font(.caption2)
                .foregroundColor(.secondary)
        }
    }

    private var vocabularySection: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Vocabulary").font(.headline)
                Spacer()
                if vocabularyTermCount > 0 {
                    Text("\(vocabularyTermCount) terms")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
            HStack(spacing: 8) {
                Button(action: openVocabularyFile) {
                    Label("Open glossary.json", systemImage: "doc.text")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)

                Button(action: revealVocabularyInFinder) {
                    Label("Show in Finder", systemImage: "folder")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)

                Button(action: refreshVocabularyInfo) {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(.borderless)
                .help("Refresh vocabulary info")
                Spacer()
            }
            if !vocabularyPath.isEmpty {
                Text(vocabularyPath)
                    .font(.system(.caption2, design: .monospaced))
                    .foregroundColor(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
        }
    }

    private var sensitivitySection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Sensitivity").font(.headline)

            // Mic Sensitivity preset — the high-level control most users
            // ever touch. Internally writes the same thresholds as the
            // CLI `always config preset <level>` command.
            HStack {
                Text("Mic Sensitivity")
                    .font(.body)
                Spacer()
                Picker("", selection: presetBinding) {
                    ForEach(SensitivityPreset.allCases) { preset in
                        Text(preset.label).tag(Optional(preset))
                    }
                    if currentPreset == nil {
                        Text("Custom").tag(Optional<SensitivityPreset>.none)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 280)
            }

            // Raw advanced controls — collapsed by default. Power users
            // and the CLI keep full per-knob access.
            DisclosureGroup("Advanced") {
                VStack(alignment: .leading, spacing: 8) {
                    NumericSettingRow(
                        title: "STT Energy Threshold",
                        help: "Lower = more sensitive to quiet speech",
                        formatter: Self.energyFormatter,
                        value: $config.sttEnergyThreshold,
                        defaultValue: 0.012,
                        range: 0.0001...0.5
                    )

                    NumericSettingRow(
                        title: "Hear Energy Threshold",
                        help: "Lower = picks up quieter background voice",
                        formatter: Self.energyFormatter,
                        value: $config.hearEnergyThreshold,
                        defaultValue: 0.001,
                        range: 0.0001...0.5
                    )

                    NumericSettingRow(
                        title: "Silence Timeout",
                        help: "Seconds of silence before utterance ends",
                        unit: "s",
                        formatter: Self.secondsFormatter,
                        value: $config.sttSilence,
                        defaultValue: 2.0,
                        range: 0.1...10
                    )

                    NumericSettingRow(
                        title: "Cooldown",
                        help: "Min ms between consecutive pastes",
                        unit: "ms",
                        formatter: Self.intFormatter,
                        value: $config.sttCooldownMs,
                        defaultValue: 150,
                        range: 0...60_000
                    )

                    NumericSettingRow(
                        title: "Silero VAD Threshold",
                        help: "Higher = stricter speech detection",
                        formatter: Self.probabilityFormatter,
                        value: $config.sileroThreshold,
                        defaultValue: 0.5,
                        range: 0...1
                    )
                }
                .padding(.top, 6)
            }
        }
    }

    /// Two-way binding between the segmented picker and the raw
    /// thresholds. Reads compute the current preset (or `nil` for custom
    /// values); writes apply the preset's threshold pair to both raw
    /// fields, which trips the existing `onChange` saveConfig path.
    private var presetBinding: Binding<SensitivityPreset?> {
        Binding(
            get: { self.currentPreset },
            set: { newValue in
                guard let preset = newValue else { return }
                let (stt, hear) = preset.thresholds
                config.sttEnergyThreshold = stt
                config.hearEnergyThreshold = hear
            }
        )
    }

    private var currentPreset: SensitivityPreset? {
        SensitivityPreset.from(
            stt: config.sttEnergyThreshold,
            hear: config.hearEnergyThreshold
        )
    }

    private var shortcutsSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Keyboard Shortcuts").font(.headline)
            KeyCaptureButton(
                label: "Pause / Resume",
                shortcut: $config.shortcutPause,
                onSave: { value in
                    _ = try? await cliService.setConfig(key: "shortcut_pause", value: value)
                }
            )
            KeyCaptureButton(
                label: "Toggle Auto-Enter",
                shortcut: $config.shortcutAutoEnter,
                onSave: { value in
                    _ = try? await cliService.setConfig(key: "shortcut_auto_enter", value: value)
                }
            )
            KeyCaptureButton(
                label: "Paste Last Filtered",
                shortcut: $config.shortcutForcePaste,
                onSave: { value in
                    _ = try? await cliService.setConfig(key: "shortcut_force_paste", value: value)
                }
            )
            Text("Shortcut changes take effect on next launch.")
                .font(.caption2)
                .foregroundColor(.secondary)
        }
    }

    private func loadConfig() async {
        isLoading = true
        do {
            config = try await cliService.getConfig()
            // Mask the API key with stars
            if let key = config.groqApiKey, !key.isEmpty {
                apiKey = String(repeating: "•", count: key.count)
            } else {
                apiKey = ""
            }
        } catch {
            print("Error loading config: \(error)")
        }
        isLoading = false
    }

    private func refreshStatus() async {
        do {
            status = try await cliService.getStatus()
        } catch {
            print("Error refreshing status: \(error)")
            status = DaemonStatus(isRunning: false, pid: nil, logPath: nil)
        }
    }

    private func saveApiKey() {
        isSavingApiKey = true
        Task {
            do {
                // Only save API key if it's not masked (doesn't contain only dots)
                if shouldPersistApiKey(apiKey) {
                    _ = try await cliService.setConfig(key: "groq_api_key", value: apiKey.isEmpty ? "" : apiKey)
                }
                // Add a small delay to make the loader visible for testing
                try await Task.sleep(nanoseconds: 1_000_000_000) // 1 second
            } catch {
                print("Error saving API key: \(error)")
            }
            isSavingApiKey = false
        }
    }

    private func saveConfig() {
        Task {
            do {
                _ = try await cliService.setConfig(key: "stt_energy_threshold", value: String(config.sttEnergyThreshold))
                _ = try await cliService.setConfig(key: "hear_energy_threshold", value: String(config.hearEnergyThreshold))
                _ = try await cliService.setConfig(key: "stt_silence", value: String(config.sttSilence))
                _ = try await cliService.setConfig(key: "stt_cooldown_ms", value: String(config.sttCooldownMs))
                _ = try await cliService.setConfig(key: "stt_auto_enter", value: String(config.sttAutoEnter))
                _ = try await cliService.setConfig(key: "silero_threshold", value: String(config.sileroThreshold))
                _ = try await cliService.setConfig(key: "postprocess_enabled", value: String(config.postprocessEnabled))
                // Only save API key if it's not masked (doesn't contain only dots)
                if shouldPersistApiKey(apiKey) {
                    _ = try await cliService.setConfig(key: "groq_api_key", value: apiKey.isEmpty ? "" : apiKey)
                }
            } catch {
                print("Error saving config: \(error)")
            }
        }
    }
    
    /// Canonical user glossary location. Mirrors `user_glossary_path()`
    /// in `src/glossary.rs` — both the daemon and the GUI must agree.
    private func userGlossaryURL() -> URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".always")
            .appendingPathComponent("glossary.json")
    }

    /// Ensure the parent directory + an empty file exist before
    /// LaunchServices is asked to open the path.
    private func ensureGlossaryExists(at url: URL) {
        let dir = url.deletingLastPathComponent()
        try? FileManager.default.createDirectory(
            at: dir, withIntermediateDirectories: true
        )
        if !FileManager.default.fileExists(atPath: url.path) {
            try? "[]".write(to: url, atomically: true, encoding: .utf8)
        }
    }

    private func openVocabularyFile() {
        let url = userGlossaryURL()
        ensureGlossaryExists(at: url)
        NSWorkspace.shared.open(url)
    }

    private func revealVocabularyInFinder() {
        let url = userGlossaryURL()
        ensureGlossaryExists(at: url)
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }

    private func refreshVocabularyInfo() {
        let url = userGlossaryURL()
        vocabularyPath = url.path

        if FileManager.default.fileExists(atPath: url.path),
           let data = try? Data(contentsOf: url),
           let json = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] {
            vocabularyTermCount = json.count
        } else {
            vocabularyTermCount = 0
        }
    }
}
