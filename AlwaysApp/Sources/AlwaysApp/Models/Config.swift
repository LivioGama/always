import Foundation

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

    // Defaults match `SensitivityPreset::Normal` and the Rust
    // `AlwaysConfig::default()` values.
    static let defaultConfig = Config(
        sttEnergyThreshold: 0.012,
        hearEnergyThreshold: 0.001,
        sttCooldownMs: 150,
        sttSilence: 1.5,
        sttAutoEnter: false,
        sttAutoEnterDelaySecs: 2,
        groqApiKey: nil,
        sileroThreshold: 0.5,
        shortcutPause: "ctrl+alt+p",
        shortcutAutoEnter: "ctrl+alt+a",
        shortcutForcePaste: "ctrl+alt+v",
        shortcutCorrectionDialog: "ctrl+alt+w",
        postprocessEnabled: true
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
                default:
                    break
                }
            }
        }
        return config
    }
}
