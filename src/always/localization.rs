//! Locale-specific knobs for post-processing heuristics.
//!
//! Lives in its own module (rather than under `speech_action.rs` where
//! the heuristics consume it, or under `config.rs` where it's stored)
//! so both modules can import it without a circular dependency.
//!
//! Defaults to English ([`Localization::ENGLISH`]). A non-English user
//! can construct a custom value with their language's sentence
//! terminators and "safe to lowercase mid-sentence" word list, store
//! it on [`crate::always::config::AlwaysConfig`], and the merge logic
//! will use it without any other code change.

/// Words that Whisper often capitalizes at sentence start which are
/// safe to lowercase when appending mid-sentence. Conservative on
/// purpose — anything not in this set is left as-is so proper nouns
/// (Kubernetes, John, Berlin) are preserved when starting a continuation.
///
/// "I" and its contractions are intentionally absent: they MUST stay
/// capitalized even mid-sentence.
pub const ENGLISH_SAFE_LOWERCASE_STARTERS: &[&str] = &[
    "The", "A", "An", "And", "But", "Or", "So", "Yet", "Nor",
    "If", "When", "While", "Because", "Since", "Although", "Though",
    "Then", "Now", "Just", "Also", "Only", "Even", "Still",
    "We", "You", "They", "He", "She", "It",
    "This", "That", "These", "Those", "There", "Here",
    "In", "On", "At", "To", "For", "Of", "With", "Without", "From",
    "By", "About", "Into", "Onto", "Over", "Under", "Through",
    "As", "Is", "Are", "Was", "Were", "Will", "Would", "Should",
    "Could", "Can", "May", "Might", "Must", "Has", "Have", "Had", "Do",
    "Does", "Did", "Be", "Been", "Being",
    "Some", "Any", "All", "Most", "Many", "Few", "Each", "Every",
    "My", "Your", "Our", "Their", "His", "Her", "Its",
    "So", "Such", "What", "Which", "How", "Why", "Where",
];

/// English sentence terminators. Kept as a `&[char]` so non-English
/// locales can add their own (e.g. U+0964 DEVANAGARI DANDA `।` for
/// Hindi, U+3002 IDEOGRAPHIC FULL STOP `。` for CJK).
pub const ENGLISH_SENTENCE_TERMINATORS: &[char] = &['.', '!', '?'];

/// Locale-specific knobs for the pure post-processing heuristics.
///
/// The defaults match the historic English behavior. To support a
/// different language, build a value with the appropriate starter
/// list + terminator set and store it on
/// [`crate::always::config::AlwaysConfig`].
///
/// # Examples
///
/// English default (no allocation):
/// ```
/// use always::always::localization::Localization;
/// let en = Localization::ENGLISH;
/// assert!(en.sentence_terminators.contains(&'.'));
/// assert!(en.safe_lowercase_starters.contains(&"The"));
/// ```
///
/// Custom locale (e.g. Hindi with Devanagari danda):
/// ```
/// use always::always::localization::Localization;
/// const HI: Localization = Localization {
///     safe_lowercase_starters: &[],
///     sentence_terminators: &['।', '?', '!'],
/// };
/// assert!(HI.sentence_terminators.contains(&'।'));
/// ```
#[derive(Debug, Clone, Copy)]
#[must_use = "a Localization is only useful when stored on AlwaysConfig or passed to merge_dictation_with"]
pub struct Localization {
    pub safe_lowercase_starters: &'static [&'static str],
    pub sentence_terminators: &'static [char],
}

impl Localization {
    /// Default English heuristics. Constant so callers can use this
    /// without paying for an allocation or a lock.
    pub const ENGLISH: Self = Self {
        safe_lowercase_starters: ENGLISH_SAFE_LOWERCASE_STARTERS,
        sentence_terminators: ENGLISH_SENTENCE_TERMINATORS,
    };
}

impl Default for Localization {
    fn default() -> Self {
        Self::ENGLISH
    }
}
