use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::localization::Localization;
use super::postprocess::PostProcessor;
use super::status_sound::StatusSoundSetting;
use crate::db;
use crate::db::Preferences;
use crate::stt_dispatch::TranscriberBackendChoice;

// Default configuration values
const DEFAULT_AUTO_ENTER_DELAY_MS: u32 = 4000;
/// WeSpeaker ResNet34 same-speaker utterances score ~0.6-0.9 cosine;
/// different speakers ~0.0-0.3. 0.50 keeps a wide margin both ways
/// while tolerating short/noisy utterances from the enrolled speaker.
pub const DEFAULT_SPEAKER_GATE_THRESHOLD: f64 = 0.50;
/// Raised 0.6 → 0.9: users pause mid-sentence to think, and 0.6s split one
/// thought into two pastes with per-segment grammar correction. Speculative
/// STT still hides the transcription cost inside the silence wait, so the
/// user-visible cost is ~0.3s of extra quiet before paste — refunded by
/// fewer split/re-dictation events. Pair with the adaptive mid-sentence
/// extension in `vad.rs` for pauses longer than the base window.
pub const DEFAULT_SILENCE_SECS: f64 = 0.9;
/// Ten minutes of no voice before idle auto-pause. Short values (e.g. 120s)
/// felt like the daemon "randomly" paused during normal desk work.
const DEFAULT_IDLE_PAUSE_SECS: u32 = 600;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Default, Serialize, Deserialize)]
pub enum IdlePauseAction {
    #[default]
    #[serde(rename = "pause")]
    Pause,
    #[serde(rename = "pause_and_mute")]
    PauseAndMute,
}

impl FromStr for IdlePauseAction {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pause" => Ok(Self::Pause),
            "pause_and_mute" => Ok(Self::PauseAndMute),
            _ => {
                anyhow::bail!("invalid idle pause action: {s}, must be 'pause' or 'pause_and_mute'")
            }
        }
    }
}

impl std::fmt::Display for IdlePauseAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pause => write!(f, "pause"),
            Self::PauseAndMute => write!(f, "pause_and_mute"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub enum VadMode {
    #[default]
    Local,
}

impl std::str::FromStr for VadMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "local" => Ok(Self::Local),
            _ => anyhow::bail!("invalid VAD mode: {s}, must be 'local'"),
        }
    }
}

impl std::fmt::Display for VadMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
        }
    }
}

/// Coarse, user-facing mic sensitivity level. Each variant maps to a
/// fixed pair of `stt_energy_threshold` + `hear_energy_threshold`. Both
/// the GUI Mic Sensitivity picker and the
/// `always config preset <low|normal|high>` CLI command write the same
/// underlying preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensitivityPreset {
    /// Quiet rooms / soft speakers — picks up faint speech but more
    /// likely to false-trigger on background noise.
    High,
    /// Recommended default — balanced for typical office voice.
    Normal,
    /// Noisy environments — only loud, clear speech triggers transcription.
    Low,
}

impl SensitivityPreset {
    /// `(stt_energy_threshold, hear_energy_threshold)` for this preset.
    pub fn thresholds(self) -> (f64, f64) {
        match self {
            SensitivityPreset::High => (0.005, 0.0005),
            SensitivityPreset::Normal => (0.012, 0.001),
            SensitivityPreset::Low => (0.025, 0.002),
        }
    }

    /// Which preset (if any) the supplied raw thresholds correspond to.
    /// Returns `None` for custom values.
    pub fn from_thresholds(stt: f64, hear: f64) -> Option<Self> {
        for preset in [Self::High, Self::Normal, Self::Low] {
            let (s, h) = preset.thresholds();
            if (s - stt).abs() < 1e-6 && (h - hear).abs() < 1e-6 {
                return Some(preset);
            }
        }
        None
    }
}

impl std::str::FromStr for SensitivityPreset {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "low" => Ok(Self::Low),
            "normal" | "medium" | "med" => Ok(Self::Normal),
            "high" => Ok(Self::High),
            _ => anyhow::bail!("invalid preset: {s}, must be one of low|normal|high"),
        }
    }
}

