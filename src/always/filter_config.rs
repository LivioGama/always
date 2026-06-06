use regex::Regex;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct FilterConfig {
    pub hard_filter_starts_with: Vec<String>,
    pub hard_filter_exact: Vec<String>,
    pub hard_filter_regex: Vec<String>,
}

fn default_filter_config() -> FilterConfig {
    FilterConfig {
        hard_filter_starts_with: vec![
            "thank you for watching",
            "thanks for watching",
            "thank you for listening",
            "thanks for listening",
            "subtitles by the",
            "subtitles by",
            "captions by the",
            "captions by",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        hard_filter_exact: vec![
            "subtitles",
            "closed captions",
            "captions",
            "subtitle",
            "caption",
            "turn on subtitles",
            "turn off subtitles",
            "enable subtitles",
            "disable subtitles",
            "subtitle settings",
            "caption settings",
            "subtitles available",
            "captions available",
            "auto-generated subtitles",
            "automatic captions",
            "subtitles by the amara.org community",
            "captions by the amara.org community",
            "cc",
            // Standalone politeness / conversational fillers
            "thank you",
            "thanks",
            "thank you so much",
            "thanks a lot",
            "you're welcome",
            "your welcome",
            "excuse me",
            "i'm sorry",
            "sorry",
            "pardon",
            "good morning",
            "good night",
            "good afternoon",
            "good evening",
            "how are you",
            "see you later",
            "have a nice day",
            "take care",
            "sounds good",
            "that's great",
            "absolutely",
            "mm hmm",
            "uh huh",
            "mwah",
            "pff",
            "pfff",
            "oh okay",
            "oh ok",
            "bye",
            "goodbye",
            "bye bye",
            "i'm going to go",
            "i am going to go",
            "i'm gonna go",
            "i gotta go",
            "hello",
            "hi",
            "hey",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        hard_filter_regex: vec![
            r"^thank.*for.*watching$",
            r"^thanks.*for.*watching$",
            r"^thank.*for.*listening$",
            r"^thanks.*for.*listening$",
            r".*amara\.org.*",
            r".*subtitle.*community.*",
            r".*caption.*community.*",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
    }
}

pub fn load_filter_config() -> Option<FilterConfig> {
    let paths = vec![
        PathBuf::from("filter_config.json"),
        dirs::config_dir()
            .map(|d| d.join("always/filter_config.json"))
            .unwrap_or_default(),
    ];

    for path in paths {
        if path.exists()
            && let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(config) = serde_json::from_str::<FilterConfig>(&content)
        {
            tracing::info!(path = %path.display(), "filter_config_loaded");
            return Some(config);
        }
    }

    tracing::debug!("filter_config_using_defaults");
    Some(default_filter_config())
}

pub fn compile_regex_patterns(patterns: &[String]) -> Vec<Regex> {
    patterns.iter().filter_map(|p| Regex::new(p).ok()).collect()
}
