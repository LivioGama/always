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

/// Bool that decodes to `true` when its key is absent — backward
/// compatibility for Config JSON written before the field existed.
@propertyWrapper
struct DefaultTrue: Codable, Equatable {
    var wrappedValue: Bool
    init(wrappedValue: Bool = true) { self.wrappedValue = wrappedValue }
    init(from decoder: Decoder) throws {
        wrappedValue = try decoder.singleValueContainer().decode(Bool.self)
    }
    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(wrappedValue)
    }
}

extension KeyedDecodingContainer {
    func decode(_ type: DefaultTrue.Type, forKey key: Key) throws -> DefaultTrue {
        try decodeIfPresent(DefaultTrue.self, forKey: key) ?? DefaultTrue()
    }
}

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
    /// Live provisional transcript in the overlay while the user is
    /// still talking (daemon periodically re-transcribes the growing
    /// utterance on non-streaming backends). Default on; decodes to
    /// `true` for JSON written before the field existed.
    @DefaultTrue var sttLivePreview: Bool
    var sttAutoEnter: Bool
    /// Auto-enter delay in milliseconds. Single source of truth — UI
    /// displays as seconds via `Double(autoEnterDelayMs) / 1000` but the
    /// wire and DB columns are always ms.
    var autoEnterDelayMs: Int
    var groqApiKey: String?
    /// True when the daemon reports the key is saved (`*** (saved)`),
    /// even though the actual value is masked and not sent to the GUI.
    var groqKeySaved: Bool
    var sileroThreshold: Float
    var shortcutPause: String
    var shortcutAutoEnter: String
    var shortcutForcePaste: String
    var shortcutCorrectionDialog: String
    var shortcutMasterPause: String
    var postprocessEnabled: Bool
    /// Post-processing LLM provider: "groq" (remote, needs API key) or
    /// "apple" (on-device Apple Intelligence, keyless). Default "groq".
    var postprocessProvider: String
    /// Whether Apple's on-device language model is ready on this machine.
    /// The daemon reports it via `config show`; when false, selecting the
    /// Apple provider can't actually run. Defaults true so older daemons
    /// don't spuriously warn.
    var appleIntelligenceAvailable: Bool
    var perAppSettingsJson: String?
    /// Seconds of no voice before daemon auto-pauses. 0 = disabled.
    var idlePauseSecs: Int
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
        sttLivePreview: DefaultTrue(wrappedValue: true),
        sttAutoEnter: true,
        autoEnterDelayMs: 4000,
        groqApiKey: nil,
        groqKeySaved: false,
        sileroThreshold: 0.5,
        shortcutPause: "ctrl+alt+p",
        shortcutAutoEnter: "ctrl+alt+a",
        shortcutForcePaste: "ctrl+alt+v",
        shortcutCorrectionDialog: "ctrl+alt+w",
        shortcutMasterPause: "ctrl+alt+shift+p",
        postprocessEnabled: true,
        postprocessProvider: "groq",
        appleIntelligenceAvailable: true,
        perAppSettingsJson: nil,
        idlePauseSecs: 600,
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
                case "stt_live_preview":
                    config.sttLivePreview = (value == "true" || value == "1")
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
                    if !value.contains("(not set)") {
                        if isMaskedApiKeyPlaceholder(value) {
                            config.groqKeySaved = true
                        } else {
                            config.groqApiKey = value
                            config.groqKeySaved = true
                        }
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
                case "shortcut_master_pause":
                    if !value.contains("(not set)") {
                        config.shortcutMasterPause = value
                    }
                case "postprocess_enabled":
                    config.postprocessEnabled = (value == "true" || value == "1")
                case "postprocess_provider":
                    // Accept only known providers; ignore anything else to
                    // avoid persisting a value the daemon can't parse.
                    if ["groq", "apple"].contains(value) {
                        config.postprocessProvider = value
                    }
                case "apple_intelligence_available":
                    config.appleIntelligenceAvailable = (value == "true" || value == "1")
                case "per_app_settings_json":
                    config.perAppSettingsJson = value == "{}" ? nil : value
                case "idle_pause_secs":
                    config.idlePauseSecs = Int(value) ?? defaultConfig.idlePauseSecs
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
