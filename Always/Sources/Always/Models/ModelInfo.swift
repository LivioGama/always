import Foundation

/// Mirror of the Rust `EngineType` enum
/// (`src/managers/model_registry.rs`). The daemon serializes via
/// `#[derive(Serialize)]`, which emits unit variants as bare strings —
/// matching `Codable` defaults.
enum EngineType: String, Codable, Equatable {
    case whisper = "Whisper"
    case parakeet = "Parakeet"
    case moonshine = "Moonshine"
    case moonshineStreaming = "MoonshineStreaming"
    case senseVoice = "SenseVoice"
    case gigaAM = "GigaAM"
    case canary = "Canary"
    case cohere = "Cohere"

    var displayName: String {
        switch self {
        case .whisper: return "Whisper"
        case .parakeet: return "Parakeet"
        case .moonshine: return "Moonshine"
        case .moonshineStreaming: return "Moonshine Streaming"
        case .senseVoice: return "SenseVoice"
        case .gigaAM: return "GigaAM"
        case .canary: return "Canary"
        case .cohere: return "Cohere"
        }
    }
}

/// One catalog entry. Field names match `ModelInfo` in
/// `src/managers/model_registry.rs` so the JSON serialised by
/// `serde_json` decodes here without per-field coding keys.
struct ModelInfo: Codable, Identifiable, Equatable, Hashable {
    let id: String
    let name: String
    let description: String
    let filename: String
    let url: String?
    let sha256: String?
    let size_mb: UInt64
    let is_downloaded: Bool
    let is_downloading: Bool
    let partial_size: UInt64
    let is_directory: Bool
    let engine_type: EngineType
    let accuracy_score: Float
    let speed_score: Float
    let supports_translation: Bool
    let supports_streaming: Bool
    let is_recommended: Bool
    let supported_languages: [String]
    let supports_language_selection: Bool
    let is_custom: Bool

    /// "146 MB" / "1.5 GB" — same units the Handy UI uses.
    var sizeLabel: String {
        if size_mb >= 1024 {
            let gb = Double(size_mb) / 1024.0
            return String(format: "%.1f GB", gb)
        }
        return "\(size_mb) MB"
    }

    /// True when both score fields are 0 — the daemon's sentinel for
    /// user-supplied custom models we don't have benchmarks for.
    var hidesScores: Bool {
        accuracy_score == 0 && speed_score == 0
    }

    var supportsMultipleLanguages: Bool {
        supported_languages.count > 1
    }

    var languageLabel: String {
        if supports_language_selection && supported_languages.count > 1 {
            return "Multi-language"
        }
        if supported_languages.count == 1, let only = supported_languages.first {
            return only.uppercased() == "EN" ? "English Only" : "\(only.uppercased()) Only"
        }
        return "Multi-language"
    }

    var translateLabel: String? {
        supports_translation ? "Translate to English" : nil
    }

    var streamingLabel: String? {
        supports_streaming ? "Live streaming" : nil
    }
}

/// Catalog snapshot pushed by the daemon in response to
/// `ListModels` and after every model mutation. Wraps a single
/// `models` field — the same `Vec<ModelInfo>` the Rust side emits.
struct ModelsListData: Codable {
    let models: [ModelInfo]
}

/// Streaming download progress payload.
struct ModelDownloadProgressData: Codable {
    let model_id: String
    let downloaded: UInt64
    let total: UInt64
    let percentage: Double
}

/// Common single-id payloads — collapses ten near-identical structs
/// into one. The daemon emits the same `{"model_id":"..."}` shape for
/// `ModelDownloadComplete`, `ModelVerificationStarted`, etc.
struct ModelIdData: Codable {
    let model_id: String
}

/// Failure payloads carry a free-form `error` string in addition to
/// the model id. Kept separate from `ModelIdData` so the UI doesn't
/// have to second-guess which events have an error field.
struct ModelErrorData: Codable {
    let model_id: String
    let error: String
}

struct ActiveTranscriberChangedData: Codable {
    let backend: String
}
