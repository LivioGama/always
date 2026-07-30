import Foundation
import os.log

private let configLogger = Logger(subsystem: "com.always.app", category: "config-parse")

/// Keys the CLI's `config show` output emits today but the Swift Config
/// struct intentionally doesn't bind. Filter these out of the
/// "unknown_key" warning so we only surface genuinely-unknown drift.
private let knownButUnboundCliKeys: Set<String> = [
    "deepgram_api_key",
    "deepgram_model",
    "always_log_path",
    "shortcut_log_correction",
    "passive_correction_capture",
]

struct AppOverride: Codable {
    var autoEnter: Bool?
    var paused: Bool?
    var autoEnterDelayMs: Int?

    enum CodingKeys: String, CodingKey {
        case autoEnter = "auto_enter"
        case paused
        case autoEnterDelayMs = "auto_enter_delay_ms"
    }
}

struct Config: Codable {
    var sttEnergyThreshold: Double
    var hearEnergyThreshold: Double
    var sttCooldownMs: Int
    var sttSilence: Double
    /// Extend the silence window when the transcript-so-far looks
    /// mid-sentence (daemon-side heuristic). Default on.
    var sttAdaptiveSilence: Bool
    var sttAutoEnter: Bool
    /// Auto-enter delay in milliseconds. Single source of truth — UI
    /// displays as seconds via `Double(autoEnterDelayMs) / 1000` but the
    /// wire and DB columns are always ms.
    var autoEnterDelayMs: Int
    var groqApiKey: String?
    var sileroThreshold: Float
    var shortcutPause: String
    var shortcutAutoEnter: String
    var shortcutForcePaste: String
    var shortcutCorrectionDialog: String
    var postprocessEnabled: Bool
    var perAppSettingsJson: String?
    /// Seconds of no voice before daemon auto-pauses. 0 = disabled.
    var idlePauseSecs: Int
    /// Action on idle timeout: "pause" or "pause_and_mute".
    var idlePauseAction: String
    /// Status sound setting: off, low, medium, or high.
    var audibleStatusSound: String
    /// Language code for transcription ("auto", "en", "fr", etc.) or nil if not set.
    var lang: String?

    // Defaults match `SensitivityPreset::Normal` and the Rust
    // `AlwaysConfig::default()` values.
    static let defaultConfig = Config(
        sttEnergyThreshold: 0.012,
        hearEnergyThreshold: 0.001,
        sttCooldownMs: 150,
        sttSilence: 0.9,
        sttAdaptiveSilence: true,
        sttAutoEnter: true,
        autoEnterDelayMs: 4000,
        groqApiKey: nil,
        sileroThreshold: 0.5,
        shortcutPause: "ctrl+alt+p",
        shortcutAutoEnter: "ctrl+alt+a",
        shortcutForcePaste: "ctrl+alt+v",
        shortcutCorrectionDialog: "ctrl+alt+w",
        postprocessEnabled: true,
        perAppSettingsJson: nil,
        idlePauseSecs: 600,
        idlePauseAction: "pause",
        audibleStatusSound: "off",
        lang: nil
    )

    static func fromCLI(output: String) -> Config? {
        var config = defaultConfig
        let lines = output.split(separator: "\n")

        for line in lines {
            let parts = line.split(separator: ":", maxSplits: 1)
            if parts.count == 2 {
                let key = parts[0].trimmingCharacters(in: .whitespaces)
                let value = parts[1].trimmingCharacters(in: .whitespaces)

                switch key {
                case "stt_energy_threshold":
                    config.sttEnergyThreshold = Double(value) ?? defaultConfig.sttEnergyThreshold
                case "hear_energy_threshold":
                    config.hearEnergyThreshold = Double(value) ?? defaultConfig.hearEnergyThreshold
                case "stt_cooldown_ms":
                    config.sttCooldownMs = Int(value) ?? defaultConfig.sttCooldownMs
                case "stt_cooldown_secs":
                    // Daemon `config show` prints seconds; convert back to ms.
                    if let secs = Double(value) {
                        config.sttCooldownMs = Int((secs * 1000).rounded())
                    }
                case "stt_silence", "stt_silence_secs":
                    config.sttSilence = Double(value.replacingOccurrences(of: "s", with: "")) ?? defaultConfig.sttSilence
                case "stt_adaptive_silence":
                    config.sttAdaptiveSilence = (value == "true" || value == "1")
                case "stt_auto_enter":
                    config.sttAutoEnter = (value == "true" || value == "1")
                case "auto_enter_delay_ms":
                    config.autoEnterDelayMs = Int(value) ?? defaultConfig.autoEnterDelayMs
                case "auto_enter_delay_secs", "stt_auto_enter_delay_secs":
                    // Legacy daemon emitted fractional seconds (e.g. "4.000")
                    // under these keys. The canonical key is `auto_enter_delay_ms`
                    // and the field is ms-typed; convert on read so older
                    // daemon builds still feed the GUI correctly.
                    if let secs = Double(value) {
                        config.autoEnterDelayMs = Int((secs * 1000).rounded())
                    }
                case "groq_api_key":
                    if !value.contains("(not set)") && !isMaskedApiKeyPlaceholder(value) {
                        config.groqApiKey = value
                    }
                case "silero_threshold":
                    config.sileroThreshold = Float(value) ?? defaultConfig.sileroThreshold
                case "shortcut_pause":
                    if !value.contains("(not set)") {
                        config.shortcutPause = value
                    }
                case "shortcut_auto_enter":
                    if !value.contains("(not set)") {
                        config.shortcutAutoEnter = value
                    }
                case "shortcut_force_paste":
                    if !value.contains("(not set)") {
                        config.shortcutForcePaste = value
                    }
                case "shortcut_correction_dialog":
                    if !value.contains("(not set)") {
                        config.shortcutCorrectionDialog = value
                    }
                case "postprocess_enabled":
                    config.postprocessEnabled = (value == "true" || value == "1")
                case "per_app_settings_json":
                    config.perAppSettingsJson = value == "{}" ? nil : value
                case "idle_pause_secs":
                    config.idlePauseSecs = Int(value) ?? defaultConfig.idlePauseSecs
                case "idle_pause_action":
                    if value == "pause" || value == "pause_and_mute" {
                        config.idlePauseAction = value
                    }
                case "audible_status_sound":
                    if ["off", "low", "medium", "high"].contains(value) {
                        config.audibleStatusSound = value
                    }
                case "lang":
                    config.lang = value.isEmpty || value.contains("(not set)") ? nil : value
                default:
                    // Surface drift: if the CLI emits a new key the GUI
                    // doesn't bind, log it once per parse so a daemon
                    // update doesn't silently lose a setting. Skip
                    // intentionally-unbound keys (deepgram, log path,
                    // passive correction etc.) to keep the signal:noise
                    // ratio useful.
                    if !knownButUnboundCliKeys.contains(key) {
                        configLogger.warning("unknown_cli_key: \(key, privacy: .public) = \(value, privacy: .public)")
                    }
                }
            }
        }
        return config
    }
}
