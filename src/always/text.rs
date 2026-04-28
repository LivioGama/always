use std::collections::HashMap;

use regex::Regex;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Vocabulary {
    corrections: HashMap<String, String>,
    patterns: Vec<PatternRule>,
}

#[derive(Debug, Clone)]
pub struct PatternRule {
    regex: Regex,
    replacement: String,
    quote_kind: QuoteKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteKind {
    Single,
    Double,
    Backtick,
    None,
}

#[derive(Debug, Deserialize)]
struct RawVocabulary {
    #[serde(default)]
    corrections: HashMap<String, String>,
    #[serde(default)]
    patterns: Vec<RawPatternRule>,
}

#[derive(Debug, Deserialize)]
struct RawPatternRule {
    pattern: String,
    replacement: String,
    #[serde(default)]
    add_quotes: bool,
    #[serde(default)]
    quote_type: Option<String>,
}

impl Vocabulary {
    pub fn load() -> Option<Self> {
        let path = dirs::home_dir()?.join(".always").join("vocabulary.json");
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str::<RawVocabulary>(&content)
            .ok()
            .map(Self::from_raw)
    }

    pub fn load_or_default() -> Self {
        Self::load().unwrap_or_else(Self::default_patterns)
    }

    pub fn default_patterns() -> Self {
        Self::from_raw(RawVocabulary {
            corrections: HashMap::new(),
            patterns: vec![
                RawPatternRule {
                    pattern: r"\bdot\s+(\w+)".to_string(),
                    replacement: r".$1".to_string(),
                    add_quotes: false,
                    quote_type: None,
                },
                RawPatternRule {
                    pattern: r"[\w/\-.]+/[\w/\-.]+".to_string(),
                    replacement: r"$0".to_string(),
                    add_quotes: true,
                    quote_type: Some("backtick".to_string()),
                },
                RawPatternRule {
                    pattern: r"(^|[\s(])([\w-]+\.[a-z]{2,4})\b".to_string(),
                    replacement: r"$1`$2`".to_string(),
                    add_quotes: false,
                    quote_type: None,
                },
                RawPatternRule {
                    pattern: r"\b(git\s+\w+|npm\s+\w+|cargo\s+\w+|bun\s+\w+)".to_string(),
                    replacement: r"`$1`".to_string(),
                    add_quotes: false,
                    quote_type: None,
                },
            ],
        })
    }

    pub fn apply(&self, text: &str) -> String {
        let mut result = text.to_string();
        let mut corrections: Vec<_> = self.corrections.iter().collect();
        corrections.sort_by_key(|(wrong, _)| std::cmp::Reverse(wrong.len()));

        for (wrong, correct) in corrections {
            result = result.replace(wrong, correct);
        }

        for rule in &self.patterns {
            let replacement = rule.replacement();
            result = rule
                .regex
                .replace_all(&result, replacement.as_str())
                .to_string();
        }

        result
    }

    fn from_raw(raw: RawVocabulary) -> Self {
        let patterns = raw
            .patterns
            .into_iter()
            .filter_map(PatternRule::from_raw)
            .collect();
        Self {
            corrections: raw.corrections,
            patterns,
        }
    }
}

impl PatternRule {
    fn from_raw(raw: RawPatternRule) -> Option<Self> {
        let regex = Regex::new(&raw.pattern).ok()?;
        Some(Self {
            regex,
            replacement: raw.replacement,
            quote_kind: if raw.add_quotes {
                QuoteKind::parse(raw.quote_type.as_deref())
            } else {
                QuoteKind::None
            },
        })
    }

    fn replacement(&self) -> String {
        match self.quote_kind {
            QuoteKind::Single => format!("'{}'", self.replacement),
            QuoteKind::Double => format!("\"{}\"", self.replacement),
            QuoteKind::Backtick => format!("`{}`", self.replacement),
            QuoteKind::None => self.replacement.clone(),
        }
    }
}

impl QuoteKind {
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("single") => Self::Single,
            Some("double") => Self::Double,
            Some("backtick") | None => Self::Backtick,
            _ => Self::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_dot_words() {
        let vocab = Vocabulary::default_patterns();
        assert_eq!(vocab.apply("dot iris"), ".iris");
    }

    #[test]
    fn applies_corrections_before_patterns() {
        let vocab = Vocabulary::from_raw(RawVocabulary {
            corrections: HashMap::from([("wanna".to_string(), "want to".to_string())]),
            patterns: vec![RawPatternRule {
                pattern: r"\bdot\s+(\w+)".to_string(),
                replacement: r".$1".to_string(),
                add_quotes: false,
                quote_type: None,
            }],
        });

        assert_eq!(vocab.apply("I wanna edit dot env"), "I want to edit .env");
    }

    #[test]
    fn quotes_full_match_when_requested() {
        let vocab = Vocabulary::default_patterns();
        assert_eq!(vocab.apply("open src/main.rs"), "open `src/main.rs`");
    }
}