impl std::fmt::Display for SensitivityPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SensitivityPreset::Low => "low",
            SensitivityPreset::Normal => "normal",
            SensitivityPreset::High => "high",
        })
    }
}

#[cfg(test)]
mod sensitivity_preset_tests {
    use super::{AlwaysConfig, SensitivityPreset};
    use std::str::FromStr;

    #[test]
    fn parses_canonical_names() {
        assert_eq!(
            SensitivityPreset::from_str("low").unwrap(),
            SensitivityPreset::Low
        );
        assert_eq!(
            SensitivityPreset::from_str("Normal").unwrap(),
            SensitivityPreset::Normal
        );
        assert_eq!(
            SensitivityPreset::from_str("HIGH").unwrap(),
            SensitivityPreset::High
        );
    }

    #[test]
    fn accepts_medium_alias() {
        assert_eq!(
            SensitivityPreset::from_str("medium").unwrap(),
            SensitivityPreset::Normal
        );
    }

    #[test]
    fn rejects_unknown_levels() {
        assert!(SensitivityPreset::from_str("ultra").is_err());
    }

    #[test]
    fn round_trips_through_thresholds() {
        for preset in [
            SensitivityPreset::Low,
            SensitivityPreset::Normal,
            SensitivityPreset::High,
        ] {
            let (s, h) = preset.thresholds();
            assert_eq!(SensitivityPreset::from_thresholds(s, h), Some(preset));
        }
    }

    #[test]
    fn custom_values_resolve_to_none() {
        assert_eq!(SensitivityPreset::from_thresholds(0.999, 0.001), None);
    }

    #[test]
    fn normal_default_matches_alwaysconfig_default() {
        let (s, h) = SensitivityPreset::Normal.thresholds();
        assert!((s - 0.012).abs() < 1e-9);
        assert!((h - 0.001).abs() < 1e-9);
        let cfg = AlwaysConfig::default();
        assert!((cfg.energy_threshold - s).abs() < 1e-9);
        assert!((cfg.hear_energy_threshold - h).abs() < 1e-9);
        assert_eq!(cfg.onset_ms, 60);
    }
}

#[derive(Debug, Clone)]
pub struct AlwaysConfig {
    pub lang: String,
    pub timeout_secs: u32,
    pub silence_secs: f64,
    /// Extend the silence window when the speculative transcript looks
    /// mid-sentence (see `vad.rs::looks_mid_sentence`). Default on;
    /// DB pref `stt_adaptive_silence`.
    pub adaptive_silence_enabled: bool,
    /// "My Voice" speaker-verification gate. Active only when BOTH
    /// this flag is on AND a complete voiceprint is enrolled — so the
    /// default experience is unchanged until the user opts in via
    /// Settings → My Voice. DB pref `speaker_gate_enabled`.
    pub speaker_gate_enabled: bool,
    /// Minimum cosine similarity vs the enrolled voiceprint for an
    /// utterance to be transcribed. DB pref `speaker_gate_threshold`.
    pub speaker_gate_threshold: f64,
    pub auto_enter: bool,
    pub filter_enabled: bool,
    pub energy_threshold: f64,
    pub hear_energy_threshold: f64,
    pub onset_ms: u32,
    pub cooldown_ms: u32,
    pub log_path: PathBuf,
    pub post_processor: Option<Arc<PostProcessor>>,
    pub project_root: Option<PathBuf>,
    pub learning_enabled: bool,
    /// Groq Whisper API key. `None` when no key is configured — valid
    /// state once local models exist (user can run fully offline).
    /// The Groq backend refuses to start without one; local backends
    /// don't read this field.
    pub groq_stt_api_key: Option<String>,
    /// Active STT backend. Defaults to [`TranscriberBackendChoice::Groq`]
    /// so existing installs keep their current behavior on upgrade.
    pub transcriber_backend: TranscriberBackendChoice,
    pub vad_mode: VadMode,
    pub silero_threshold: f32,
    pub vocab_config: VocabConfig,
    pub postprocess_config: PostprocessConfig,
    /// Delay (ms) between paste and the synthesized Return when
    /// `auto_enter` is on. `0` = press Return immediately (legacy
    /// behavior). When > 0, a countdown overlay is shown; any key
    /// press cancels.
    pub auto_enter_delay_ms: u32,
    /// Auto-pause the daemon after this many seconds with no voice
    /// activity. `0` = disabled. Default 120 (matches requirement).
    pub idle_pause_secs: u32,
    /// What action to take when idle timeout occurs: pause only, or pause+mute.
    pub idle_pause_action: IdlePauseAction,
    /// Locale-specific heuristics for post-processing (sentence-terminator
    /// detection + "safe to lowercase mid-sentence" word list). Defaults
    /// to [`Localization::ENGLISH`]; overridable so non-English users
    /// get correct merge-time casing without code changes.
    pub localization: Localization,
    /// Append accepted utterances to `~/.always/transcripts.jsonl` for
    /// external consumers (e.g. IRIS tailing the file). Opt-in: persisted
    /// transcripts are privacy-relevant, so this defaults to off.
    pub transcript_stream_enabled: bool,
    /// Status sounds for the four sleep-coding states. Off by default.
    pub audible_status_sound: StatusSoundSetting,
}

