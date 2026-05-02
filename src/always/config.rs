use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::context_vocab::ContextVocabulary;
use super::postprocess::PostProcessor;
use super::text::Vocabulary;
use crate::db::Preferences;
use crate::db;

#[derive(Debug, Clone)]
pub enum VadMode {
    Local,
}

impl Default for VadMode {
    fn default() -> Self {
        Self::Local
    }
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

#[derive(Debug, Clone)]
pub struct AlwaysConfig {
    pub lang: String,
    pub timeout_secs: u32,
    pub silence_secs: f64,
    pub auto_enter: bool,
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
            groq_model: "llama-3.1-8b-instant".to_string(),
            learning_history_limit: 1000,
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

        let context_vocab = if let Some(ref root) = project_root {
            Some(Arc::new(Mutex::new(ContextVocabulary::new_with_config(
                Some(root.clone()),
                vocab_config.clone(),
            ))))
        } else {
            None
        };

        let post_processor = if let (Some(vocab), Some(context_vocab)) = (&vocab, &context_vocab) {
            let groq_api_key = std::env::var("GROQ_API_KEY").ok();
            Some(Arc::new(PostProcessor::new_with_config(
                vocab.clone(),
                Arc::clone(context_vocab),
                postprocess_config.clone(),
                groq_api_key,
            )))
        } else {
            None
        };

        let config = Self {
            lang,
            timeout_secs,
            silence_secs: prefs.stt_silence.unwrap_or(silence_secs),
            auto_enter,
            filter_enabled: true, // Always enabled - filter is always on
            energy_threshold: prefs.stt_energy_threshold.unwrap_or(0.15),
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
            postprocess_config,
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
            silence_secs: 0.5,
            auto_enter: false,
            filter_enabled: true,
            energy_threshold: 0.15,
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
    // Use platform-standard log directory from telemetry module
    crate::always::telemetry::get_log_directory().join("always.log")
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
    if let Ok(conn) = db::open() {
        if let Ok(prefs) = db::get_preferences(&conn) {
            if let Some(key) = prefs.groq_api_key {
                tracing::warn!("Groq API key loaded from database (should migrate to keychain)");
                return Ok(key);
            }
        }
    }

    anyhow::bail!(
        "GROQ_API_KEY environment variable not set and no key found in keychain. Set it with: always config set groq_api_key <your-key>"
    )
}
