import Foundation

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
    var sttAutoEnter: Bool
    var sttAutoEnterDelaySecs: Int
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

    // Defaults match `SensitivityPreset::Normal` and the Rust
    // `AlwaysConfig::default()` values.
    static let defaultConfig = Config(
        sttEnergyThreshold: 0.012,
        hearEnergyThreshold: 0.001,
        sttCooldownMs: 150,
        sttSilence: 2.0,
        sttAutoEnter: true,
        sttAutoEnterDelaySecs: 4,
        groqApiKey: nil,
        sileroThreshold: 0.5,
        shortcutPause: "ctrl+alt+p",
        shortcutAutoEnter: "ctrl+alt+a",
        shortcutForcePaste: "ctrl+alt+v",
        shortcutCorrectionDialog: "ctrl+alt+w",
        postprocessEnabled: true,
        perAppSettingsJson: nil,
        idlePauseSecs: 120,
        idlePauseAction: "pause"
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
                case "stt_silence":
                    config.sttSilence = Double(value.replacingOccurrences(of: "s", with: "")) ?? defaultConfig.sttSilence
                case "stt_auto_enter":
                    config.sttAutoEnter = value == "true"
                case "auto_enter_delay_ms":
                    if let ms = Int(value) {
                        config.sttAutoEnterDelaySecs = ms / 1000
                    } else {
                        config.sttAutoEnterDelaySecs = defaultConfig.sttAutoEnterDelaySecs
                    }
                case "stt_auto_enter_delay_secs":
                    config.sttAutoEnterDelaySecs = Int(value) ?? defaultConfig.sttAutoEnterDelaySecs
                case "groq_api_key":
                    if !value.contains("(not set)") {
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
                default:
                    break
                }
            }
        }
        return config
    }
}
