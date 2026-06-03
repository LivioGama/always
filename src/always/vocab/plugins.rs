use anyhow::Result;
use std::path::PathBuf;

/// Configuration for a vocabulary plugin
pub struct PluginConfig {
    /// Custom installation path (optional)
    pub install_path: Option<PathBuf>,
    /// Configuration file paths
    pub config_paths: Vec<PathBuf>,
    /// Data file paths
    pub data_paths: Vec<PathBuf>,
    /// API endpoints (if applicable)
    pub api_endpoints: Vec<String>,
    /// Custom metadata
    pub metadata: std::collections::HashMap<String, String>,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            install_path: None,
            config_paths: Vec::new(),
            data_paths: Vec::new(),
            api_endpoints: Vec::new(),
            metadata: std::collections::HashMap::new(),
        }
    }
}

/// Trait for vocabulary import plugins
pub trait VocabPlugin {
    /// Get the plugin name
    fn name(&self) -> &str;

    /// Get the plugin description
    fn description(&self) -> &str {
        "No description provided"
    }

    /// Get the plugin version
    fn version(&self) -> &str {
        "1.0.0"
    }

    /// Get custom configuration
    fn get_config(&self) -> PluginConfig {
        PluginConfig::default()
    }

    /// Check if the software is installed
    /// Uses default paths unless custom config is provided
    fn is_installed(&self) -> bool;

    /// Check if the software is installed at a specific path
    fn is_installed_at(&self, path: &PathBuf) -> bool {
        path.exists()
    }

    /// Get installation paths to check (platform-specific)
    fn get_installation_paths(&self) -> Vec<PathBuf>;

    /// Extract vocabulary from the software
    fn extract_vocabulary(&self) -> Vec<String>;

    /// Extract vocabulary from a specific path
    fn extract_vocabulary_from_path(&self, path: &PathBuf) -> Result<Vec<String>> {
        let _ = path;
        Ok(Vec::new())
    }

