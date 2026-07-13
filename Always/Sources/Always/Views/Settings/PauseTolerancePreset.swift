/// User-facing presets for `stt_silence` (the end-of-utterance silence
/// window). Mirrors the `SensitivityPreset` pattern: the segmented picker
/// maps to canonical values, and any other value read back from the DB
/// (e.g. set via the Advanced field or the CLI) shows as "Custom".
enum PauseTolerancePreset: String, CaseIterable, Identifiable {
    case fast
    case balanced
    case relaxed

    var id: String { rawValue }

    var label: String {
        switch self {
        case .fast: return "Fast"
        case .balanced: return "Balanced"
        case .relaxed: return "Relaxed"
        }
    }

    /// Canonical `stt_silence` seconds. `balanced` matches the daemon's
    /// `DEFAULT_SILENCE_SECS`.
    var silenceSecs: Double {
        switch self {
        case .fast: return 0.6
        case .balanced: return 0.9
        case .relaxed: return 1.4
        }
    }

    static func from(silenceSecs: Double) -> PauseTolerancePreset? {
        for p in PauseTolerancePreset.allCases where abs(p.silenceSecs - silenceSecs) < 1e-6 {
            return p
        }
        return nil
    }
}
