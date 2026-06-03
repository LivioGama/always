import Foundation

/// Mirror of `SensitivityPreset` in `src/always/config.rs`. Both sides
/// MUST hold the same `(stt, hear)` threshold pairs — the GUI preset
/// picker and the CLI `always config preset <level>` command write the
/// same underlying preferences. The Swift test
/// `testNormalPresetMatchesDefaultConfig` plus the Rust
/// `normal_default_matches_alwaysconfig_default` test are the lockstep
/// guard.
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
