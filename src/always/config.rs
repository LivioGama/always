use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use parking_lot::Mutex;

use super::context_vocab::ContextVocabulary;
use super::postprocess::PostProcessor;
use super::text::Vocabulary;
use crate::db;
use crate::db::Preferences;

// Default configuration values
const DEFAULT_AUTO_ENTER_DELAY_MS: u32 = 4000;
const DEFAULT_IDLE_PAUSE_SECS: u32 = 120;

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
    use super::SensitivityPreset;
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
        let (s, _) = SensitivityPreset::Normal.thresholds();
        assert!((s - 0.012).abs() < 1e-9);
    }
}

#[derive(Debug, Clone)]
pub struct AlwaysConfig {
    pub lang: String,
    pub timeout_secs: u32,
    pub silence_secs: f64,
    pub auto_enter: bool,
    pub auto_enter_delay_secs: u64,
    pub filter_enabled: bool,
    pub energy_threshold: f64,
    pub onset_ms: u32,
    pub cooldown_ms: u32,
    pub log_path: PathBuf,
    pub vocab: Option<Vocabulary>,
    pub context_vocab: Option<Arc<Mutex<ContextVocabulary>>>,
    pub post_processor: Option<Arc<PostProcessor>>,
    pub project_root: Option<PathBuf>,
    pub learning_enabled: bool,
    pub groq_stt_api_key: String,
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
        silence_secs: f64,
        auto_enter: bool,
    ) -> Result<Self> {
        let groq_stt_api_key = get_groq_stt_api_key()?;
        let vad_mode = std::env::var("ALWAYS_VAD_MODE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_default();
        let prefs = load_preferences()?;
        let vocab = Vocabulary::load();

        let vocab_config = load_vocab_config();
        let postprocess_config = load_postprocess_config();

        let project_root = detect_project_root();

        let context_vocab = project_root.as_ref().map(|root| {
            Arc::new(Mutex::new(ContextVocabulary::new_with_config(
                Some(root.clone()),
                vocab_config.clone(),
            )))
        });

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
            let groq_api_key = std::env::var("GROQ_API_KEY").ok().or_else(|| {
                if groq_stt_api_key.is_empty() {
                    None
                } else {
                    Some(groq_stt_api_key.clone())
                }
            });
            let vocab_for_pp = vocab.clone().unwrap_or_else(Vocabulary::default_patterns);
            let context_vocab_for_pp = context_vocab
                .clone()
                .unwrap_or_else(|| Arc::new(Mutex::new(ContextVocabulary::new(None))));
            tracing::info!(
                grammar_correction_enabled = effective_postprocess.grammar_correction_enabled,
                has_api_key = groq_api_key.is_some(),
                project_root = project_root.is_some(),
                "post_processor_init"
            );
            Some(Arc::new(PostProcessor::new_with_config(
                vocab_for_pp,
                context_vocab_for_pp,
                effective_postprocess.clone(),
                groq_api_key,
            )))
        };

        let config = Self {
            lang,
            timeout_secs,
            silence_secs: prefs.stt_silence.unwrap_or(silence_secs),
            auto_enter,
            auto_enter_delay_secs: prefs
                .auto_enter_delay_ms
                .map(|ms| (ms as u64 + 999) / 1000) // Convert ms to seconds, round up
                .unwrap_or(2),
            filter_enabled: true, // Always enabled - filter is always on
            energy_threshold: prefs.stt_energy_threshold.unwrap_or(0.012),
            onset_ms: 30,
            cooldown_ms: prefs.stt_cooldown_ms.unwrap_or(150),
            log_path: log_path_from_preferences(&prefs),
            vocab,
            context_vocab,
            post_processor,
            project_root,
            learning_enabled: postprocess_config.learning_history_limit > 0,
            groq_stt_api_key,
            vad_mode,
            silero_threshold: prefs.silero_threshold.unwrap_or(0.5) as f32,
            vocab_config,
            postprocess_config: effective_postprocess,
            auto_enter_delay_ms: prefs.auto_enter_delay_ms.unwrap_or(DEFAULT_AUTO_ENTER_DELAY_MS),
            idle_pause_secs: prefs.idle_pause_secs.unwrap_or(DEFAULT_IDLE_PAUSE_SECS),
        };

        Ok(config)
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
            // 1.5s — snappier than 2.0 / 2.5 while still tolerating
            // typical mid-sentence pauses. Speculative transcription
            // kicks off at 75% of this (≈1.1s) so end-to-end paste
            // latency rarely exceeds ~1.5s after the user stops talking.
            silence_secs: 1.5,
            auto_enter: true,
            auto_enter_delay_secs: 4,
            filter_enabled: true,
            energy_threshold: 0.012,
            onset_ms: 30,
            cooldown_ms: 150,
            log_path: default_log_path(),
            vocab: Vocabulary::load(),
            context_vocab: None,
            post_processor: None,
            project_root: detect_project_root(),
            learning_enabled: postprocess_config.learning_history_limit > 0,
            groq_stt_api_key: String::new(),
            vad_mode: VadMode::default(),
            silero_threshold: 0.5,
            vocab_config,
            postprocess_config,
            auto_enter_delay_ms: DEFAULT_AUTO_ENTER_DELAY_MS,
            idle_pause_secs: DEFAULT_IDLE_PAUSE_SECS,
        }
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
    PostprocessConfig {
        groq_model: std::env::var("ALWAYS_GROQ_MODEL")
            .unwrap_or_else(|_| "llama-3.1-8b-instant".to_string()),
        learning_history_limit: std::env::var("ALWAYS_LEARNING_LIMIT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1000),
        grammar_correction_enabled: std::env::var("ALWAYS_GRAMMAR_CORRECTION")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(true),
        cache_ttl_seconds: std::env::var("ALWAYS_CACHE_TTL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(300),
    }
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

fn get_groq_stt_api_key() -> Result<String> {
    use crate::always::keyring;

    // Try environment variable first (highest priority)
    if let Ok(key) = std::env::var("GROQ_API_KEY") {
        tracing::info!("Groq API key loaded from environment variable");
        return Ok(key);
    }

    // Try keyring
    if let Ok(Some(key)) = keyring::get_groq_api_key() {
        tracing::info!("Groq API key loaded from keychain");
        return Ok(key);
    }

    // Fallback to database preferences for migration purposes
    if let Ok(conn) = db::open()
        && let Ok(prefs) = db::get_preferences(&conn)
        && let Some(key) = prefs.groq_api_key
    {
        tracing::warn!("Groq API key loaded from database (should migrate to keychain)");
        return Ok(key);
    }

    anyhow::bail!(
        "GROQ_API_KEY environment variable not set and no key found in keychain. Set it with: always config set groq_api_key <your-key>"
    )
}
