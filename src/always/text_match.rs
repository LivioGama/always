//! Acoustic custom-word matching ported from Handy
//! (`src-tauri/src/audio_toolkit/text.rs`, MIT, cjpais).
//!
//! Whisper transcripts often mishear domain-specific terms (`cloud` for
//! `Claude`, `kubernetics` for `Kubernetes`, `Charge B` for `ChargeBee`).
//! The LLM postprocess pass catches some of these via the curated
//! `mistranscriptions` map, but it's slow (network round-trip) and
//! doesn't always fire.
//!
//! This module runs BEFORE the LLM as a deterministic fix-up: walk the
//! transcript, for each 1-, 2-, or 3-word window try to match it against
//! the user's canonical glossary terms using a combination of:
//!
//! 1. **Levenshtein distance** (string similarity, normalized)
//! 2. **Soundex phonetic match** (catches misheard pronunciations like
//!    `Claude` ↔ `cloud` that Levenshtein alone would reject)
//! 3. **N-gram fusion** (joins multi-word windows so `Charge B` collapses
//!    to `chargeb` and matches canonical `ChargeBee`)
//!
//! A combined score below `threshold` (default 0.18) wins. When a match
//! lands, the canonical replacement adopts the original word's case
//! pattern (UPPER, Title, lower) and preserves any leading/trailing
//! punctuation.
//!
//! Soundex is implemented inline rather than pulling the `natural` crate
//! to keep the dep tree lean.

use strsim::levenshtein;

/// Default match threshold mirrored from Handy's settings_store.json.
/// Combined score = `levenshtein_normalized * (0.3 if phonetic else 1.0)`,
/// so 0.18 accepts strong typos and any phonetic match within ~60% edit
/// distance.
pub const DEFAULT_THRESHOLD: f64 = 0.18;

/// Maximum n-gram window size. 3 covers cases like `Charge B too` where
/// the third token might still be relevant; longer windows risk
/// over-matching.
const MAX_NGRAM: usize = 3;

/// Maximum candidate length for matching. Prevents pathological long
/// "n-grams" from a single mis-tokenized line.
const MAX_CANDIDATE_LEN: usize = 50;

/// Walk `text` word-by-word and replace n-grams that match a custom
/// vocabulary term within `threshold`. Returns the rewritten text plus
/// the list of `(original_window, canonical)` substitutions applied
/// (useful for logging / pasting into the LLM context).
pub fn apply_custom_words(
    text: &str,
    custom_words: &[String],
    threshold: f64,
) -> (String, Vec<(String, String)>) {
    if custom_words.is_empty() || text.trim().is_empty() {
        return (text.to_string(), Vec::new());
    }

    let custom_words_nospace: Vec<String> = custom_words
        .iter()
        .map(|w| w.to_lowercase().replace(' ', ""))
        .collect();

    let words: Vec<&str> = text.split_whitespace().collect();
    let mut result: Vec<String> = Vec::with_capacity(words.len());
    let mut subs: Vec<(String, String)> = Vec::new();
    let mut i = 0;

    while i < words.len() {
        let mut matched = false;

        // Greedy longest-first: a 3-gram match wins over a 1-gram
        // sub-match, so multi-word canonical terms (`Claude Code`,
        // `pull request`) survive.
        for n in (1..=MAX_NGRAM).rev() {
            if i + n > words.len() {
                continue;
            }
            let window = &words[i..i + n];
            let ngram = build_ngram(window);

            let Some((replacement, _score)) =
                find_best_match(&ngram, custom_words, &custom_words_nospace, threshold)
            else {
                continue;
            };

            let (prefix, _) = extract_punctuation(window[0]);
            let (_, suffix) = extract_punctuation(window[n - 1]);
            let cased = preserve_case_pattern(window[0], replacement);

            let original_window = window.join(" ");
            let rewritten = format!("{prefix}{cased}{suffix}");

            // Only record as a substitution if it actually changed
            // something — exact case-insensitive matches that round-trip
            // would otherwise pollute the log.
            if original_window.to_lowercase() != rewritten.to_lowercase() {
                subs.push((original_window, rewritten.clone()));
            }
            result.push(rewritten);
            i += n;
            matched = true;
            break;
        }

        if !matched {
            result.push(words[i].to_string());
            i += 1;
        }
    }

    (result.join(" "), subs)
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Strip non-alphanumerics from each word, lowercase, concatenate without
/// spaces. Lets `"Charge B"` collapse to `"chargeb"` so it can match
/// canonical `"ChargeBee"` after Levenshtein.
fn build_ngram(words: &[&str]) -> String {
    words
        .iter()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .collect::<Vec<_>>()
        .concat()
}

/// Score `candidate` against every custom word. Levenshtein distance
/// normalized by max length is the base; if the two also share a
/// Soundex code, we multiply the score by 0.3 to favor phonetic
/// matches over pure-string typos.
fn find_best_match<'a>(
    candidate: &str,
    custom_words: &'a [String],
    custom_words_nospace: &[String],
    threshold: f64,
) -> Option<(&'a String, f64)> {
    if candidate.is_empty() || candidate.len() > MAX_CANDIDATE_LEN {
        return None;
    }

    let mut best_match: Option<&String> = None;
    let mut best_score = f64::MAX;

    for (i, custom_word_nospace) in custom_words_nospace.iter().enumerate() {
        // Length-difference fast reject (≥25% of max length, or 2 chars).
        // Catches `openaigpt` ≠ `openai` cases that string similarity
        // would otherwise rank suspiciously close.
        let len_diff = (candidate.len() as i32 - custom_word_nospace.len() as i32).unsigned_abs() as f64;
        let max_len = candidate.len().max(custom_word_nospace.len()) as f64;
        let max_allowed_diff = (max_len * 0.25).max(2.0);
        if len_diff > max_allowed_diff {
            continue;
        }

        let dist = levenshtein(candidate, custom_word_nospace) as f64;
        let lev_score = if max_len > 0.0 { dist / max_len } else { 1.0 };

        let phonetic = soundex(candidate) == soundex(custom_word_nospace)
            && !soundex(candidate).is_empty();

        let combined = if phonetic { lev_score * 0.3 } else { lev_score };

        if combined < threshold && combined < best_score {
            best_match = Some(&custom_words[i]);
            best_score = combined;
        }
    }

    best_match.map(|m| (m, best_score))
}

