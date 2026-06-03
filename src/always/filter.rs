use crate::always::AlwaysConfig;

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

    // ALWAYS reject thank you and politeness phrases - these should never be pasted
    const ALWAYS_FILTERED: &[&str] = &[
        // Thank you variations (core hard filter)
        "thank you", "thanks a lot", "thank you so much", "thank you very much",
        "thanks so much", "many thanks", "thanks again", "thanks very much",
        "appreciate it", "much appreciated", "really appreciate it", "i appreciate it",
        "thanks",

        // Response to thanks
        "you're welcome", "your welcome", "no problem", "no worries", "don't mention it",
        "my pleasure", "anytime", "happy to help", "glad to help", "of course",

        // Basic farewells that are never commands
        "bye", "goodbye", "good night", "have a good day", "have a nice day", "take care",

        // Common filler words that should never be pasted (excluding hello and ok per user request)
        "uh", "um", "ah", "oh", "hmm", "wow",

        // STT error variations that should always be blocked
        "thank u", "ur welcome", "thankyou",
    ];

    // Check for exact matches
    if ALWAYS_FILTERED.iter().any(|phrase| normalized == *phrase) {
        return true;
    }

    // Handle common STT substitutions for hard filter phrases
    let mut substituted = normalized.clone();
    let substitutions = [
        (" u ", " you "),
        (" r ", " are "),
        (" ur ", " you are "),
        ("ur ", "your "),
        (" u", " you"),
        ("u ", "you "),
    ];

    for (from, to) in &substitutions {
        substituted = substituted.replace(from, to);
    }

    if substituted != normalized && ALWAYS_FILTERED.iter().any(|phrase| substituted == *phrase) {
        return true;
    }

    false
}

pub fn should_accept(text: &str, _cfg: &AlwaysConfig) -> bool {
    // Hard filter always applies - these phrases should NEVER be pasted
    if hard_reject(text) {
        return false;
    }

    // Apply additional filtering for conversational noise
    !quick_reject(text)
}

#[cfg(test)]
mod tests {
    use super::quick_reject;

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

        // Hard filter should always reject these, regardless of context
        assert!(hard_reject("thank you"));
        assert!(hard_reject("Thank You!"));
        assert!(hard_reject("thanks"));
        assert!(hard_reject("thank u"));
        assert!(hard_reject("ur welcome"));
        assert!(hard_reject("you're welcome"));
        assert!(hard_reject("bye"));

        // These should NOT be rejected by hard filter anymore
        assert!(!hard_reject("hello"));
        assert!(!hard_reject("ok"));
        assert!(!hard_reject("okay"));
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