    /// Extract vocabulary from configuration files
    fn extract_vocabulary_from_config(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Extract vocabulary from data files
    fn extract_vocabulary_from_data(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Extract vocabulary from API (if applicable)
    fn extract_vocabulary_from_api(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Main extraction method that combines all sources
    fn extract_all_vocabulary(&self) -> Result<Vec<String>> {
        let mut all_terms = Vec::new();

        // From default extraction
        all_terms.extend(self.extract_vocabulary());

        // From config
        if let Ok(config_terms) = self.extract_vocabulary_from_config() {
            all_terms.extend(config_terms);
        }

        // From data files (already limited to top 100)
        if let Ok(data_terms) = self.extract_vocabulary_from_data() {
            all_terms.extend(data_terms);
        }

        // From API
        if let Ok(api_terms) = self.extract_vocabulary_from_api() {
            all_terms.extend(api_terms);
        }

        // Limit to top 100 terms by frequency
        let mut term_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for term in &all_terms {
            *term_counts.entry(term.clone()).or_insert(0) += 1;
        }
        
        let mut sorted_terms: Vec<(String, usize)> = term_counts.into_iter().collect();
        sorted_terms.sort_by(|a, b| b.1.cmp(&a.1)); // Sort by frequency descending
        
        let top_100: Vec<String> = sorted_terms.into_iter()
            .take(100)
            .map(|(term, _)| term)
            .collect();

        Ok(top_100)
    }
}

pub struct SuperwhisperPlugin;

impl VocabPlugin for SuperwhisperPlugin {
    fn name(&self) -> &str {
        "Superwhisper"
    }

    fn description(&self) -> &str {
        "Superwhisper - AI-powered speech-to-text application"
    }

    fn get_config(&self) -> PluginConfig {
        let mut config = PluginConfig::default();
        
        // Common Superwhisper configuration paths
        #[cfg(target_os = "macos")]
        {
            if let Ok(home) = std::env::var("HOME") {
                config.config_paths.push(PathBuf::from(format!("{}/Library/Application Support/Superwhisper", home)));
                config.data_paths.push(PathBuf::from(format!("{}/Library/Application Support/Superwhisper/vocabulary.json", home)));
                config.data_paths.push(PathBuf::from(format!("{}/Documents/superwhisper/recordings", home)));
            }
        }
        
        #[cfg(target_os = "windows")]
        {
            if let Ok(appdata) = std::env::var("APPDATA") {
                config.config_paths.push(PathBuf::from(format!(r"{}\Superwhisper", appdata)));
                config.data_paths.push(PathBuf::from(format!(r"{}\Superwhisper\vocabulary.json", appdata)));
                config.data_paths.push(PathBuf::from(format!(r"{}\Superwhisper\recordings", appdata)));
            }
            if let Ok(home) = std::env::var("USERPROFILE") {
                config.data_paths.push(PathBuf::from(format!(r"{}\Documents\Superwhisper\recordings", home)));
            }
        }
        
        #[cfg(target_os = "linux")]
        {
            if let Ok(home) = std::env::var("HOME") {
                config.config_paths.push(PathBuf::from(format!("{}/.config/superwhisper", home)));
                config.data_paths.push(PathBuf::from(format!("{}/.config/superwhisper/vocabulary.json", home)));
                config.data_paths.push(PathBuf::from(format!("{}/Documents/superwhisper/recordings", home)));
            }
        }
        
        config
    }

    fn get_installation_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        
        #[cfg(target_os = "macos")]
        {
            paths.push(PathBuf::from("/Applications/superwhisper.app"));
        }
        
        #[cfg(target_os = "windows")]
        {
            paths.push(PathBuf::from(r"C:\Program Files\Superwhisper"));
            paths.push(PathBuf::from(r"C:\Program Files (x86)\Superwhisper"));
            if let Ok(username) = std::env::var("USERNAME") {
                paths.push(PathBuf::from(format!(r"C:\Users\{}\AppData\Local\Programs\Superwhisper", username)));
            }
        }
        
        #[cfg(target_os = "linux")]
        {
            paths.push(PathBuf::from("/usr/bin/superwhisper"));
            paths.push(PathBuf::from("/usr/local/bin/superwhisper"));
            if let Ok(home) = std::env::var("HOME") {
                paths.push(PathBuf::from(format!("{}/.local/bin/superwhisper", home)));
            }
        }
        
        paths
    }

    fn is_installed(&self) -> bool {
        // Check custom config path first
        let config = self.get_config();
        if let Some(custom_path) = config.install_path {
            if custom_path.exists() {
                return true;
            }
        }
        
        // Check default installation paths
        self.get_installation_paths().iter().any(|p| p.exists())
    }

    fn extract_vocabulary(&self) -> Vec<String> {
        // Superwhisper-specific technical terms
        vec![
            "superwhisper".to_string(),
            "whisper".to_string(),
            "transcription".to_string(),
            "speech-to-text".to_string(),
            "voice recognition".to_string(),
            "audio transcription".to_string(),
            "real-time transcription".to_string(),
            "speaker diarization".to_string(),
            "language detection".to_string(),
            "timestamp".to_string(),
            "subtitles".to_string(),
            "captions".to_string(),
            "hotkey".to_string(),
            "global hotkey".to_string(),
            "recording".to_string(),
            "audio recording".to_string(),
            "microphone".to_string(),
            "audio device".to_string(),
            "output format".to_string(),
            "text editor".to_string(),
            "paste".to_string(),
            "clipboard".to_string(),
            "api key".to_string(),
            "openai".to_string(),
            "groq".to_string(),
            "language model".to_string(),
            "model selection".to_string(),
            "large model".to_string(),
            "medium model".to_string(),
            "small model".to_string(),
            "tiny model".to_string(),
            "multilingual".to_string(),
            "english".to_string(),
            "auto-detect language".to_string(),
            "confidence score".to_string(),
            "word confidence".to_string(),
            "segmentation".to_string(),
            "vad".to_string(),
            "voice activity detection".to_string(),
            "silence detection".to_string(),
            "noise suppression".to_string(),
            "audio normalization".to_string(),
        ]
    }

    fn extract_vocabulary_from_config(&self) -> Result<Vec<String>> {
        let mut terms = Vec::new();
        let config = self.get_config();
        
        // Try to read vocabulary from config files
        for config_path in &config.config_paths {
            if config_path.exists() {
                if let Ok(entries) = std::fs::read_dir(config_path) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().map_or(false, |ext| ext == "json") {
                            if let Ok(content) = std::fs::read_to_string(&path) {
                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                                    extract_strings_from_json(&json, &mut terms);
                                }
                            }
                        }
                    }
                }
            }
        }
        
