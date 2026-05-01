use crate::always::{AlwaysConfig, filter_config};
use once_cell::sync::Lazy;
use regex;

// Load filter config once at startup
static FILTER_CONFIG: Lazy<Option<filter_config::FilterConfig>> = Lazy::new(filter_config::load_filter_config);
static COMPILED_REGEX: Lazy<Vec<regex::Regex>> = Lazy::new(|| {
    match &*FILTER_CONFIG {
        Some(config) => filter_config::compile_regex_patterns(&config.hard_filter_regex),
        None => vec![],
    }
});

#[derive(Debug, Clone)]
pub enum FilterReason {
    None,
    MeaninglessSound(String),
    VideoArtifact(String),
    ConfigPhrase(String),
    ConfigRegex(String),
    Onomatopoeia(String),
    Gibberish(String),
    Degeneracy(String),
    NonAscii(String),
}

impl FilterReason {
    pub fn to_log_string(&self) -> String {
        match self {
            FilterReason::None => "No filter".to_string(),
            FilterReason::MeaninglessSound(detail) => format!("Meaningless sound: {}", detail),
            FilterReason::VideoArtifact(detail) => format!("Video artifact: {}", detail),
            FilterReason::ConfigPhrase(detail) => format!("Config phrase: {}", detail),
            FilterReason::ConfigRegex(detail) => format!("Config regex: {}", detail),
            FilterReason::Onomatopoeia(detail) => format!("Sound effect: {}", detail),
            FilterReason::Gibberish(detail) => format!("Gibberish: {}", detail),
            FilterReason::Degeneracy(detail) => format!("Repetitive: {}", detail),
            FilterReason::NonAscii(detail) => format!("Non-ASCII: {}", detail),
        }
    }
}

pub fn quick_reject_with_reason(text: &str) -> FilterReason {
    let normalized = text
        .trim()
        .trim_matches(|ch: char| ch.is_ascii_punctuation())
        .to_lowercase();

    // ONLY reject truly meaningless single sounds/fillers that provide no value
    if normalized.split_whitespace().count() == 1 {
        const MEANINGLESS_SOUNDS: &[&str] = &[
            "uh", "um", "ah", "mmm", "hmm", "uhh", "umm", "ahh", "err",
        ];
        if let Some(sound) = MEANINGLESS_SOUNDS.iter().find(|sound| normalized == **sound) {
            return FilterReason::MeaninglessSound(format!("Single meaningless sound: '{}'", sound));
        }
    }

    FilterReason::None
}

// Legacy function for backward compatibility
pub fn quick_reject(text: &str) -> bool {
    !matches!(quick_reject_with_reason(text), FilterReason::None)
}

/// Hard filter that always rejects certain phrases regardless of filter settings
pub fn hard_reject_with_reason(text: &str) -> FilterReason {
    let normalized = text
        .trim()
        .trim_matches(|ch: char| ch.is_ascii_punctuation())
        .to_lowercase();

    // If no config is loaded, pass everything through (no hard filtering)
    let config = match &*FILTER_CONFIG {
        Some(c) => c,
        None => return FilterReason::None,
    };

    // Check for transcription service watermarks
    if normalized.starts_with("transcription by") || normalized.contains("transcription by") {
        return FilterReason::VideoArtifact("Transcription service watermark".to_string());
    }

    // FIRST: Check if text STARTS with any configured phrases - block EVERYTHING starting with these
    for start_phrase in &config.hard_filter_starts_with {
        if normalized.starts_with(start_phrase) {
            return FilterReason::VideoArtifact(format!("Starts with blocked phrase: '{}'", start_phrase));
        }
    }

    // Handle STT substitutions for phrases at start
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
                return FilterReason::VideoArtifact(format!("Starts with blocked phrase after substitution: '{}'", start_phrase));
            }
        }
    }

    // THEN: Check exact matches for other filtered phrases
    for phrase in &config.hard_filter_exact {
        if normalized == *phrase {
            return FilterReason::ConfigPhrase(format!("Exact match: '{}'", phrase));
        }
    }

    // FINALLY: Check regex patterns
    for (i, regex) in COMPILED_REGEX.iter().enumerate() {
        if regex.is_match(&normalized) {
            let pattern = config.hard_filter_regex.get(i).map(|s| s.as_str()).unwrap_or("unknown");
            return FilterReason::ConfigRegex(format!("Matches pattern: '{}'", pattern));
        }
    }

    FilterReason::None
}

