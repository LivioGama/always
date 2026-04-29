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

pub fn quick_reject(text: &str) -> bool {
    let normalized = text
        .trim()
        .trim_matches(|ch: char| ch.is_ascii_punctuation())
        .to_lowercase();

    // Check for single word fillers
    if normalized.split_whitespace().count() == 1 {
        const SINGLE_FILLERS: &[&str] = &[
            "ok", "okay", "yes", "no", "yeah", "yep", "nope", "hmm", "uh", "um", "ah", "oh", "wow",
            "cool", "nice", "sure", "thanks", "welcome", "hello", "hi", "bye", "goodbye",
        ];
        if SINGLE_FILLERS.iter().any(|filler| normalized == *filler) {
            return true;
        }
    }

    // Enhanced politeness and courtesy phrases filter
    const POLITENESS_PHRASES: &[&str] = &[
        // Thank you variations
        "thank you", "thanks a lot", "thank you so much", "thank you very much",
        "thanks so much", "many thanks", "thanks again", "thanks very much",
        "appreciate it", "much appreciated", "really appreciate it", "i appreciate it",

        // Response to thanks
        "you're welcome", "your welcome", "no problem", "no worries", "don't mention it",
        "my pleasure", "anytime", "happy to help", "glad to help", "of course",

        // Apologies
        "i'm sorry", "sorry about that", "sorry for that", "my apologies", "i apologize",
        "excuse me", "pardon me", "forgive me", "my bad",

        // Greetings and farewells
        "good morning", "good afternoon", "good evening", "good night", "good day",
        "have a good day", "have a nice day", "have a great day", "take care",
        "see you later", "talk to you later", "catch you later", "bye bye",
        "goodbye now", "see you soon", "until next time",

        // Small talk
        "how are you", "how's it going", "how you doing", "what's up", "how are things",
        "how's everything", "how have you been", "nice to see you", "good to see you",

        // Conversation fillers and acknowledgments
        "mm hmm", "uh huh", "oh okay", "alright then", "sounds good", "that's good",
        "i see", "got it", "makes sense", "fair enough", "right on", "absolutely",
        "definitely", "totally", "exactly", "for sure", "no doubt", "indeed",

        // Polite interjections
        "excuse me", "pardon", "what was that", "come again", "say that again",
        "could you repeat that", "one more time", "i didn't catch that",

        // Generic positive responses that aren't commands
        "that's great", "that's awesome", "that's wonderful", "that's perfect",
        "excellent", "fantastic", "brilliant", "amazing", "incredible",

        // Common filler words as phrases
        "you know", "i mean", "like", "so", "well", "anyway", "basically", "actually",

        // Background conversation noise (often misheard)
        "hello there", "hi there", "over here", "over there", "right here",
        "just a second", "one moment", "give me a second", "hang on", "wait a minute",
    ];

    // Check if the entire normalized text matches any politeness phrase
    if POLITENESS_PHRASES.iter().any(|phrase| normalized == *phrase) {
        return true;
    }

    // Check for variations with common transcription errors
    // Handle common STT substitutions (whole word replacements)
    let common_substitutions = [
        ("thank u", "thank you"),
        ("ur welcome", "you're welcome"),
        ("ur", "your"),       // General "ur" -> "your" for other contexts
    ];

    for (from, to) in &common_substitutions {
        if normalized == *from {
            return POLITENESS_PHRASES.iter().any(|phrase| *phrase == *to);
        }
    }

    // Also check for single character substitutions in context
    let mut substituted = normalized.clone();
    let single_char_substitutions = [
        (" u ", " you "),     // "u" in middle of phrase
        (" r ", " are "),     // "r" in middle of phrase
        (" c ", " see "),     // "c" in middle of phrase
        (" 2 ", " to "),      // "2" in middle of phrase
        (" 4 ", " for "),     // "4" in middle of phrase
    ];

    for (from, to) in &single_char_substitutions {
        substituted = substituted.replace(from, to);
    }

    // Handle end-of-phrase substitutions
    if substituted.ends_with(" u") {
        substituted = substituted.replace(" u", " you");
    }
    if substituted.starts_with("u ") {
        substituted = substituted.replace("u ", "you ");
    }

    if substituted != normalized && POLITENESS_PHRASES.iter().any(|phrase| substituted == *phrase) {
        return true;
    }

    // Reject if it starts with politeness phrases but is slightly longer (with filler)
    const POLITENESS_STARTS: &[&str] = &[
        "thank you", "thanks", "sorry", "excuse me", "good morning", "good afternoon",
        "good evening", "how are you", "see you", "talk to you", "have a good"
    ];

    for start_phrase in POLITENESS_STARTS {
        if normalized.starts_with(start_phrase) {
            let remainder = normalized[start_phrase.len()..].trim();
            // If what follows is just filler words, reject it
            const FILLER_ENDINGS: &[&str] = &[
                "", "so much", "very much", "a lot", "again", "there", "later",
                "soon", "day", "night", "doing", "going", "everyone", "everybody"
            ];
            if FILLER_ENDINGS.iter().any(|ending| remainder == *ending) {
                return true;
            }
        }
    }

    false
}