        Ok(terms)
    }

    fn extract_vocabulary_from_data(&self) -> Result<Vec<String>> {
        let mut terms = Vec::new();
        let config = self.get_config();
        
        // Try to read transcripts from recordings folder
        for data_path in &config.data_paths {
            if data_path.ends_with("recordings") && data_path.exists() {
                if let Ok(entries) = std::fs::read_dir(data_path) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_dir() {
                            // Check if this recording was made with Ultra model
                            let meta_path = path.join("meta.json");
                            if meta_path.exists() {
                                if let Ok(meta_content) = std::fs::read_to_string(&meta_path) {
                                    if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta_content) {
                                        // Check if model is Ultra
                                        let is_ultra = meta.get("modelName")
                                            .and_then(|m| m.as_str())
                                            .map(|s| s.contains("Ultra"))
                                            .unwrap_or(false) ||
                                            meta.get("modelKey")
                                            .and_then(|k| k.as_str())
                                            .map(|s| s.contains("ultra"))
                                            .unwrap_or(false);
                                        
                                        if !is_ultra {
                                            continue; // Skip non-Ultra recordings
                                        }
                                    }
                                }
                            }
                            
                            // Look for transcript files in each recording folder
                            if let Ok(recording_entries) = std::fs::read_dir(&path) {
                                for recording_entry in recording_entries.flatten() {
                                    let recording_path = recording_entry.path();
                                    if recording_path.extension().map_or(false, |ext| {
                                        matches!(ext.to_str(), Some("txt") | Some("json") | Some("md"))
                                    }) {
                                        if let Ok(content) = std::fs::read_to_string(&recording_path) {
                                            // Extract interesting words from transcript
                                            terms.extend(extract_interesting_words(&content));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        
        // Limit to top 100 terms by frequency
        let mut term_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for term in &terms {
            *term_counts.entry(term.clone()).or_insert(0) += 1;
        }
        
        let mut sorted_terms: Vec<(String, usize)> = term_counts.into_iter().collect();
        sorted_terms.sort_by(|a, b| b.1.cmp(&a.1)); // Sort by frequency descending
        
        let top_100: Vec<String> = sorted_terms.into_iter()
            .take(100)
            .map(|(term, _)| term)
            .collect();
        
        Ok(top_100)
    }
}

/// Helper function to extract strings from JSON
fn extract_strings_from_json(value: &serde_json::Value, terms: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => {
            if s.len() >= 3 && s.len() <= 50 {
                terms.push(s.clone());
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                extract_strings_from_json(item, terms);
            }
        }
        serde_json::Value::Object(obj) => {
            for (_key, val) in obj {
                extract_strings_from_json(val, terms);
            }
        }
        _ => {}
    }
}

/// Extract interesting words from transcript content using pure statistical analysis
fn extract_interesting_words(content: &str) -> Vec<String> {
    let mut word_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut total_words = 0;
    
    // Count word frequencies
    for word in content.split_whitespace() {
        let clean_word = word
            .trim()
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-' && c != '_')
            .to_lowercase();
        
        if clean_word.len() >= 3 && clean_word.len() <= 50 {
            *word_counts.entry(clean_word.clone()).or_insert(0) += 1;
            total_words += 1;
        }
    }
    
    if total_words == 0 {
        return Vec::new();
    }
    
    // Calculate frequency statistics
    let frequencies: Vec<f64> = word_counts.values()
        .map(|&count| count as f64 / total_words as f64)
        .collect();
    
    let mean = frequencies.iter().sum::<f64>() / frequencies.len() as f64;
    let variance = frequencies.iter()
        .map(|&f| (f - mean).powi(2))
        .sum::<f64>() / frequencies.len() as f64;
    let std_dev = variance.sqrt();
    
    let mut interesting_words = Vec::new();
    
    // Filter words based on statistical outliers and technical characteristics
    for (word, count) in word_counts {
        let relative_freq = count as f64 / total_words as f64;
        
        // Calculate Z-score (how many standard deviations from mean)
        let z_score = if std_dev > 0.0 {
            (relative_freq - mean) / std_dev
        } else {
            0.0
        };
        
        // Technical term indicators:
        let has_numbers = word.chars().any(|c| c.is_numeric());
        let has_underscore = word.chars().any(|c| c == '_');
        let has_hyphen = word.chars().any(|c| c == '-');
        let has_multiple_special = word.chars().filter(|c| !c.is_alphanumeric()).count() > 1;
        let is_long = word.len() > 8;
        let is_capitalized = word.chars().next().map(|c| c.is_uppercase()).unwrap_or(false);
        
        // Statistical outlier: words with frequency significantly different from mean
        // Either very rare (negative Z-score) or very common (positive Z-score)
        let is_statistical_outlier = z_score.abs() > 1.5;
        
        // Technical characteristics
        let is_technical = has_numbers || has_underscore || has_hyphen || has_multiple_special;
        
        // Keep word if it's a statistical outlier AND has technical characteristics
        // OR if it's very rare (Z-score < -1) and long
        if (is_statistical_outlier && is_technical) || (z_score < -1.0 && is_long && is_capitalized) {
            interesting_words.push(word);
        }
    }
    
    interesting_words
}

pub fn get_all_plugins() -> Vec<Box<dyn VocabPlugin>> {
    vec![
        Box::new(SuperwhisperPlugin),
        Box::new(WisprflowPlugin),
    ]
}

pub struct WisprflowPlugin;

impl VocabPlugin for WisprflowPlugin {
    fn name(&self) -> &str {
        "Wispr Flow"
    }

    fn description(&self) -> &str {
        "Wispr Flow - AI-powered workflow automation tool"
    }

    fn get_config(&self) -> PluginConfig {
        let mut config = PluginConfig::default();
        
        // Common Wispr Flow configuration paths
        #[cfg(target_os = "macos")]
        {
            if let Ok(home) = std::env::var("HOME") {
                config.config_paths.push(PathBuf::from(format!("{}/Library/Application Support/Wispr Flow", home)));
                config.data_paths.push(PathBuf::from(format!("{}/Library/Application Support/Wispr Flow/workflows.json", home)));
            }
        }
        
        #[cfg(target_os = "windows")]
        {
            if let Ok(appdata) = std::env::var("APPDATA") {
                config.config_paths.push(PathBuf::from(format!(r"{}\WisprFlow", appdata)));
                config.data_paths.push(PathBuf::from(format!(r"{}\WisprFlow\workflows.json", appdata)));
            }
        }
        
        #[cfg(target_os = "linux")]
        {
            if let Ok(home) = std::env::var("HOME") {
                config.config_paths.push(PathBuf::from(format!("{}/.config/wisprflow", home)));
                config.data_paths.push(PathBuf::from(format!("{}/.config/wisprflow/workflows.json", home)));
            }
        }
        
        config
    }

    fn get_installation_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        
        #[cfg(target_os = "macos")]
        {
            paths.push(PathBuf::from("/Applications/Wispr Flow.app"));
        }
        
        #[cfg(target_os = "windows")]
        {
            paths.push(PathBuf::from(r"C:\Program Files\Wispr Flow"));
            paths.push(PathBuf::from(r"C:\Program Files (x86)\Wispr Flow"));
            if let Ok(username) = std::env::var("USERNAME") {
                paths.push(PathBuf::from(format!(r"C:\Users\{}\AppData\Local\Programs\Wispr Flow", username)));
            }
        }
        
        #[cfg(target_os = "linux")]
        {
            paths.push(PathBuf::from("/usr/bin/wisprflow"));
            paths.push(PathBuf::from("/usr/local/bin/wisprflow"));
            if let Ok(home) = std::env::var("HOME") {
                paths.push(PathBuf::from(format!("{}/.local/bin/wisprflow", home)));
            }
        }
        
        paths
    }

    fn is_installed(&self) -> bool {
        // Check custom config path first
        let config = self.get_config();
        if let Some(custom_path) = config.install_path {
            if custom_path.exists() {
                return true;
            }
        }
        
        // Check default installation paths
        self.get_installation_paths().iter().any(|p| p.exists())
    }

    fn extract_vocabulary(&self) -> Vec<String> {
        // Wispr Flow-specific technical terms
        vec![
            "wispr flow".to_string(),
            "wisprflow".to_string(),
            "workflow".to_string(),
            "workflow automation".to_string(),
            "automation".to_string(),
            "trigger".to_string(),
            "action".to_string(),
            "node".to_string(),
            "workflow node".to_string(),
            "integration".to_string(),
            "api integration".to_string(),
            "webhook".to_string(),
            "webhook trigger".to_string(),
            "condition".to_string(),
            "condition logic".to_string(),
            "branch".to_string(),
            "workflow branch".to_string(),
            "loop".to_string(),
            "iteration".to_string(),
            "variable".to_string(),
            "workflow variable".to_string(),
            "expression".to_string(),
            "workflow expression".to_string(),
            "template".to_string(),
            "workflow template".to_string(),
            "execution".to_string(),
            "workflow execution".to_string(),
            "execution history".to_string(),
            "log".to_string(),
            "execution log".to_string(),
            "error handling".to_string(),
            "retry".to_string(),
            "retry policy".to_string(),
            "timeout".to_string(),
            "timeout handling".to_string(),
            "schedule".to_string(),
            "scheduled workflow".to_string(),
            "cron".to_string(),
            "cron expression".to_string(),
            "event".to_string(),
            "event trigger".to_string(),
            "notification".to_string(),
            "email notification".to_string(),
            "slack notification".to_string(),
            "discord notification".to_string(),
            "http request".to_string(),
            "http trigger".to_string(),
            "rest api".to_string(),
            "graphql".to_string(),
            "json".to_string(),
            "xml".to_string(),
            "csv".to_string(),
            "data transformation".to_string(),
            "data mapping".to_string(),
            "filter".to_string(),
            "data filter".to_string(),
            "sort".to_string(),
            "data sort".to_string(),
            "map".to_string(),
            "reduce".to_string(),
            "transform".to_string(),
            "pipeline".to_string(),
            "data pipeline".to_string(),
            "connector".to_string(),
            "service connector".to_string(),
            "authentication".to_string(),
            "auth token".to_string(),
            "api key".to_string(),
            "oauth".to_string(),
            "jwt".to_string(),
            "environment variable".to_string(),
            "secret".to_string(),
            "credential".to_string(),
            "deployment".to_string(),
            "version control".to_string(),
            "git".to_string(),
            "rollback".to_string(),
            "monitoring".to_string(),
            "analytics".to_string(),
            "dashboard".to_string(),
            "report".to_string(),
            "export".to_string(),
            "import".to_string(),
            "backup".to_string(),
            "restore".to_string(),
        ]
    }

    fn extract_vocabulary_from_config(&self) -> Result<Vec<String>> {
        let mut terms = Vec::new();
        let config = self.get_config();
        
        // Try to read vocabulary from config files
        for config_path in &config.config_paths {
            if config_path.exists() {
                if let Ok(entries) = std::fs::read_dir(config_path) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().map_or(false, |ext| ext == "json") {
                            if let Ok(content) = std::fs::read_to_string(&path) {
                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                                    extract_strings_from_json(&json, &mut terms);
                                }
                            }
                        }
                    }
                }
            }
        }
        
        Ok(terms)
    }
}