// Legacy function for backward compatibility
pub fn hard_reject(text: &str) -> bool {
    !matches!(hard_reject_with_reason(text), FilterReason::None)
}

/// Filter onomatopoeia and nonsense sounds using pattern analysis
pub fn onomatopoeia_reject(text: &str) -> bool {
    let normalized = text
        .trim()
        .trim_matches(|ch: char| ch.is_ascii_punctuation())
        .to_lowercase();

    let words: Vec<&str> = normalized.split_whitespace().collect();

    // Single word that's very short or has repeated characters
    if words.len() == 1 {
        let word = words[0];

        // Allow common valid short words and technical terms
        const VALID_SHORT_WORDS: &[&str] = &[
            "yes", "no", "ok", "hi", "bye", "go", "stop", "wait", "run", "get", "set",
            "add", "new", "old", "big", "top", "end", "use", "try", "see", "say", "do",
            "can", "may", "will", "was", "are", "the", "and", "but", "for", "you", "all",
            "api", "sql", "css", "jwt", "css", "url", "uri", "xml", "dom", "npm", "git",
            "ssh", "tcp", "udp", "tls", "ssl", "cdn", "dns", "php", "css", "jsx", "tsx"
        ];

        if VALID_SHORT_WORDS.contains(&word) {
            return false;
        }

        // Very short text (1-2 chars) is likely just a sound - AFTER checking allowlist
        if normalized.len() <= 2 {
            return true;
        }

        // Check for repeated single characters (e.g., "aaa", "mmm", "uuu")
        if word.len() >= 2 && word.chars().collect::<Vec<char>>().windows(2).all(|w| w[0] == w[1]) {
            return true;
        }

        // Very short single word (2-3 chars) that's not in allowlist is likely a sound
        if word.len() <= 3 {
            return true;
        }
    }

    // Check for alternating repeated patterns (e.g., "abab", "lalala")
    if normalized.len() >= 6 {
        let chars: Vec<char> = normalized.chars().collect();
        let pattern = &chars[0..2];
        let mut matches = true;
        for i in (0..normalized.len()).step_by(2) {
            if i + 1 < normalized.len() {
                if &chars[i..i+2] != pattern {
                    matches = false;
                    break;
                }
            }
        }
        if matches && normalized.len() % 2 == 0 {
            return true;
        }
    }

    false
}