/// Hard filter that always rejects certain phrases regardless of filter settings
pub fn hard_reject(text: &str) -> bool {
    let normalized = text
        .trim()
        .trim_matches(|ch: char| ch.is_ascii_punctuation())
        .to_lowercase();

    // If no config is loaded, pass everything through (no hard filtering)
    let config = match &*FILTER_CONFIG {
        Some(c) => c,
        None => return false,
    };

    // FIRST: Check if text STARTS with any configured phrases - block EVERYTHING starting with these
    for start_phrase in &config.hard_filter_starts_with {
        if normalized.starts_with(start_phrase) {
            return true;
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
                return true;
            }
        }
    }

    // THEN: Check exact matches for other filtered phrases
    for phrase in &config.hard_filter_exact {
        if normalized == *phrase {
            return true;
        }
    }

    // FINALLY: Check regex patterns
    for regex in &*COMPILED_REGEX {
        if regex.is_match(&normalized) {
            return true;
        }
    }

    false
}

/// Filter onomatopoeia and nonsense sounds using pattern analysis
pub fn onomatopoeia_reject(text: &str) -> bool {
    let normalized = text
        .trim()
        .trim_matches(|ch: char| ch.is_ascii_punctuation())
        .to_lowercase();

    let words: Vec<&str> = normalized.split_whitespace().collect();

    // Very short text (1-2 chars) is likely just a sound
    if normalized.len() <= 2 {
        return true;
    }

    // Single word that's very short or has repeated characters
    if words.len() == 1 {
        let word = words[0];
        // Check for repeated single characters (e.g., "aaa", "mmm", "uuu")
        if word.chars().collect::<Vec<char>>().windows(2).all(|w| w[0] == w[1]) {
            return true;
        }
        // Very short single word (2-3 chars) is likely a sound
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

    // Check for repeated words (max run > 10 for speech, stricter than ship-fast's 45)
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
    if max_run > 10 {
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

    // Check for very short text (less than 2 words)
    if words.len() < 2 {
        return true;
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

    // Extract alphabetic characters only
    let alpha_only: String = normalized.chars().filter(|c| c.is_ascii_alphabetic()).collect();

    // Vowel ratio check: real language has ~30-50% vowels; keyboard mashing has very few
    if alpha_only.len() >= 40 {
        let vowels = alpha_only.chars().filter(|c| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')).count();
        let vowel_ratio = vowels as f64 / alpha_only.len() as f64;
        if vowel_ratio < 0.15 {
            return true;
        }
    }

    // Consonant cluster check: gibberish has long runs without vowels (e.g., "jfsdkljfsdjfds")
    // Match runs of 5+ consonants (equivalent to JavaScript /[^aeiou]{5,}/g)
    let mut cluster_count = 0;
    let mut current_cluster_len = 0;
    for c in alpha_only.chars() {
        if matches!(c, 'a' | 'e' | 'i' | 'o' | 'u') {
            if current_cluster_len >= 5 {
                cluster_count += 1;
            }
            current_cluster_len = 0;
        } else {
            current_cluster_len += 1;
        }
    }
    if current_cluster_len >= 5 {
        cluster_count += 1;
    }
    if cluster_count >= 3 {
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

    false
}

/// Filter for non-ASCII characters and mixed language nonsense
pub fn non_ascii_reject(text: &str) -> bool {
    // Check for non-ASCII characters (like ç, ñ, ü, etc.)
    let non_ascii_count = text.chars().filter(|c| !c.is_ascii()).count();
    
    // Any non-ASCII character in the text is suspicious for voice commands
    if non_ascii_count > 0 {
        return true;
    }
    
    false
}

pub fn should_accept(text: &str, _cfg: &AlwaysConfig) -> bool {
    // Hard filter always applies - these phrases should NEVER be pasted
    if hard_reject(text) {
        return false;
    }

    // Filter onomatopoeia and nonsense sounds
    if onomatopoeia_reject(text) {
        return false;
    }

    // Filter gibberish (keyboard mashing, unpronounceable text)
    if gibberish_reject(text) {
        return false;
    }

    // Filter non-ASCII and mixed language nonsense
    if non_ascii_reject(text) {
        return false;
    }

    // Filter degenerate content (repetitive patterns)
    if degeneracy_reject(text) {
        return false;
    }

    // Apply additional filtering for conversational noise
    !quick_reject(text)
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
