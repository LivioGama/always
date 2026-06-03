use crate::always::{AlwaysConfig, filter_config};
use regex;
use std::sync::LazyLock;

// Load filter config once at startup
static FILTER_CONFIG: LazyLock<Option<filter_config::FilterConfig>> =
    LazyLock::new(filter_config::load_filter_config);
static COMPILED_REGEX: LazyLock<Vec<regex::Regex>> = LazyLock::new(|| match &*FILTER_CONFIG {
    Some(config) => filter_config::compile_regex_patterns(&config.hard_filter_regex),
    None => vec![],
});

#[derive(Debug, Clone)]
pub enum FilterReason {
    None,
    HardPhrase(String),
    HardRegex(String),
}

impl FilterReason {
    pub fn to_log_string(&self) -> String {
        match self {
            FilterReason::None => "No filter".to_string(),
            FilterReason::HardPhrase(detail) => format!("Phrase filter: {}", detail),
            FilterReason::HardRegex(detail) => format!("Regex filter: {}", detail),
        }
    }
}

/// Hard phrase filter: exact matches and STT corrections (simple, high-precision)
pub fn hard_reject_with_reason(text: &str) -> FilterReason {
    let normalized = text
        .trim()
        .trim_matches(|ch: char| ch.is_ascii_punctuation())
        .to_lowercase();

    let config = match &*FILTER_CONFIG {
        Some(c) => c,
        None => return FilterReason::None,
    };

    // Check if text STARTS with any blocked phrases
    for start_phrase in &config.hard_filter_starts_with {
        if normalized.starts_with(start_phrase) {
            return FilterReason::HardPhrase(format!("Starts with: '{}'", start_phrase));
        }
    }

    // STT correction: handle common misheard words (u→you, ur→your, thankyou→thank you)
    let mut substituted = normalized.clone();
    let substitutions = [
        (" u ", " you "),
        ("thank u", "thank you"),
        ("thankyou", "thank you"),
        ("ur ", "your "),
    ];
    for (from, to) in &substitutions {
        substituted = substituted.replace(from, to);
    }

    if substituted != normalized {
        for start_phrase in &config.hard_filter_starts_with {
            if substituted.starts_with(start_phrase) {
                return FilterReason::HardPhrase(format!(
                    "Starts with (after correction): '{}'",
                    start_phrase
                ));
            }
        }
    }

    // Exact phrase matches (check both original and corrected)
    for phrase in &config.hard_filter_exact {
        if normalized == *phrase || substituted == *phrase {
            return FilterReason::HardPhrase(phrase.clone());
        }
    }

    // Regex patterns
    for (i, regex) in COMPILED_REGEX.iter().enumerate() {
        if regex.is_match(&substituted) {
            let pattern = config
                .hard_filter_regex
                .get(i)
                .map(|s| s.as_str())
                .unwrap_or("unknown");
            return FilterReason::HardRegex(pattern.to_string());
        }
    }

    FilterReason::None
}

pub fn should_accept_with_reason(text: &str, _cfg: &AlwaysConfig) -> FilterReason {
    // Hard filter always applies - these phrases should NEVER be pasted
    hard_reject_with_reason(text)
}

pub fn quick_reject_with_reason(text: &str) -> FilterReason {
    hard_reject_with_reason(text)
}

pub fn onomatopoeia_reject(text: &str) -> bool {
    matches!(hard_reject_with_reason(text), FilterReason::HardRegex(_))
}

pub fn gibberish_reject(text: &str) -> bool {
    matches!(hard_reject_with_reason(text), FilterReason::HardRegex(_))
}

pub fn non_ascii_reject(text: &str) -> bool {
    matches!(hard_reject_with_reason(text), FilterReason::HardRegex(_))
}

pub fn should_accept(text: &str, cfg: &AlwaysConfig) -> bool {
    matches!(should_accept_with_reason(text, cfg), FilterReason::None)
}

#[cfg(test)]
mod tests {
    use super::{FilterReason, hard_reject_with_reason};

    fn is_rejected(text: &str) -> bool {
        !matches!(hard_reject_with_reason(text), FilterReason::None)
    }

    #[test]
    fn rejects_thank_you_phrases() {
        assert!(is_rejected("thank you"));
        assert!(is_rejected("Thank you."));
        assert!(is_rejected("thanks"));
        assert!(is_rejected("you're welcome"));
        assert!(is_rejected("your welcome"));
    }

    #[test]
    fn rejects_apologies() {
        assert!(is_rejected("i'm sorry"));
        assert!(is_rejected("sorry"));
        assert!(is_rejected("excuse me"));
    }

    #[test]
    fn rejects_greetings_and_farewells() {
        assert!(is_rejected("good morning"));
        assert!(is_rejected("good night"));
        assert!(is_rejected("how are you"));
        assert!(is_rejected("see you later"));
        assert!(is_rejected("have a nice day"));
        assert!(is_rejected("take care"));
    }

    #[test]
    fn rejects_conversation_fillers() {
        assert!(is_rejected("mm hmm"));
        assert!(is_rejected("uh huh"));
        assert!(is_rejected("oh okay"));
        assert!(is_rejected("sounds good"));
        assert!(is_rejected("that's great"));
        assert!(is_rejected("absolutely"));
    }

    #[test]
    fn rejects_common_stt_errors() {
        assert!(is_rejected("thank u"));
        assert!(is_rejected("ur welcome"));
    }

    #[test]
    fn rejects_thank_you_for_watching() {
        assert!(is_rejected("Thank you for watching!"));
        assert!(is_rejected("Thanks for watching"));
        assert!(is_rejected("thank you for listening"));
    }

    #[test]
    fn allows_actual_commands() {
        assert!(!is_rejected("thank you for the help"));
        assert!(!is_rejected("show me the thank you message"));
        assert!(!is_rejected("create a good morning function"));
        assert!(!is_rejected("set excuse me as variable"));
        assert!(!is_rejected("display thank you message"));
        assert!(!is_rejected("send good morning email"));
        assert!(!is_rejected("write a sorry function"));
        assert!(!is_rejected("debug the hello there function"));
        assert!(!is_rejected("create thank you function"));
        assert!(!is_rejected("send a thank you email"));
        assert!(!is_rejected("open hello world file"));
        assert!(!is_rejected("git status"));
        assert!(!is_rejected("run tests"));
    }
}