/// Degeneracy detection based on ship-fast's htmlLooksDegenerate algorithm
/// Filters text with repetitive patterns (repeated words, bigrams, low diversity)
pub fn degeneracy_reject(text: &str) -> bool {
    let normalized = text
        .trim()
        .trim_matches(|ch: char| ch.is_ascii_punctuation())
        .to_lowercase();

    let words: Vec<&str> = normalized.split_whitespace().collect();

    if words.is_empty() {
        return true;
    }

    // Check for repeated words (max run > 5 for speech, stricter than ship-fast's 45)
    let mut run = 1;
    let mut max_run = 1;
    for i in 1..words.len() {
        if words[i] == words[i - 1] {
            run += 1;
            max_run = max_run.max(run);
        } else {
            run = 1;
        }
    }
    if max_run > 5 {
        return true;
    }

    // Check for repeated bigrams (count > 20 for speech, stricter than ship-fast's 80)
    if words.len() > 10 {
        use std::collections::HashMap;
        let mut bigrams: HashMap<String, usize> = HashMap::new();
        for i in 0..words.len().saturating_sub(1) {
            let bigram = format!("{} {}", words[i].to_lowercase(), words[i + 1].to_lowercase());
            *bigrams.entry(bigram).or_insert(0) += 1;
        }
        for count in bigrams.values() {
            if *count > 20 {
                return true;
            }
        }
    }

    // Check for low unique word ratio (< 0.15 for speech, stricter than ship-fast's 0.06)
    if words.len() > 20 {
        let unique: std::collections::HashSet<&str> = words.iter().cloned().collect();
        let ratio = unique.len() as f64 / words.len() as f64;
        if ratio < 0.15 {
            return true;
        }
    }

    // Check for very short text (less than 2 words) - but allow common valid single words
    if words.len() < 2 {
        // Allow common single-word responses, commands, and technical terms
        const VALID_SINGLE_WORDS: &[&str] = &[
            "yes", "no", "ok", "okay", "hi", "hello", "bye", "goodbye", "thanks", "stop", "wait",
            "go", "run", "get", "set", "add", "new", "old", "big", "small", "open", "close",
            "save", "load", "help", "start", "end", "up", "down", "left", "right", "next", "back",
            "api", "sql", "json", "html", "css", "http", "rest", "crud", "oauth", "jwt", "xml",
            "dom", "npm", "git", "ssh", "tcp", "udp", "tls", "ssl", "cdn", "dns", "php", "jsx", "tsx",
            "ios", "app", "web", "dev", "cli", "sdk", "ide", "url", "uri", "aws", "gcp", "vpc",
            "continue", "cancel", "exit",
            // Popular frameworks and libraries
            "react.js", "node.js", "vue.js", "angular.js", "next.js", "express.js", "nest.js",
            "react", "angular", "vue", "svelte", "nuxt", "gatsby", "webpack", "vite", "rollup"
        ];

        if words.len() == 1 && VALID_SINGLE_WORDS.contains(&words[0]) {
            return false; // Don't reject valid single words
        }

        return true; // Reject other single words and empty text
    }

    false
}