/// Apply UPPER, Title, or lower casing from `original` to `replacement`.
/// All-uppercase originals (e.g. `KUBERNETES`) → uppercase the
/// replacement; capitalized originals → capitalize the replacement;
/// otherwise leave the canonical form intact.
fn preserve_case_pattern(original: &str, replacement: &str) -> String {
    let alpha: Vec<char> = original.chars().filter(|c| c.is_alphabetic()).collect();
    if alpha.is_empty() {
        return replacement.to_string();
    }
    if alpha.iter().all(|c| c.is_uppercase()) {
        return replacement.to_uppercase();
    }
    if alpha[0].is_uppercase() {
        let mut chars = replacement.chars();
        let first = chars
            .next()
            .map(|c| c.to_uppercase().next().unwrap_or(c))
            .unwrap_or_default();
        return std::iter::once(first).chain(chars).collect();
    }
    replacement.to_string()
}

/// Split `word` into `(leading_punct, trailing_punct)` so we can
/// re-attach surrounding punctuation around the canonical replacement.
fn extract_punctuation(word: &str) -> (&str, &str) {
    let prefix_end = word
        .char_indices()
        .find(|(_, c)| c.is_alphanumeric())
        .map(|(i, _)| i)
        .unwrap_or(word.len());
    let suffix_start = word
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_alphanumeric())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let prefix = &word[..prefix_end];
    let suffix = if suffix_start > 0 && suffix_start < word.len() {
        &word[suffix_start..]
    } else {
        ""
    };
    (prefix, suffix)
}

// ---------------------------------------------------------------------------
// Soundex (American Soundex, length-4 code)
// ---------------------------------------------------------------------------

/// Map a single ASCII letter to its Soundex digit, or `None` for vowels
/// and `h`/`w`/`y` (which are dropped per Soundex rules).
fn soundex_digit(c: char) -> Option<char> {
    match c.to_ascii_lowercase() {
        'b' | 'f' | 'p' | 'v' => Some('1'),
        'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => Some('2'),
        'd' | 't' => Some('3'),
        'l' => Some('4'),
        'm' | 'n' => Some('5'),
        'r' => Some('6'),
        // Vowels and h/w/y are dropped. Anything else (digits,
        // punctuation that survived `build_ngram`) returns None too.
        _ => None,
    }
}

