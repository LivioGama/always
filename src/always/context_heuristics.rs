//! Lightweight context heuristics for ambiguous-term resolution
//! when no LLM is available.
//!
//! When the tiered matcher flags an ambiguous exact or fuzzy match
//! (e.g. "cloud" → "Claude"), and there's no LLM to arbitrate, these
//! heuristics check the surrounding words for context cues that
//! strongly suggest one meaning over the other.
//!
//! Design principles:
//! - **Fail safe**: when in doubt, leave the text unchanged.
//! - **Strong cues only**: weak or ambiguous context defaults to
//!   skipping the correction.
//! - **Product vs ordinary**: the core distinction is whether the
//!   word is being used as a product/person name (apply correction)
//!   or as an ordinary English word (skip correction).
//!
//! Examples:
//! - "ask cloud to fix it" → "ask Claude to fix it" (product context)
//! - "deploy to the cloud" → "deploy to the cloud" (ordinary context)
//! - "cloud is great" → "cloud is great" (ambiguous, fail safe)

/// Decision returned by the context heuristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeuristicDecision {
    /// The context strongly suggests the correction should apply.
    Apply,
    /// The context suggests the ordinary meaning, or is ambiguous.
    /// Leave the text unchanged.
    Skip,
}

/// Verbs that strongly suggest a product/person name when followed
/// by the ambiguous word. "ask cloud" → "ask Claude".
const PRODUCT_CONTEXT_VERBS: &[&str] = &[
    "ask", "asks", "asked", "asking",
    "tell", "tells", "told", "telling",
    "prompt", "prompts", "prompted", "prompting",
    "query", "queries", "queried",
    "call", "calls", "called", "calling",
    "message", "messages", "messaged",
    "email", "emailed", "emails",
    "ping", "pings", "pinged",
    "mention", "mentions", "mentioned",
    "thank", "thanks", "thanked",
    "greet", "greets", "greeted",
    "hear", "hears", "heard",
    "love", "loves", "loved",
    "hate", "hates",
    "like", "likes", "liked",
];

/// Prepositions/articles that strongly suggest the ordinary meaning.
/// "deploy to the cloud" → keep "cloud".
const ORDINARY_CONTEXT_PRECEDING: &[&str] = &[
    "to", "to the", "in the", "on the", "into the", "onto the",
    "via the", "through the", "from the", "over the",
    "a", "an", "the", "my", "your", "our", "their",
    "this", "that", "some", "any", "every",
];

/// Words that follow the ambiguous word and suggest ordinary meaning.
/// "cloud storage", "cloud computing", "cloud infrastructure".
const ORDINARY_CONTEXT_FOLLOWING: &[&str] = &[
    "storage", "computing", "infrastructure", "services", "service",
    "provider", "platform", "server", "servers", "deployment",
    "hosting", "account", "instance", "cluster", "database",
    "function", "functions", "lambda", "bucket", "cdn",
    "sync", "backup", "backups", "archive",
];

/// Check whether the context around `word` in `sentence` suggests
/// applying the correction (product/person meaning) or skipping
/// (ordinary meaning).
///
/// `word` is the ambiguous word as it appears in the sentence.
/// `sentence` is the full sentence (or the available window around
/// the word).
pub fn decide(word: &str, sentence: &str) -> HeuristicDecision {
    let tokens: Vec<&str> = sentence.split_whitespace().collect();
    if tokens.is_empty() {
        return HeuristicDecision::Skip;
    }

    let word_lower = word.to_lowercase();
    let word_idx = tokens
        .iter()
        .position(|t| t.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase() == word_lower);

    let Some(idx) = word_idx else {
        return HeuristicDecision::Skip;
    };

    // Check preceding token(s) for product-context verbs.
    if idx > 0 {
        let prev = strip_punct(tokens[idx - 1]).to_lowercase();
        if PRODUCT_CONTEXT_VERBS.contains(&prev.as_str()) {
            return HeuristicDecision::Apply;
        }
        // Check two-word preceding context ("to the cloud").
        if idx > 1 {
            let prev2 = strip_punct(tokens[idx - 2]).to_lowercase();
            let two_word = format!("{prev2} {}", strip_punct(tokens[idx - 1]).to_lowercase());
            if ORDINARY_CONTEXT_PRECEDING.contains(&two_word.as_str())
                || ORDINARY_CONTEXT_PRECEDING.contains(&prev2.as_str())
            {
                return HeuristicDecision::Skip;
            }
            if ORDINARY_CONTEXT_PRECEDING.contains(&prev.as_str()) {
                return HeuristicDecision::Skip;
            }
        } else if ORDINARY_CONTEXT_PRECEDING.contains(&prev.as_str()) {
            return HeuristicDecision::Skip;
        }
    }

    // Check following token for ordinary-context nouns.
    if idx + 1 < tokens.len() {
        let next = strip_punct(tokens[idx + 1]).to_lowercase();
        if ORDINARY_CONTEXT_FOLLOWING.contains(&next.as_str()) {
            return HeuristicDecision::Skip;
        }
    }

    // No strong cue either way — fail safe.
    HeuristicDecision::Skip
}

fn strip_punct(s: &str) -> String {
    s.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_cloud_suggests_product() {
        assert_eq!(decide("cloud", "ask cloud to fix it"), HeuristicDecision::Apply);
    }

    #[test]
    fn deploy_to_the_cloud_suggests_ordinary() {
        assert_eq!(
            decide("cloud", "deploy this to the cloud today"),
            HeuristicDecision::Skip
        );
    }

    #[test]
    fn cloud_storage_suggests_ordinary() {
        assert_eq!(
            decide("cloud", "we use cloud storage for backups"),
            HeuristicDecision::Skip
        );
    }

    #[test]
    fn told_cloud_suggests_product() {
        assert_eq!(
            decide("cloud", "I told cloud to refactor the module"),
            HeuristicDecision::Apply
        );
    }

    #[test]
    fn ambiguous_context_fails_safe() {
        assert_eq!(
            decide("cloud", "cloud is great today"),
            HeuristicDecision::Skip
        );
    }

    #[test]
    fn prompt_cloud_suggests_product() {
        assert_eq!(
            decide("cloud", "prompt cloud for a code review"),
            HeuristicDecision::Apply
        );
    }

    #[test]
    fn in_the_cloud_suggests_ordinary() {
        assert_eq!(
            decide("cloud", "our data lives in the cloud"),
            HeuristicDecision::Skip
        );
    }

    #[test]
    fn word_not_found_fails_safe() {
        assert_eq!(
            decide("cloud", "the weather is nice"),
            HeuristicDecision::Skip
        );
    }
}