#[derive(Debug, Clone)]
pub struct VocabConfig {
    pub file_patterns: Vec<String>,
    pub common_words: Vec<String>,
    pub min_term_length: usize,
    pub max_term_length: usize,
}

impl Default for VocabConfig {
    fn default() -> Self {
        Self {
            file_patterns: default_vocab_patterns(),
            common_words: default_common_words(),
            min_term_length: 2,
            max_term_length: 50,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PostprocessConfig {
    pub groq_model: String,
    pub learning_history_limit: usize,
    pub grammar_correction_enabled: bool,
    pub cache_ttl_seconds: u64,
}

impl Default for PostprocessConfig {
    fn default() -> Self {
        Self {
            // Tested empirically against the user's failure set:
            //   8b-instant: invents (`Mo`→`Monday`, `struts`→`structures`,
            //               `5050`→`5.050`), ignores glossary
            //   70b-versatile: catches glossary but still expands
            //               (`repo`→`repository`, `pack mine`→`pack of mine`,
            //               `cloud.md`→`cloud code`)
            //   gpt-oss-120b: clean across the board — applies glossary,
            //               preserves abbreviations, doesn't invent.
            // Override at runtime via `ALWAYS_GROQ_MODEL=<id>`.
            groq_model: "openai/gpt-oss-120b".to_string(),
            learning_history_limit: 1000,
            // Default ON. The LLM postprocess pass is the single
            // glossary-aware cleanup layer in the pipeline. Quality
            // hinges entirely on the system prompt in
            // `glossary::build_postprocess_prompt` — that's the
            // surface to iterate on when transcripts are wrong.
            grammar_correction_enabled: true,
            cache_ttl_seconds: 300,
        }
    }
}

impl AlwaysConfig {
    pub fn from_cli(
        lang: String,
        timeout_secs: u32,
        silence_secs: Option<f64>,
        auto_enter: Option<bool>,
    ) -> Result<Self> {
        // API key is now optional: the daemon can run with a local
        // model only. A missing key surfaces an error at the point the
        // Groq backend is actually invoked, not at startup.
        let groq_stt_api_key = match get_groq_stt_api_key() {
            Ok(k) => Some(k),
            Err(e) => {
                tracing::info!(reason = %e, "no_groq_api_key");
                None
            }
        };
        let vad_mode = std::env::var("ALWAYS_VAD_MODE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_default();
        let prefs = load_preferences()?;
        let transcriber_backend = resolve_transcriber_backend(&prefs);
        // CLI default "auto" means unset — honor the user's saved DB pref.
        // Explicit CLI override (e.g. `always run --lang fr`) always wins.
        let lang = if lang == "auto" || lang.is_empty() {
            prefs.lang.clone().unwrap_or_else(|| "auto".to_string())
        } else {
            lang
        };
        // CLI flag wins when explicitly set; otherwise read the user's
        // saved pref; final fallback to the canonical default (true).
        // This is the single auto-enter source-of-truth resolution —
        // the previous code always took the CLI value, so a user-saved
        // `stt_auto_enter = false` was silently ignored when the daemon
        // was relaunched without an explicit override.
        let auto_enter = auto_enter.or(prefs.stt_auto_enter).unwrap_or(true);

        let vocab_config = load_vocab_config();
        let postprocess_config = load_postprocess_config();

        let project_root = detect_project_root();

        // Honor user pref for LLM postprocess: DB > env > default true.
        let postprocess_enabled = prefs
            .postprocess_enabled
            .unwrap_or(postprocess_config.grammar_correction_enabled);
        let mut effective_postprocess = postprocess_config.clone();
        effective_postprocess.grammar_correction_enabled = postprocess_enabled;

        // Construct the post-processor whenever grammar_correction is
        // enabled. The previous gate required BOTH a loaded
        // vocabulary.json AND a detected project root (`.git`
        // ancestor of cwd) — but the daemon is launched by the Mac
        // app from /Applications, where cwd has no `.git` ancestor,
        // so the post_processor was permanently None and the LLM
        // cleanup never fired regardless of the user's pref. We now
        // build it with sensible empty defaults so the prompt-driven
        // glossary cleanup always runs when enabled.
        let post_processor = {
            let groq_api_key = groq_stt_api_key
                .clone()
                .or_else(|| std::env::var("GROQ_API_KEY").ok());
            tracing::info!(
                grammar_correction_enabled = effective_postprocess.grammar_correction_enabled,
                has_api_key = groq_api_key.is_some(),
                project_root = project_root.is_some(),
                "post_processor_init"
            );
            Some(Arc::new(PostProcessor::new_with_config(
                effective_postprocess.clone(),
                groq_api_key,
            )))
        };

        let config = Self {
            lang,
            timeout_secs,
            // Explicit CLI/app launch value wins; otherwise use saved prefs.
            // Clamp to the same bounds as the
            // Settings UI. 0.7s minimum keeps paste responsive without
            // allowing absurdly tiny windows that split normal phrases.
            silence_secs: resolve_silence_secs(silence_secs, &prefs),
            adaptive_silence_enabled: prefs.stt_adaptive_silence.unwrap_or(true),
            speaker_gate_enabled: prefs.speaker_gate_enabled.unwrap_or(false),
            speaker_gate_threshold: prefs
                .speaker_gate_threshold
                .unwrap_or(DEFAULT_SPEAKER_GATE_THRESHOLD)
                .clamp(0.30, 0.80),
            // `auto_enter` was already resolved above: CLI flag → DB pref →
            // canonical default. The previous code re-read `prefs.stt_auto_enter`
            // here, which silently undid an explicit CLI override.
            auto_enter,
            filter_enabled: true, // Always enabled - filter is always on
            // Defense-in-depth: clamp values read back from the DB to the
            // same bounds `set_preference` enforces on write. A corrupted /
            // manually-edited / schema-skewed row must not be trusted just
            // because the write path validated — an out-of-range threshold
            // silently breaks the VAD (never or always triggers).
            energy_threshold: prefs.stt_energy_threshold.unwrap_or(0.012).clamp(0.0, 1.0),
            hear_energy_threshold: prefs.hear_energy_threshold.unwrap_or(0.001).clamp(0.0, 1.0),
            onset_ms: 60,
            cooldown_ms: prefs.stt_cooldown_ms.unwrap_or(800).min(5000),
            log_path: log_path_from_preferences(&prefs),
            post_processor,
            project_root,
            learning_enabled: postprocess_config.learning_history_limit > 0,
            groq_stt_api_key,
            transcriber_backend,
            vad_mode,
            silero_threshold: {
                // Reject non-finite first: f32::clamp returns NaN for a NaN
                // input, which would make every VAD comparison silently false.
                let v = prefs.silero_threshold.unwrap_or(0.5);
                if v.is_finite() {
                    v.clamp(0.1, 0.9) as f32
                } else {
                    0.5
                }
            },
            vocab_config,
            postprocess_config: effective_postprocess,
            auto_enter_delay_ms: prefs
                .auto_enter_delay_ms
                .unwrap_or(DEFAULT_AUTO_ENTER_DELAY_MS)
                .min(60_000),
            idle_pause_secs: prefs
                .idle_pause_secs
                .unwrap_or(DEFAULT_IDLE_PAUSE_SECS)
                .min(86_400),
            idle_pause_action: prefs
                .idle_pause_action
                .as_ref()
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
            // English is the only built-in locale today; non-English
            // users can override at the call site (or via a future
            // CLI/preference) without touching the merge logic.
            localization: Localization::ENGLISH,
            transcript_stream_enabled: resolve_transcript_stream(&prefs),
            audible_status_sound: prefs
                .audible_status_sound
                .as_deref()
                .and_then(|value| value.parse().ok())
                .unwrap_or_default(),
        };

        Ok(config)
    }

    /// True when the LLM grammar/glossary postprocess pass should run for
    /// the current utterance.
    ///
    /// Requires grammar correction enabled + a usable API key (see
    /// [`PostProcessor::can_correct`]) AND the Groq STT backend active.
    /// Local models must stay fully offline and near-instant, so
    /// postprocess never fires while `transcriber_backend` is `Local` —
    /// regardless of the saved `postprocess_enabled` preference, which
    /// keeps applying normally the moment the user switches back to Groq.
    pub fn postprocess_available(&self) -> bool {
        matches!(self.transcriber_backend, TranscriberBackendChoice::Groq)
            && self
                .post_processor
                .as_ref()
                .is_some_and(|pp| pp.can_correct())
    }

    /// Whether deterministic CONTENT filtering (hard phrase filter +
    /// hallucination heuristics) may reject a transcript.
    ///
    /// Only the remote Groq path — where an LLM does the real,
    /// context-aware cleanup — is allowed to make content judgments, and
    /// even there the heuristics are just a coarse pre-pass. On LOCAL
    /// models there is no LLM in the loop and the deterministic rules are
    /// brittle: a natural stutter run ("uh uh uh", "o o o o") was counted
    /// as a hallucination and DISCARDED a whole 5-minute dictation. Local
    /// dictation is therefore RAW verbatim — the system transcribes and
    /// inserts exactly what was said, and never judges the content. The
    /// user is the authority on what they meant to say. Identity (the
    /// speaker/voiceprint gate) is separate and still applies.
    pub fn content_filtering_enabled(&self) -> bool {
        matches!(self.transcriber_backend, TranscriberBackendChoice::Groq)
    }
}

impl Default for AlwaysConfig {
    fn default() -> Self {
        let vocab_config = VocabConfig::default();
        let postprocess_config = PostprocessConfig::default();

        Self {
            lang: "en".to_string(),
            timeout_secs: 30,
            // Defaults aligned with `SensitivityPreset::Normal` and the
            // Mic Sensitivity / Speaking Style picker in the GUI.
            // Speculative STT starts before the final cutoff, and users
            // who still get mid-sentence splits can raise this in Settings
            // (Pause tolerance picker).
            silence_secs: DEFAULT_SILENCE_SECS,
            adaptive_silence_enabled: true,
            speaker_gate_enabled: false,
            speaker_gate_threshold: DEFAULT_SPEAKER_GATE_THRESHOLD,
            auto_enter: true,
            filter_enabled: true,
            energy_threshold: 0.012,
            hear_energy_threshold: 0.001,
            onset_ms: 60,
            cooldown_ms: 800,
            log_path: default_log_path(),
            post_processor: None,
            project_root: detect_project_root(),
            learning_enabled: postprocess_config.learning_history_limit > 0,
            groq_stt_api_key: None,
            transcriber_backend: TranscriberBackendChoice::default(),
            vad_mode: VadMode::default(),
            silero_threshold: 0.5,
            vocab_config,
            postprocess_config,
            auto_enter_delay_ms: DEFAULT_AUTO_ENTER_DELAY_MS,
            idle_pause_secs: DEFAULT_IDLE_PAUSE_SECS,
            idle_pause_action: IdlePauseAction::default(),
            localization: Localization::ENGLISH,
            transcript_stream_enabled: false,
            audible_status_sound: StatusSoundSetting::default(),
        }
    }
}

/// Resolve the transcript-stream opt-in. Order: DB pref →
/// `ALWAYS_TRANSCRIPT_STREAM` env var → default off.
fn resolve_transcript_stream(prefs: &Preferences) -> bool {
    if let Some(saved) = prefs.transcript_stream {
        return saved;
    }
    std::env::var("ALWAYS_TRANSCRIPT_STREAM")
        .ok()
        .and_then(|s| match s.to_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod transcript_stream_resolution_tests {
    use super::*;

    #[test]
    fn saved_preference_wins() {
        let prefs = Preferences {
            transcript_stream: Some(true),
            ..Default::default()
        };
        assert!(resolve_transcript_stream(&prefs));

        let prefs = Preferences {
            transcript_stream: Some(false),
            ..Default::default()
        };
        assert!(!resolve_transcript_stream(&prefs));
    }

    #[test]
    fn defaults_off_when_unset() {
        assert!(!resolve_transcript_stream(&Preferences::default()));
    }
}

fn load_vocab_config() -> VocabConfig {
    VocabConfig {
        file_patterns: std::env::var("ALWAYS_FILE_PATTERNS")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(default_vocab_patterns),
        common_words: std::env::var("ALWAYS_COMMON_WORDS")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(default_common_words),
        min_term_length: std::env::var("ALWAYS_MIN_TERM_LENGTH")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2),
        max_term_length: std::env::var("ALWAYS_MAX_TERM_LENGTH")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(50),
    }
}

fn load_postprocess_config() -> PostprocessConfig {
    // Build from the canonical Default, then override each field only if
    // the corresponding env var is set. Keeping `PostprocessConfig::default`
    // as the single source of truth prevents drift like the earlier
    // `gpt-oss-120b` (Default) vs `llama-3.1-8b-instant` (loader) split,
    // where the 8b model silently took over in production and degraded
    // transcript quality (documented as inventing words / ignoring glossary).
    let mut cfg = PostprocessConfig::default();
    if let Ok(model) = std::env::var("ALWAYS_GROQ_MODEL") {
        cfg.groq_model = model;
    }
    if let Ok(limit) = std::env::var("ALWAYS_LEARNING_LIMIT")
        && let Ok(parsed) = limit.parse()
    {
        cfg.learning_history_limit = parsed;
    }
    if let Ok(enabled) = std::env::var("ALWAYS_GRAMMAR_CORRECTION")
        && let Ok(parsed) = enabled.parse()
    {
        cfg.grammar_correction_enabled = parsed;
    }
    if let Ok(ttl) = std::env::var("ALWAYS_CACHE_TTL")
        && let Ok(parsed) = ttl.parse()
    {
        cfg.cache_ttl_seconds = parsed;
    }
    cfg
}

fn default_vocab_patterns() -> Vec<String> {
    vec![
        r"\b[A-Z][a-zA-Z0-9]*\b".to_string(),       // CamelCase
        r"\b[a-z]+_[a-z_]+\b".to_string(),          // snake_case
        r"\b[A-Z_]+\b".to_string(),                 // SCREAMING_SNAKE_CASE
        r"\b[a-z]+[A-Z][a-zA-Z0-9]*\b".to_string(), // camelCase
    ]
}

fn default_common_words() -> Vec<String> {
    vec![
        "the", "and", "for", "are", "but", "not", "you", "all", "can", "had", "her", "was", "one",
        "our", "out", "with", "have", "this", "that", "from", "they", "will", "would", "there",
        "their", "what", "about", "which", "when", "make", "like", "into", "year", "your", "just",
        "over", "also", "such", "because", "these", "first", "being", "through", "after", "where",
        "should", "some", "those",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn detect_project_root() -> Option<PathBuf> {
    let current = std::env::current_dir().ok()?;

    // Check if current directory has .git
    if current.join(".git").exists() {
        return Some(current);
    }

    // Check parents
    let mut path = current.clone();
    while path.pop() {
        if path.join(".git").exists() {
            return Some(path);
        }
    }

    None
}

pub fn configured_log_path() -> Result<PathBuf> {
    load_preferences().map(|prefs| log_path_from_preferences(&prefs))
}

fn load_preferences() -> Result<Preferences> {
    db::open().and_then(|conn| db::get_preferences(&conn))
}

fn log_path_from_preferences(prefs: &Preferences) -> PathBuf {
    prefs
        .always_log_path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(default_log_path)
}

fn default_log_path() -> PathBuf {
    // tracing-appender rotates daily on UTC and suffixes with the UTC date,
    // so the actual file is `always.YYYY-MM-DD` in UTC, not local time.
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    crate::always::telemetry::get_log_directory().join(format!("always.{date}"))
}

/// Resolve the active STT backend from the prefs DB, with env override.
/// Order: `ALWAYS_TRANSCRIBER` env var → DB pref → default Groq.
/// An invalid stored value falls back to Groq with a warning rather
/// than refusing to start the daemon.
fn resolve_transcriber_backend(prefs: &Preferences) -> TranscriberBackendChoice {
    if let Ok(env_val) = std::env::var("ALWAYS_TRANSCRIBER")
        && let Ok(parsed) = env_val.parse()
    {
        return parsed;
    }
    if let Some(stored) = prefs.transcriber_backend.as_deref() {
        match stored.parse() {
            Ok(parsed) => return parsed,
            Err(e) => tracing::warn!(stored, error = %e, "ignoring_invalid_transcriber_backend"),
        }
    }
    TranscriberBackendChoice::default()
}

/// Single source of truth for the silence-window range. The VAD trusts
/// the configured value as-is (no second clamp): a config floor of 0.7
/// combined with the VAD's old internal 0.5s cap meant the window was
/// pinned to exactly 0.5s and the user's setting was silently dead —
/// the direct cause of "it cuts me off mid-sentence and I can't fix it".
pub const SILENCE_SECS_MIN: f64 = 0.3;
pub const SILENCE_SECS_MAX: f64 = 15.0;

fn resolve_silence_secs(cli_silence_secs: Option<f64>, prefs: &Preferences) -> f64 {
    cli_silence_secs
        .or(prefs.stt_silence)
        .unwrap_or(DEFAULT_SILENCE_SECS)
        .clamp(SILENCE_SECS_MIN, SILENCE_SECS_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_cli_silence_overrides_saved_preference() {
        let prefs = Preferences {
            stt_silence: Some(2.0),
            ..Default::default()
        };

        assert_eq!(resolve_silence_secs(Some(1.5), &prefs), 1.5);
    }

    #[test]
    fn omitted_cli_silence_uses_saved_preference() {
        let prefs = Preferences {
            stt_silence: Some(2.0),
            ..Default::default()
        };

        assert_eq!(resolve_silence_secs(None, &prefs), 2.0);
    }

    #[test]
    fn silence_default_is_honored_and_floor_is_three_hundred_ms() {
        let prefs = Preferences::default();

        // The default must survive resolution unchanged — the old 0.7
        // floor silently rewrote it, and the VAD's old 0.5 cap then
        // rewrote it again. One range, applied once, here.
        assert_eq!(resolve_silence_secs(None, &prefs), DEFAULT_SILENCE_SECS);
        assert_eq!(resolve_silence_secs(Some(0.2), &prefs), SILENCE_SECS_MIN);
        assert_eq!(resolve_silence_secs(Some(20.0), &prefs), SILENCE_SECS_MAX);
        // User-configured values in range pass through untouched.
        assert_eq!(resolve_silence_secs(Some(0.8), &prefs), 0.8);
    }

    /// Local backend must never trigger the LLM postprocess pass, even
    /// with grammar correction enabled and a working API key — local
    /// models are the "no internet, still instant" path, and a stray
    /// Groq round-trip would defeat that.
    #[test]
    fn postprocess_unavailable_on_local_backend_even_with_working_key() {
        let mut cfg = AlwaysConfig {
            post_processor: Some(Arc::new(PostProcessor::new(Some(
                "test-key-never-used".to_string(),
            )))),
            transcriber_backend: TranscriberBackendChoice::Groq,
            ..Default::default()
        };
        assert!(cfg.postprocess_available());

        cfg.transcriber_backend = TranscriberBackendChoice::Local {
            model_id: "parakeet-tdt-0.6b-v2".to_string(),
        };
        assert!(!cfg.postprocess_available());
    }

    /// Switching back to Groq restores postprocess with no extra state
    /// to reconcile — `postprocess_enabled` itself is untouched by the
    /// backend switch.
    #[test]
    fn postprocess_available_resumes_when_switching_back_to_groq() {
        let mut cfg = AlwaysConfig {
            post_processor: Some(Arc::new(PostProcessor::new(Some(
                "test-key-never-used".to_string(),
            )))),
            transcriber_backend: TranscriberBackendChoice::Local {
                model_id: "parakeet-tdt-0.6b-v2".to_string(),
            },
            ..Default::default()
        };
        assert!(!cfg.postprocess_available());

        cfg.transcriber_backend = TranscriberBackendChoice::Groq;
        assert!(cfg.postprocess_available());
    }

    /// No API key (or grammar correction off) still blocks postprocess
    /// on the Groq backend — the backend gate is additive, not a
    /// replacement for the existing `can_correct` checks.
    #[test]
    fn postprocess_unavailable_on_groq_without_api_key() {
        let cfg = AlwaysConfig {
            post_processor: Some(Arc::new(PostProcessor::new(None))),
            transcriber_backend: TranscriberBackendChoice::Groq,
            ..Default::default()
        };
        assert!(!cfg.postprocess_available());
    }
}

fn get_groq_stt_api_key() -> Result<String> {
    let env_key = std::env::var("GROQ_API_KEY").ok();
    let db_key = db::open()
        .ok()
        .and_then(|conn| db::get_preferences(&conn).ok())
        .and_then(|prefs| prefs.groq_api_key);

    let Some((source, key)) = select_groq_key(db_key, env_key) else {
        anyhow::bail!(
            "GROQ_API_KEY environment variable not set and no saved Groq key found. Set it in Settings or with: always config set groq_api_key <your-key>"
        )
    };

    match source {
        GroqKeySource::Database => tracing::info!("Groq API key loaded from saved settings"),
        GroqKeySource::Environment => {
            tracing::info!("Groq API key loaded from environment variable")
        }
    }

    Ok(key)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroqKeySource {
    Database,
    Environment,
}

fn select_groq_key(
    db_key: Option<String>,
    env_key: Option<String>,
) -> Option<(GroqKeySource, String)> {
    if let Some(key) = db_key.filter(|key| !key.is_empty()) {
        return Some((GroqKeySource::Database, key));
    }
    if let Some(key) = env_key.filter(|key| !key.is_empty()) {
        return Some((GroqKeySource::Environment, key));
    }
    None
}

#[cfg(test)]
mod groq_key_tests {
    use super::{GroqKeySource, select_groq_key};

    #[test]
    fn saved_groq_key_wins_over_stale_environment() {
        let selected = select_groq_key(Some("saved-key".to_string()), Some("env-key".to_string()));

        assert_eq!(
            selected,
            Some((GroqKeySource::Database, "saved-key".to_string()))
        );
    }

    #[test]
    fn environment_groq_key_is_fallback_for_cli_only_setup() {
        let selected = select_groq_key(None, Some("env-key".to_string()));

        assert_eq!(
            selected,
            Some((GroqKeySource::Environment, "env-key".to_string()))
        );
    }

    #[test]
    fn empty_groq_key_values_are_ignored() {
        let selected = select_groq_key(Some(String::new()), Some("env-key".to_string()));

        assert_eq!(
            selected,
            Some((GroqKeySource::Environment, "env-key".to_string()))
        );
    }
}
