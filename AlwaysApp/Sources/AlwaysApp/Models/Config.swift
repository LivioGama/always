import Foundation

struct Config: Codable {
    var sttEnergyThreshold: Double
    var hearEnergyThreshold: Double
    var sttCooldownMs: Int
    var sttSilence: Double
    var sttAutoEnter: Bool
    var groqApiKey: String?

    static let defaultConfig = Config(
        sttEnergyThreshold: 0.005,
        hearEnergyThreshold: 0.001,
        sttCooldownMs: 150,
        sttSilence: 0.4,
        sttAutoEnter: false,
        groqApiKey: nil
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
                case "groq_api_key":
                    if !value.contains("(not set)") {
                        config.groqApiKey = value
                    }
                default:
                    break
                }
            }
        }
        return config
    }
}