/// Gibberish detection based on commit 01f51f9 by Abhi
/// Uses vowel/consonant analysis to detect keyboard mashing and unpronounceable text
pub fn gibberish_reject(text: &str) -> bool {
    let normalized = text
        .trim()
        .trim_matches(|ch: char| ch.is_ascii_punctuation())
        .to_lowercase();

    let words: Vec<&str> = normalized.split_whitespace().collect();

    // Check for keyboard layout patterns (qwerty rows, adjacent keys)
    const KEYBOARD_PATTERNS: &[&str] = &[
        "qwerty", "asdf", "zxcv", "uiop", "hjkl", "bnm",
        "qwertyuiop", "asdfghjkl", "zxcvbnm",
        "qwe", "asd", "zxc", "uio", "hjk", "bnm"
    ];

    for word in &words {
        if KEYBOARD_PATTERNS.contains(word) {
            return true;
        }
    }

    // Reject if multiple words are just random keyboard sequences
    if words.len() >= 3 {
        let keyboard_words = words.iter()
            .filter(|w| KEYBOARD_PATTERNS.contains(w))
            .count();
        if keyboard_words >= 2 {
            return true;
        }
    }

    // Reject if it has many mixed fragments that look like partial words
    if words.len() > 3 {
        let suspicious_words = words.iter()
            .filter(|w| {
                let len = w.len();
                // Words that are 6+ chars but have no common English patterns
                len >= 6 && (
                    w.contains("diy") ||  // Random tech acronyms mixed in gibberish
                    // Only flag words ending in "in" if they don't match common English patterns
                    (w.ends_with("in") && len > 8 &&
                     !w.ends_with("ing") && !w.ends_with("tion") &&
                     !w.ends_with("sion") && !w.ends_with("atin") && !w.ends_with("erin") &&
                     !w.ends_with("ain") && !w.ends_with("oin")) ||
                    !w.chars().any(|c| matches!(c, 'e' | 'a' | 'i' | 'o' | 'u')) // No vowels
                )
            })
            .count();

        if suspicious_words >= 2 {
            return true;
        }
    }

    // Check for words that are clearly corrupted/truncated (like "distrvDIY")
    for word in &words {
        if word.len() >= 6 {
            let has_mixed_case_in_middle = word.chars()
                .enumerate()
                .any(|(i, c)| i > 1 && i < word.len() - 1 && c.is_uppercase());

            let has_few_vowels = word.chars()
                .filter(|c| matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u'))
                .count() < 2;

            if has_mixed_case_in_middle || (word.len() >= 8 && has_few_vowels) {
                return true;
            }
        }
    }

    // Extract alphabetic characters only
    let alpha_only: String = normalized.chars().filter(|c| c.is_ascii_alphabetic()).collect();

    // Vowel ratio check: real language has ~30-50% vowels; keyboard mashing has very few
    if alpha_only.len() >= 30 {  // Lowered from 40 to catch shorter gibberish
        let vowels = alpha_only.chars().filter(|c| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')).count();
        let vowel_ratio = vowels as f64 / alpha_only.len() as f64;
        if vowel_ratio < 0.18 {  // Slightly increased from 0.15 to be less aggressive
            return true;
        }
    }

    // Consonant cluster check: gibberish has long runs without vowels (e.g., "jfsdkljfsdjfds")
    let mut cluster_count = 0;
    let mut current_cluster_len = 0;
    for c in alpha_only.chars() {
        if matches!(c, 'a' | 'e' | 'i' | 'o' | 'u') {
            if current_cluster_len >= 5 {  // Back to 5 from 6 - be more aggressive
                cluster_count += 1;
            }
            current_cluster_len = 0;
        } else {
            current_cluster_len += 1;
        }
    }
    if current_cluster_len >= 5 {  // Back to 5 from 6
        cluster_count += 1;
    }
    if cluster_count >= 3 {  // Back to 3 from 5 - catch more gibberish
        return true;
    }

    // Check if most 4+ letter words have no vowels at all
    let long_words: Vec<&str> = words.iter()
        .filter(|w| w.chars().filter(|c| c.is_ascii_alphabetic()).count() >= 4)
        .cloned()
        .collect();

    if long_words.len() >= 3 {
        let no_vowel_count = long_words.iter()
            .filter(|w| !w.chars().any(|c| matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u')))
            .count();
        if no_vowel_count as f64 / long_words.len() as f64 > 0.5 {
            return true;
        }
    }

    // Additional check: if text contains random capitalization mixed with gibberish
    // Only trigger for truly random caps (like "eLlO WoRLd" patterns), not normal sentences
    if normalized != text.trim().to_lowercase() {
        let words_orig: Vec<&str> = text.trim().split_whitespace().collect();
        let words_lower: Vec<&str> = normalized.split_whitespace().collect();

        let mut random_caps_words = 0;
        for (orig_word, _lower_word) in words_orig.iter().zip(words_lower.iter()) {
            if orig_word.len() > 3 {
                // Count internal capitals (not first/last letter)
                let internal_caps = orig_word.chars()
                    .enumerate()
                    .filter(|(i, c)| *i > 0 && *i < orig_word.len() - 1 && c.is_uppercase())
                    .count();

                // Only flag words with multiple internal capitals (like "eLlO" or "WoRLd")
                if internal_caps >= 2 {
                    random_caps_words += 1;
                }
            }
        }

        // Only reject if multiple words have random internal capitalization
        if random_caps_words >= 3 {
            return true;
        }
    }

    false
}

/// Filter for non-ASCII characters and mixed language nonsense
pub fn non_ascii_reject(text: &str) -> bool {
    let normalized = text.trim().to_lowercase();

    // Allow common accented characters in European languages (French, Spanish, etc.)
    let european_accents = ['á', 'à', 'â', 'ä', 'ã', 'é', 'è', 'ê', 'ë', 'í', 'ì', 'î', 'ï', 'ó', 'ò', 'ô', 'ö', 'õ', 'ú', 'ù', 'û', 'ü', 'ý', 'ÿ', 'ñ', 'ç', 'ß'];

    let mut non_ascii_chars = 0;
    let mut total_chars = 0;

    for ch in normalized.chars() {
        if ch.is_alphabetic() {
            total_chars += 1;
            if !ch.is_ascii() && !european_accents.contains(&ch) {
                non_ascii_chars += 1;
            }
        }
    }

    // If text has substantial non-European characters (> 20% of alphabetic chars), likely gibberish
    if total_chars > 3 && non_ascii_chars as f64 / total_chars as f64 > 0.2 {
        return true;
    }

    // Reject if it contains CJK characters mixed with random Latin - common in garbled transcripts
    let has_cjk = normalized.chars().any(|c| {
        matches!(c,
            '\u{4E00}'..='\u{9FFF}' |  // Chinese
            '\u{3040}'..='\u{309F}' |  // Hiragana
            '\u{30A0}'..='\u{30FF}' |  // Katakana
            '\u{AC00}'..='\u{D7AF}'    // Korean
        )
    });

    let has_latin = normalized.chars().any(|c| c.is_ascii_alphabetic());

    // If it has both CJK and Latin characters, and Latin part looks random, reject
    if has_cjk && has_latin {
        let words: Vec<&str> = normalized.split_whitespace().collect();
        // If multiple words with mixed scripts, likely garbage
        if words.len() > 1 {
            return true;
        }
    }

    false
}

pub fn should_accept_with_reason(text: &str, _cfg: &AlwaysConfig) -> FilterReason {
    // Hard filter always applies - these phrases should NEVER be pasted
    let hard_result = hard_reject_with_reason(text);
    if !matches!(hard_result, FilterReason::None) {
        return hard_result;
    }

    // Filter onomatopoeia and nonsense sounds
    if onomatopoeia_reject(text) {
        return FilterReason::Onomatopoeia("Repetitive sound pattern or very short utterance".to_string());
    }

    // Filter gibberish (keyboard mashing, unpronounceable text)
    if gibberish_reject(text) {
        return FilterReason::Gibberish("Unpronounceable text or keyboard mashing".to_string());
    }

    // Filter non-ASCII and mixed language nonsense
    if non_ascii_reject(text) {
        return FilterReason::NonAscii("Contains non-ASCII characters".to_string());
    }

    // Filter degenerate content (repetitive patterns)
    if degeneracy_reject(text) {
        return FilterReason::Degeneracy("Repetitive or low-diversity content".to_string());
    }

    // Apply additional filtering for conversational noise
    quick_reject_with_reason(text)
}

pub fn should_accept(text: &str, cfg: &AlwaysConfig) -> bool {
    matches!(should_accept_with_reason(text, cfg), FilterReason::None)
}

#[cfg(test)]
mod tests {
    use super::{quick_reject, onomatopoeia_reject, non_ascii_reject};

    #[test]
    fn rejects_onomatopoeia_sounds() {
        assert!(onomatopoeia_reject("aaa"));
        assert!(onomatopoeia_reject("mmm"));
        assert!(onomatopoeia_reject("uuu"));
        assert!(onomatopoeia_reject("zzz"));
        assert!(onomatopoeia_reject("lalala"));
        assert!(onomatopoeia_reject("nanana"));
        assert!(onomatopoeia_reject("bababa"));
        assert!(onomatopoeia_reject("a"));
        assert!(onomatopoeia_reject("ab"));
        assert!(onomatopoeia_reject("abc"));
    }

    #[test]
    fn allows_actual_commands_with_onomatopoeia_words() {
        assert!(!onomatopoeia_reject("open burp file")); // "burp" as part of a command
        assert!(!onomatopoeia_reject("create boom function")); // "boom" as part of a command
        assert!(!onomatopoeia_reject("zap the bug")); // "zap" as part of a command
    }

    #[test]
    fn rejects_mixed_language_nonsense() {
        // Test with individual filters since AlwaysConfig doesn't have Default
        let text = "In collaboration with çalışatobeltragende. tell debe";
        // The non_ascii filter now uses 20% threshold for non-ASCII characters
        assert!(non_ascii_reject(text));
    }

    #[test]
    fn rejects_single_fillers_only() {
        assert!(quick_reject("uh"));
        assert!(quick_reject("okay."));
        assert!(!quick_reject("uh open settings"));
    }

    #[test]
    fn rejects_thank_you_phrases() {
        assert!(quick_reject("thank you"));
        assert!(quick_reject("Thank you."));
        assert!(quick_reject("thank you so much"));
        assert!(quick_reject("thanks very much"));
        assert!(quick_reject("thanks"));
        assert!(quick_reject("much appreciated"));
        assert!(quick_reject("you're welcome"));
        assert!(quick_reject("your welcome")); // Common typo
    }

    #[test]
    fn rejects_apologies() {
        assert!(quick_reject("i'm sorry"));
        assert!(quick_reject("sorry about that"));
        assert!(quick_reject("my apologies"));
        assert!(quick_reject("excuse me"));
    }

    #[test]
    fn rejects_greetings_and_farewells() {
        assert!(quick_reject("good morning"));
        assert!(quick_reject("good night"));
        assert!(quick_reject("how are you"));
        assert!(quick_reject("see you later"));
        assert!(quick_reject("have a nice day"));
        assert!(quick_reject("take care"));
    }

    #[test]
    fn rejects_conversation_fillers() {
        assert!(quick_reject("mm hmm"));
        assert!(quick_reject("uh huh"));
        assert!(quick_reject("oh okay"));
        assert!(quick_reject("sounds good"));
        assert!(quick_reject("that's great"));
        assert!(quick_reject("absolutely"));
    }

    #[test]
    fn rejects_common_stt_errors() {
        assert!(quick_reject("thank u")); // Should substitute "u" -> "you"
        assert!(quick_reject("ur welcome")); // Should substitute "ur" -> "you're"
    }

    #[test]
    fn rejects_partial_politeness_with_filler() {
        assert!(quick_reject("thank you so much"));
        assert!(quick_reject("thank you very much"));
        assert!(quick_reject("good morning everyone"));
        assert!(quick_reject("see you later"));
        assert!(quick_reject("how are you doing"));
    }

    #[test]
    fn allows_actual_commands() {
        assert!(!quick_reject("open file"));
        assert!(!quick_reject("git status"));
        assert!(!quick_reject("thank you for the help")); // Longer phrases should pass through
        assert!(!quick_reject("show me the thank you message")); // Commands containing filtered words
        assert!(!quick_reject("create a good morning function")); // Commands with filtered phrases
        assert!(!quick_reject("set excuse me as variable")); // Commands with filtered content
    }

    #[test]
    fn allows_commands_with_embedded_politeness() {
        assert!(!quick_reject("display thank you message"));
        assert!(!quick_reject("send good morning email"));
        assert!(!quick_reject("write a sorry function"));
        assert!(!quick_reject("debug the hello there function"));
    }

    #[test]
    fn hard_filter_always_rejects_thank_you() {
        use super::hard_reject;

        // These should ALWAYS be rejected by hard filter
        assert!(hard_reject("Thank You!"));
        assert!(hard_reject("thanks"));
        assert!(hard_reject("thank u"));
        assert!(hard_reject("ur welcome"));
        assert!(hard_reject("you're welcome"));
        assert!(hard_reject("bye"));
    }

    #[test]
    fn hard_filter_allows_actual_commands() {
        use super::hard_reject;

        // These should NOT be rejected by hard filter (but might be by quick_reject)
        assert!(!hard_reject("create thank you function"));
        assert!(!hard_reject("send a thank you email"));
        assert!(!hard_reject("open hello world file"));
        assert!(!hard_reject("git status"));
        assert!(!hard_reject("run tests"));
    }
}
