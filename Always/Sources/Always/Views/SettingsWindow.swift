import SwiftUI
import AppKit
import os.log

// Helpers (KeyCaptureButton, NumericSettingRow, SensitivityPreset,
// formatShortcut) live in `Views/Settings/` so this file holds only the
// composition root and section bodies. Add new sections by extracting
// another file in `Views/Settings/`, not by growing this struct.

private let settingsLogger = Logger(subsystem: "com.always.app", category: "settings")

/// Fixed settings panel size (matches pre–Models-section layout + Models block).
enum SettingsWindowMetrics {
    static let width: CGFloat = 660
    static let height: CGFloat = 920

    static func apply(to window: NSWindow) {
        window.setContentSize(NSSize(width: width, height: height))
        window.minSize = NSSize(width: 620, height: 560)
    }
}

// MARK: - Settings window

struct SettingsWindow: View {
    @ObservedObject var cliService: CLIService
    @ObservedObject private var stateMonitor: StateMonitor = .shared
    @ObservedObject private var focusedApp: FocusedAppMonitor = .shared
    @State private var config: Config = Config.defaultConfig
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

    /// Cooldown uses 3 decimal places because typical values are ~0.150s.
    private static let cooldownSecondsFormatter: NumberFormatter = {
        let f = NumberFormatter()
        f.numberStyle = .decimal
        f.minimumFractionDigits = 3
        f.maximumFractionDigits = 3
        f.minimum = 0
        f.maximum = 60
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
            // Banner only renders when one or more permissions are
            // missing — invisible afterwards.
            PermissionsBanner()
            // Connection status stays on top — it's the at-a-glance
            // health indicator. Allowlist sits directly below as the
            // single section that mutates most often, before the
            // static settings (behavior, API, vocabulary, …).
            statusAndPauseRow
            Divider()
            perAppSection
            Divider()
            behaviorRow
            Divider()
            apiSection
            Divider()
            ModelsSection()
            Divider()
            vocabularySection
            Divider()
            sensitivitySection
            Divider()
            shortcutsSection
        }
        .padding(20)
        .frame(width: 620, alignment: .topLeading)
        .background(SettingsWindowFrameFix())
        .onAppear {
            focusedField = nil
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
                focusedField = nil
            }
            refreshVocabularyInfo()
            Task {
                await loadConfig()
            }
        }
        .onChange(of: config.sttEnergyThreshold) { _, _ in saveConfig() }
        .onChange(of: config.hearEnergyThreshold) { _, _ in saveConfig() }
        .onChange(of: config.sttSilence) { _, _ in saveConfig() }
        .onChange(of: config.sttCooldownMs) { _, _ in saveConfig() }
        .onChange(of: config.autoEnterDelayMs) { _, _ in saveConfig() }
        .onChange(of: config.sileroThreshold) { _, _ in saveConfig() }
        .onChange(of: config.postprocessEnabled) { _, _ in saveConfig() }
        .onChange(of: config.idlePauseSecs) { _, _ in saveConfig() }
        .onChange(of: config.idlePauseAction) { _, _ in saveConfig() }
    }

    // MARK: Sections

    private var statusAndPauseRow: some View {
        // Bind to `stateMonitor` — the live UDS connection signal — instead
        // of a one-shot `cliService.getStatus()` snapshot. The old version
        // only ran on `.onAppear`, so the row stayed at "Reconnecting…" for
        // the rest of the session if the daemon wasn't ready in that first
        // second. `isDaemonConnected` flips via `UDSClient.$isConnected` and
        // the watchdog `isDaemonDegraded` flag.
        let isRunning = stateMonitor.isDaemonConnected
        let isDegraded = stateMonitor.isDaemonDegraded
        let displayLabel: String
        let displayColor: Color
        let symbol: String
        if isRunning && !isDegraded {
            displayLabel = "Running"
            displayColor = .green
            symbol = "checkmark.circle.fill"
        } else if isDegraded {
            displayLabel = "Reconnecting…"
            displayColor = .orange
            symbol = "arrow.triangle.2.circlepath"
        } else {
            displayLabel = "Disconnected"
            displayColor = .orange
            symbol = "exclamationmark.triangle.fill"
        }
        // The master pause/resume toggle now lives at the top of the
        // Voice Typing Allowlist section — the allowlist model makes
        // it more an "emergency stop / lift the freeze" affordance
        // than a primary control, so this row is just a connection
        // status indicator now.
        return HStack(spacing: 12) {
            Image(systemName: symbol)
                .foregroundColor(displayColor)
            Text(displayLabel)
                .font(.headline)
                .foregroundColor(displayColor)
            Spacer()
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
            NumericSettingRow(
                title: "Auto-Enter Delay",
                help: "Seconds before auto-enter. Set to 0 to disable.",
                unit: "s",
                formatter: Self.intFormatter,
                value: Binding(
                    get: { config.autoEnterDelayMs / 1000 },
                    set: { config.autoEnterDelayMs = $0 * 1000 }
                ),
                defaultValue: 4,
                range: 0...60
            )
            .padding(.top, 4)
            HStack {
                Toggle(
                    "Smart Postprocess (LLM grammar + glossary fixes)",
                    isOn: $config.postprocessEnabled
                )
                Spacer()
            }

            Divider().padding(.vertical, 2)

            // Idle Auto-Pause Section
            Text("Idle Auto-Pause")
                .font(.subheadline.bold())
                .foregroundColor(.secondary)

            Text(
                "Pauses listening after a stretch with no speech. Does not flip the global master switch — switch to a resumed app or speak to wake."
            )
            .font(.caption)
            .foregroundColor(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            NumericSettingRow(
                title: "Pause After Inactivity",
                help: "Seconds of no voice before idle auto-pause. Default 600 (10 min). Set to 0 to disable.",
                unit: "s",
                formatter: Self.intFormatter,
                value: $config.idlePauseSecs,
                defaultValue: 600,
                range: 0...86_400
            )

            HStack {
                Text("On Idle Timeout:")
                    .help("What happens when idle timeout occurs. 'Pause' just pauses listening. 'Pause + Mute' also mutes input audio.")
                Picker("", selection: $config.idlePauseAction) {
                    Text("Pause Only").tag("pause")
                    Text("Pause + Mute").tag("pause_and_mute")
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                Spacer()
            }
            .padding(.top, 2)
        }
    }

    /// Pause is now off-by-default per app. The Settings section
    /// surfaces:
    ///   1. The currently-focused app + a one-click toggle to add or
    ///      remove it from the resumed-apps allowlist.
    ///   2. The full allowlist (every app the user has explicitly
    ///      resumed) with a "Remove" button per row.
    /// Everything else is implicit ("any app not on the list is paused").
    private var perAppSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                Text("Voice Typing Allowlist").font(.headline)
                Spacer()
                Text("\(stateMonitor.resumedBundleIds.count) app\(stateMonitor.resumedBundleIds.count == 1 ? "" : "s") resumed")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            Text("Always is paused by default. Add an app here to resume voice typing while you're in it.")
                .font(.caption)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            masterPauseControl
            focusedAppCard

            if !stateMonitor.resumedBundleIds.isEmpty {
                Divider().padding(.vertical, 2)
                Text("Resumed apps")
                    .font(.subheadline.bold())
                    .foregroundColor(.secondary)
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(
                        stateMonitor.resumedBundleIds.sorted(),
                        id: \.self
                    ) { bundleId in
                        resumedAppRow(bundleId: bundleId)
                    }
                }
            }
        }
    }

    /// Master pause kill switch — pauses every resumed app at once
    /// (and unpauses back to per-app rules when toggled off). The
    /// allowlist makes this an exceptional control rather than a
    /// primary one, so it sits at the top of the section instead of
    /// next to the connection-status row.
    @ViewBuilder
    private var masterPauseControl: some View {
        let masterPaused = stateMonitor.isMasterPaused
        HStack(spacing: 10) {
            Image(systemName: masterPaused ? "exclamationmark.octagon.fill" : "checkmark.shield.fill")
                .font(.title3)
                .foregroundColor(masterPaused ? .orange : .accentColor)
            VStack(alignment: .leading, spacing: 2) {
                Text(masterPaused ? "Paused everywhere" : "Allowlist active")
                    .font(.subheadline.bold())
                Text(
                    masterPaused
                        ? "Master kill switch is on — every resumed app is force-paused."
                        : "Resumed apps below are listening; everything else is paused."
                )
                .font(.caption)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            }
            Spacer()
            Button {
                stateMonitor.togglePause()
            } label: {
                Text(masterPaused ? "Lift pause" : "Pause everything")
            }
            .controlSize(.small)
            .disabled(!stateMonitor.isDaemonConnected)
        }
        .padding(10)
        .background(
            (masterPaused ? Color.orange : Color.accentColor).opacity(0.08)
        )
        .cornerRadius(6)
    }

    /// Header card that surfaces the currently-focused app and a
    /// one-click toggle to add/remove it from the allowlist. This is
    /// the answer to "what app am I paused for right now?".
    @ViewBuilder
    private var focusedAppCard: some View {
        let bundleId = focusedApp.currentBundleId
        let name = focusedApp.currentAppName ?? bundleId ?? "—"
        let isResumed = bundleId.map { stateMonitor.resumedBundleIds.contains($0) } ?? false
        let isPaused = stateMonitor.isPaused
        HStack(spacing: 10) {
            Image(systemName: isResumed ? "checkmark.circle.fill" : "pause.circle.fill")
                .font(.title3)
                .foregroundColor(isResumed && !isPaused ? .green : .orange)
            VStack(alignment: .leading, spacing: 2) {
                Text(bundleId == nil ? "No focused app" : "Currently focused")
                    .font(.caption)
                    .foregroundColor(.secondary)
                Text(name)
                    .font(.subheadline.bold())
                Text(currentAppStateText(isResumed: isResumed, isPaused: isPaused))
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            Spacer()
            if let bundle = bundleId {
                Button(action: {
                    let newPaused: Bool? = isResumed ? nil : false
                    stateMonitor.setAppPaused(bundleId: bundle, paused: newPaused)
                }) {
                    Text(isResumed ? "Remove from allowlist" : "Resume for this app")
                }
                .controlSize(.small)
            }
        }
        .padding(10)
        .background(Color.secondary.opacity(0.08))
        .cornerRadius(6)
    }

    private func currentAppStateText(isResumed: Bool, isPaused: Bool) -> String {
        if stateMonitor.isMasterPaused {
            return "Globally paused — use “Lift pause” above to resume all apps."
        }
        if stateMonitor.isIdleAutoPaused {
            return "Idle timeout — speak or switch to a resumed app to wake listening."
        }
        if isResumed && isPaused {
            return "On the allowlist but paused for another reason."
        }
        if isResumed { return "Voice typing is active here." }
        return "Paused (not on the allowlist)."
    }

    @ViewBuilder
    private func resumedAppRow(bundleId: String) -> some View {
        let isFocused = focusedApp.currentBundleId == bundleId
        HStack(spacing: 8) {
            Image(systemName: isFocused ? "arrowtriangle.right.fill" : "checkmark.circle")
                .font(.caption)
                .foregroundColor(isFocused ? .accentColor : .green)
                .frame(width: 14)
            Text(appNameForBundle(bundleId))
                .font(.callout)
            Spacer()
            Button(role: .destructive) {
                stateMonitor.setAppPaused(bundleId: bundleId, paused: nil)
            } label: {
                Image(systemName: "trash")
            }
            .buttonStyle(.plain)
            .help("Remove from allowlist (this app will be paused again)")
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(
            isFocused
                ? Color.accentColor.opacity(0.10)
                : Color.clear
        )
        .cornerRadius(4)
    }

    private func appNameForBundle(_ bundleId: String) -> String {
        let workspace = NSWorkspace.shared
        if let bundle = NSRunningApplication.runningApplications(withBundleIdentifier: bundleId).first {
            return bundle.localizedName ?? bundleId
        }
        if let app = workspace.urlForApplication(withBundleIdentifier: bundleId),
           let bundle = Bundle(url: app),
           let displayName = bundle.infoDictionary?["CFBundleDisplayName"] as? String {
            return displayName
        }
        if let app = workspace.urlForApplication(withBundleIdentifier: bundleId) {
            return FileManager.default.displayName(atPath: app.path)
        }
        return bundleId
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
                        help: "Min seconds between consecutive pastes",
                        unit: "s",
                        formatter: Self.cooldownSecondsFormatter,
                        value: Binding(
                            get: { Double(config.sttCooldownMs) / 1000.0 },
                            set: { config.sttCooldownMs = Int(($0 * 1000).rounded()) }
                        ),
                        defaultValue: 0.150,
                        range: 0...60.0
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
            KeyCaptureButton(
                label: "Correction Dialog",
                shortcut: $config.shortcutCorrectionDialog,
                onSave: { value in
                    _ = try? await cliService.setConfig(key: "shortcut_correction_dialog", value: value)
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
            settingsLogger.error("loadConfig failed: \(error.localizedDescription, privacy: .public)")
        }
        isLoading = false
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
                settingsLogger.error("saveApiKey failed: \(error.localizedDescription, privacy: .public)")
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
                _ = try await cliService.setConfig(key: "auto_enter_delay_ms", value: String(config.autoEnterDelayMs))
                _ = try await cliService.setConfig(key: "silero_threshold", value: String(config.sileroThreshold))
                _ = try await cliService.setConfig(key: "postprocess_enabled", value: String(config.postprocessEnabled))
                _ = try await cliService.setConfig(key: "idle_pause_secs", value: String(config.idlePauseSecs))
                _ = try await cliService.setConfig(key: "idle_pause_action", value: config.idlePauseAction)
                // Only save API key if it's not masked (doesn't contain only dots)
                if shouldPersistApiKey(apiKey) {
                    _ = try await cliService.setConfig(key: "groq_api_key", value: apiKey.isEmpty ? "" : apiKey)
                }
            } catch {
                settingsLogger.error("saveConfig failed: \(error.localizedDescription, privacy: .public)")
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

/// Re-applies the intended content size once the `NSWindow` exists (SwiftUI
/// `defaultSize` alone is unreliable on macOS 26 menu-bar apps).
private struct SettingsWindowFrameFix: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView {
        let view = NSView(frame: .zero)
        DispatchQueue.main.async {
            if let window = view.window {
                SettingsWindowMetrics.apply(to: window)
            }
        }
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        DispatchQueue.main.async {
            if let window = nsView.window {
                SettingsWindowMetrics.apply(to: window)
            }
        }
    }
}
