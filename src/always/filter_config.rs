use regex::Regex;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct FilterConfig {
    pub hard_filter_starts_with: Vec<String>,
    pub hard_filter_exact: Vec<String>,
    pub hard_filter_regex: Vec<String>,
}

pub fn load_filter_config() -> Option<FilterConfig> {
    // Try to load from project root first, then from config directory
    let paths = vec![
        PathBuf::from("filter_config.json"),
        dirs::config_dir()
            .map(|d| d.join("always/filter_config.json"))
            .unwrap_or_default(),
    ];

    for path in paths {
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(config) = serde_json::from_str::<FilterConfig>(&content) {
                    return Some(config);
                }
            }
        }
    }

    None
}

pub fn compile_regex_patterns(patterns: &[String]) -> Vec<Regex> {
    patterns
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect()
}