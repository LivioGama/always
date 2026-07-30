import SwiftUI
import AppKit
import os.log

// Helpers (KeyCaptureButton, NumericSettingRow, SensitivityPreset,
// formatShortcut) live in `Views/Settings/` so this file holds only the
// composition root and section bodies. Add new sections by extracting
// another file in `Views/Settings/`, not by growing this struct.

private let settingsLogger = Logger(subsystem: "com.always.app", category: "settings")

/// Fixed settings panel size (sidebar layout with separated panels).
enum SettingsWindowMetrics {
    static let width: CGFloat = 900
    static let height: CGFloat = 920

    static func apply(to window: NSWindow) {
        window.setContentSize(NSSize(width: width, height: height))
        window.minSize = NSSize(width: 860, height: 560)
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
    @State private var selectedPanel: SettingsPanel = .general

    enum Field {
        case apiKey
    }

    // ----- Number formatters used by the sensitivity rows. -----
    static let energyFormatter: NumberFormatter = {
        let f = NumberFormatter()
        f.numberStyle = .decimal
        f.minimumFractionDigits = 4
        f.maximumFractionDigits = 4
        f.minimum = 0
        f.maximum = 1
        return f
    }()

    static let secondsFormatter: NumberFormatter = {
        let f = NumberFormatter()
        f.numberStyle = .decimal
        f.minimumFractionDigits = 1
        f.maximumFractionDigits = 2
        f.minimum = 0
        f.maximum = 10
        return f
    }()

    /// Cooldown uses 3 decimal places because typical values are ~0.150s.
    static let cooldownSecondsFormatter: NumberFormatter = {
        let f = NumberFormatter()
        f.numberStyle = .decimal
        f.minimumFractionDigits = 3
        f.maximumFractionDigits = 3
        f.minimum = 0
        f.maximum = 60
        return f
    }()

    static let intFormatter: NumberFormatter = {
        let f = NumberFormatter()
        f.numberStyle = .none
        f.minimum = 0
        f.maximum = 60_000
        return f
    }()

    static let probabilityFormatter: NumberFormatter = {
        let f = NumberFormatter()
        f.numberStyle = .decimal
        f.minimumFractionDigits = 2
        f.maximumFractionDigits = 2
        f.minimum = 0
        f.maximum = 1
        return f
    }()

    var body: some View {
        HStack(spacing: 0) {
            SettingsSidebar(selectedPanel: $selectedPanel, stateMonitor: stateMonitor)
            Divider()
            panelContent
        }
        .background(SettingsWindowFrameFix())
        .onAppear {
            focusedField = nil
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
                focusedField = nil
            }
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
        .onChange(of: config.audibleStatusSound) { _, _ in saveConfig() }
    }

    @ViewBuilder
    private var panelContent: some View {
        switch selectedPanel {
        case .general:
            GeneralPanel(
                stateMonitor: stateMonitor,
                focusedApp: focusedApp,
                config: $config
            )
        case .behavior:
            BehaviorPanel(
                cliService: cliService,
                stateMonitor: stateMonitor,
                config: $config
            )
        case .shortcuts:
            ShortcutsPanel(cliService: cliService, config: $config)
        case .permissions:
            PermissionsPanel()
        case .vocabulary:
            VocabularyPanel()
        case .snippets:
            SnippetsPanel()
        case .myVoice:
            MyVoicePanel(stateMonitor: stateMonitor)
        case .history:
            HistoryPanel()
        case .models:
            ModelsPanel(
                config: $config,
                apiKey: $apiKey,
                showApiKey: $showApiKey,
                isSavingApiKey: $isSavingApiKey,
                focusedField: $focusedField,
                saveApiKey: saveApiKey
            )
        case .about:
            AboutPanel()
        }
    }

    // MARK: Helper Methods

    private func loadConfig() async {
        isLoading = true
        do {
            config = try await cliService.getConfig()
            apiKey = config.groqApiKey ?? ""
        } catch {
            settingsLogger.error("loadConfig failed: \(error.localizedDescription, privacy: .public)")
        }
        isLoading = false
    }

    private func saveApiKey() {
        isSavingApiKey = true
        Task {
            do {
                if apiKey.isEmpty || shouldPersistApiKey(apiKey) {
                    _ = try await cliService.setConfig(key: "groq_api_key", value: apiKey)
                    _ = try await cliService.restartDaemon()
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
                _ = try await cliService.setConfig(key: "stt_adaptive_silence", value: String(config.sttAdaptiveSilence))
                _ = try await cliService.setConfig(key: "stt_cooldown_ms", value: String(config.sttCooldownMs))
                _ = try await cliService.setConfig(key: "auto_enter_delay_ms", value: String(config.autoEnterDelayMs))
                _ = try await cliService.setConfig(key: "silero_threshold", value: String(config.sileroThreshold))
                _ = try await cliService.setConfig(key: "postprocess_enabled", value: String(config.postprocessEnabled))
                _ = try await cliService.setConfig(key: "idle_pause_secs", value: String(config.idlePauseSecs))
                _ = try await cliService.setConfig(key: "idle_pause_action", value: config.idlePauseAction)
                _ = try await cliService.setConfig(key: "audible_status_sound", value: config.audibleStatusSound)
                await MainActor.run {
                    stateMonitor.applyRuntimePreferences(from: config)
                }
            } catch {
                settingsLogger.error("saveConfig failed: \(error.localizedDescription, privacy: .public)")
            }
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