/// Produce the 4-character American Soundex code of `s`. Empty input
/// yields an empty string (rather than `0000`) so phonetic matching
/// can disambiguate "no signal" from "starts with 0-mappable letter".
pub fn soundex(s: &str) -> String {
    let chars: Vec<char> = s
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .collect();
    let Some(&first) = chars.first() else {
        return String::new();
    };

    let mut out = String::with_capacity(4);
    out.push(first.to_ascii_uppercase());
    let first_digit = soundex_digit(first);
    let mut prev = first_digit;

    for c in chars.iter().skip(1) {
        let digit = soundex_digit(*c);
        // Skip H, W, Y (returned None) — but they don't reset the
        // duplicate-collapse state. So `Robert` → R163 not R1163.
        if matches!(*c, 'h' | 'H' | 'w' | 'W' | 'y' | 'Y') {
            continue;
        }
        if let Some(d) = digit {
            if Some(d) != prev {
                out.push(d);
                if out.len() == 4 {
                    return out;
                }
            }
            prev = Some(d);
        } else {
            // Vowel: resets the duplicate-collapse anchor so
            // consecutive same-coded consonants separated by a vowel
            // both emit (e.g. `Tymczak` → T522 not T520).
            prev = None;
        }
    }

    while out.len() < 4 {
        out.push('0');
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn soundex_canonical_examples() {
        // Cases from Knuth's Soundex specification.
        assert_eq!(soundex("Robert"), "R163");
        assert_eq!(soundex("Rupert"), "R163");
        assert_eq!(soundex("Rubin"), "R150");
        assert_eq!(soundex("Ashcraft"), "A261");
        assert_eq!(soundex("Tymczak"), "T522");
        assert_eq!(soundex("Pfister"), "P236");
        assert_eq!(soundex(""), "");
    }

    #[test]
    fn cloud_matches_claude_phonetically() {
        // The flagship test: Whisper hears `Claude` and writes `cloud`.
        // Soundex(cloud)=C430, Soundex(Claude)=C430 — phonetic match.
        // Levenshtein 3 / max-len 6 = 0.5, * 0.3 phonetic = 0.15 < 0.18.
        let custom = words(&["Claude"]);
        let (out, subs) = apply_custom_words("I love cloud today", &custom, DEFAULT_THRESHOLD);
        assert_eq!(out, "I love Claude today");
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].1, "Claude");
    }

    #[test]
    fn multi_word_canonical_matches_via_ngram() {
        // `Charge B` → concatenated `chargeb` → matches `ChargeBee`
        // within threshold.
        let custom = words(&["ChargeBee"]);
        let (out, _) = apply_custom_words("Pay with Charge B today", &custom, DEFAULT_THRESHOLD);
        assert!(out.contains("ChargeBee"), "got: {out}");
    }

    #[test]
    fn punctuation_preserved_around_replacement() {
        let custom = words(&["Claude"]);
        let (out, _) = apply_custom_words("Hi cloud, hello!", &custom, DEFAULT_THRESHOLD);
        assert_eq!(out, "Hi Claude, hello!");
    }

    #[test]
    fn case_pattern_preserved() {
        let custom = words(&["Kubernetes"]);
        let (out, _) = apply_custom_words("KUBERNETICS rocks", &custom, DEFAULT_THRESHOLD);
        assert!(out.starts_with("KUBERNETES"), "got: {out}");

        let (out2, _) = apply_custom_words("kubernetics rocks", &custom, DEFAULT_THRESHOLD);
        // Lowercased input: canonical retains its own case (Kubernetes).
        assert!(out2.starts_with("Kubernetes"), "got: {out2}");
    }

    #[test]
    fn empty_inputs_passthrough() {
        let (out, subs) = apply_custom_words("", &words(&["Claude"]), DEFAULT_THRESHOLD);
        assert_eq!(out, "");
        assert!(subs.is_empty());

        let (out, subs) = apply_custom_words("hello world", &[], DEFAULT_THRESHOLD);
        assert_eq!(out, "hello world");
        assert!(subs.is_empty());
    }

    #[test]
    fn does_not_match_dissimilar_words() {
        let custom = words(&["Kubernetes"]);
        let (out, subs) = apply_custom_words("the cat sat", &custom, DEFAULT_THRESHOLD);
        assert_eq!(out, "the cat sat");
        assert!(subs.is_empty());
    }

    #[test]
    fn length_reject_keeps_short_overmatch_at_bay() {
        // `openaigpt` (9 chars) vs `openai` (6 chars). diff=3, max=9,
        // max_allowed = max(2.25, 2.0) = 2.25. diff > allowed → reject.
        let custom = words(&["openai"]);
        let (out, _) = apply_custom_words("OpenAIGPT", &custom, DEFAULT_THRESHOLD);
        assert_eq!(out, "OpenAIGPT");
    }

    #[test]
    fn substitution_log_excludes_pure_case_changes() {
        // Input "Claude" already matches "Claude" canonical. No real
        // substitution; subs should be empty (case round-trip).
        let custom = words(&["Claude"]);
        let (out, subs) = apply_custom_words("Claude is here", &custom, DEFAULT_THRESHOLD);
        assert_eq!(out, "Claude is here");
        assert!(subs.is_empty());
    }
}
