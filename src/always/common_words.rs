//! Common English words that are also likely to be confused with
//! product/tech names. Used by the tiered matcher to classify
//! mistranscriptions as ambiguous (defer to LLM) or unambiguous
//! (rewrite immediately).
//!
//! This is NOT a general-purpose frequency list. It's a curated set of
//! short, common English words that overlap with tech vocabulary —
//! the exact words that cause false-positive rewrites like
//! "deploy to the cloud" → "deploy to the Claude".
//!
//! A mistranscription that appears in this list is classified as
//! **ambiguous**: the exact tier will NOT rewrite it unconditionally.
//! Instead it becomes a deferred candidate for the LLM to arbitrate
//! with sentence context. When no LLM is available, ambiguous exact
//! hits are skipped (fail safe — better to leave the text unchanged
//! than to rewrite a common word into a product name blindly).

use std::collections::HashSet;
use std::sync::OnceLock;

static COMMON_WORDS: OnceLock<HashSet<&'static str>> = OnceLock::new();

/// Returns true if `word` is a common English word that should be
/// treated as an ambiguous mistranscription (defer to LLM rather
/// than rewrite unconditionally). Case-insensitive.
pub fn is_common_word(word: &str) -> bool {
    let set = COMMON_WORDS.get_or_init(|| {
        let words: &[&str] = &[
            // Nature / weather
            "cloud", "rain", "snow", "wind", "storm", "fire", "ice", "sun", "moon", "star",
            "sky", "sea", "lake", "river", "rock", "stone", "tree", "leaf", "flower", "grass",
            // Body / health
            "arm", "leg", "eye", "ear", "nose", "hand", "foot", "head", "heart", "bone",
            "skin", "hair", "face", "back", "chest", "mind", "body", "life", "death", "soul",
            // Common verbs
            "ask", "use", "open", "close", "start", "stop", "call", "tell", "show", "find",
            "make", "take", "give", "get", "put", "set", "run", "play", "work", "look",
            "come", "go", "see", "know", "think", "say", "tell", "want", "need", "help",
            "fix", "build", "send", "keep", "let", "try", "turn", "move", "live", "feel",
            // Common nouns
            "time", "day", "way", "year", "week", "month", "hour", "home", "house", "room",
            "door", "wall", "floor", "roof", "gate", "path", "road", "park", "city", "town",
            "idea", "plan", "goal", "task", "job", "work", "team", "group", "line", "list",
            "note", "book", "page", "form", "type", "kind", "sort", "part", "side", "base",
            "camp", "lab", "hub", "net", "web", "app", "dev", "pro", "max", "min",
            "edge", "node", "mesh", "grid", "wave", "beam", "spark", "flash", "dash", "dot",
            // Tech-adjacent common words
            "code", "data", "file", "name", "type", "test", "loop", "mode", "port", "host",
            "shell", "pipe", "fork", "branch", "tree", "root", "leaf", "seed", "pool", "cache",
            "block", "chain", "link", "ring", "stack", "queue", "stream", "flow", "source",
            "sink", "state", "event", "signal", "pipe", "hook", "flag", "guard", "lock",
            "key", "token", "seed", "salt", "hash", "digest", "cipher", "core", "cell",
            // Communication
            "mail", "post", "chat", "call", "text", "voice", "video", "meeting", "channel",
            "topic", "thread", "reply", "share", "link", "feed", "story", "post", "blog",
            // Products that are also common words
            "slack", "zoom", "teams", "page", "notion", "form", "base", "camp", "note",
            "suite", "office", "word", "excel", "number", "point", "access", "power",
            "contact", "calendar", "reminder", "alarm", "timer", "counter", "tracker",
            // Common adjectives
            "good", "bad", "big", "small", "fast", "slow", "high", "low", "new", "old",
            "hot", "cold", "warm", "cool", "bright", "dark", "light", "heavy", "full",
            "empty", "open", "close", "sharp", "flat", "round", "square", "long", "short",
            // Colors
            "red", "blue", "green", "yellow", "orange", "purple", "black", "white", "gray",
            "brown", "pink", "gold", "silver",
            // Numbers (spelled out)
            "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
            "first", "second", "third", "last", "next", "zero",
            // Common short words that overlap with tech
            "ap", "pi", "id", "ip", "ui", "ux", "ai", "ml", "db", "qa",
            "it", "is", "in", "on", "at", "by", "of", "to", "or", "as",
            "up", "so", "do", "no", "go", "be", "if", "we", "he", "my",
            // Greek letters (common in tech naming)
            "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta",
            "iota", "kappa", "lambda", "mu", "nu", "xi", "omicron", "pi",
            "rho", "sigma", "tau", "upsilon", "phi", "chi", "psi", "omega",
            // Common tech-adjacent
            "plus", "prime", "pro", "lite", "core", "edge", "zone", "realm", "domain",
            "forge", "craft", "mint", "spark", "pulse", "echo", "shadow", "mirror",
            "crystal", "prism", "beacon", "compass", "anchor", "shield", "crown",
            "tower", "bridge", "tunnel", "portal", "nexus", "vertex", "matrix",
            // Animals (commonly used in product names)
            "fox", "cat", "dog", "bird", "fish", "bear", "wolf", "lion", "tiger",
            "hawk", "eagle", "owl", "crow", "raven", "duck", "swan", "crane",
            "shark", "whale", "dolphin", "ape", "monkey", "snake", "dragon",
        ];
        words.iter().copied().collect()
    });
    set.contains(word.to_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_is_common() {
        assert!(is_common_word("cloud"));
        assert!(is_common_word("Cloud"));
        assert!(is_common_word("CLOUD"));
    }

    #[test]
    fn kubernetes_is_not_common() {
        assert!(!is_common_word("kubernetes"));
        assert!(!is_common_word("kubernetics"));
    }

    #[test]
    fn short_tech_words_are_common() {
        assert!(is_common_word("ap"));
        assert!(is_common_word("code"));
        assert!(is_common_word("line"));
    }

    #[test]
    fn non_words_are_not_common() {
        assert!(!is_common_word("kubernetics"));
        assert!(!is_common_word("cuber"));
        assert!(!is_common_word("chargebee"));
    }
}
