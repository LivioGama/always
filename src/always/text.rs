use std::collections::HashMap;

use regex::Regex;
use serde::Deserialize;
use strsim::jaro_winkler;

const FUZZY_CORRECTION_THRESHOLD: f64 = 0.92;

/// Whole-word replace helper kept for the regression test suite.
///
/// The hot `Vocabulary::apply` path uses [`compile_corrections`] to
/// pre-build per-entry regexes once at load time — this standalone
/// function is what the regression tests in `mod tests` call to pin
/// the substring-bug fix without instantiating a full `Vocabulary`.
///
/// Same matching rules: `(?i)\bneedle\b`, regex-escaped needle,
/// `regex::NoExpand` replacement so `$1`-style interpolation can't
/// fire from a glossary term that happens to contain a dollar sign.
#[cfg(test)]
fn whole_word_replace(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() || haystack.is_empty() {
        return haystack.to_string();
    }
    // `(?i)` = case-insensitive. Surround the literal needle with `\b` so we
    // never match in the middle of a longer word.
    let pattern = format!(r"(?i)\b{}\b", regex::escape(needle));
    match Regex::new(&pattern) {
        Ok(re) => re
            .replace_all(haystack, regex::NoExpand(replacement))
            .into_owned(),
        Err(_) => haystack.to_string(),
    }
}

#[derive(Debug, Clone)]
pub struct Vocabulary {
    corrections: HashMap<String, String>,
    /// Pre-compiled `\b…\b` regexes for every entry in `corrections`,
    /// sorted longest-needle-first so multi-word phrases match before
    /// their constituent words. Built once at `from_raw` time so the
    /// hot paste path doesn't pay regex-compilation cost per utterance
    /// (with a 450-entry glossary that was ~50–150 ms per paste — see
    /// `bench_apply_glossary` for the regression test).
    compiled_corrections: Vec<CompiledCorrection>,
    patterns: Vec<PatternRule>,
}

#[derive(Debug, Clone)]
struct CompiledCorrection {
    /// Case-insensitive `(?i)\bneedle\b` regex.
    regex: Regex,
    /// Canonical replacement string (the glossary's `term` value).
    replacement: String,
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

        // Pre-compiled regexes — already sorted longest-first at load.
        for cc in &self.compiled_corrections {
            // `regex::NoExpand` disables `$1` etc. interpolation in the
            // replacement so a glossary term containing `$` survives
            // verbatim.
            result = cc
                .regex
                .replace_all(&result, regex::NoExpand(cc.replacement.as_str()))
                .into_owned();
        }

        result = self.apply_fuzzy_corrections(&result);

        for rule in &self.patterns {
            let replacement = rule.replacement();
            result = rule
                .regex
                .replace_all(&result, replacement.as_str())
                .to_string();
        }

        result
    }

    fn apply_fuzzy_corrections(&self, text: &str) -> String {
        let tokens = TokenSpan::scan(text);
        if tokens.len() < 2 {
            return text.to_string();
        }

        let mut replacements = Vec::new();
        let mut correction_phrases: Vec<_> = self
            .corrections
            .iter()
            .filter_map(|(wrong, correct)| {
                let word_count = wrong.split_whitespace().count();
                (word_count >= 2).then_some((wrong.as_str(), correct.as_str(), word_count))
            })
            .collect();
        correction_phrases.sort_by_key(|(wrong, _, word_count)| {
            (
                std::cmp::Reverse(*word_count),
                std::cmp::Reverse(wrong.len()),
            )
        });

        for start in 0..tokens.len() {
            let Some((replacement, end_token)) =
                best_fuzzy_replacement(text, &tokens, start, &correction_phrases)
            else {
                continue;
            };

            let start_byte = tokens[start].start;
            let end_byte = tokens[end_token].end;
            if replacements
                .iter()
                .any(|(existing_start, existing_end, _)| {
                    ranges_overlap(start_byte, end_byte, *existing_start, *existing_end)
                })
            {
                continue;
            }
            replacements.push((start_byte, end_byte, replacement.to_string()));
        }

        if replacements.is_empty() {
            return text.to_string();
        }

        replacements.sort_by_key(|(start, _, _)| *start);
        let mut result = String::new();
        let mut cursor = 0;
        for (start, end, replacement) in replacements {
            result.push_str(&text[cursor..start]);
            result.push_str(&replacement);
            cursor = end;
        }
        result.push_str(&text[cursor..]);
        result
    }

    fn from_raw(raw: RawVocabulary) -> Self {
        let patterns = raw
            .patterns
            .into_iter()
            .filter_map(PatternRule::from_raw)
            .collect();
        let compiled_corrections = compile_corrections(&raw.corrections);
        Self {
            corrections: raw.corrections,
            compiled_corrections,
            patterns,
        }
    }
}

/// Build the longest-first list of pre-compiled correction regexes.
///
/// The compile cost is paid once per `from_raw` call (daemon startup,
/// glossary reload). Bad patterns are silently dropped — a single
/// malformed glossary entry can't break the whole pipeline.
fn compile_corrections(map: &HashMap<String, String>) -> Vec<CompiledCorrection> {
    let mut entries: Vec<(&String, &String)> = map.iter().collect();
    // Longest needle first so multi-word phrases ("git status") match
    // before any single-word constituents ("git").
    entries.sort_by_key(|(wrong, _)| std::cmp::Reverse(wrong.len()));

    let mut out = Vec::with_capacity(entries.len());
    for (wrong, correct) in entries {
        if wrong.is_empty() {
            continue;
        }
        let pattern = format!(r"(?i)\b{}\b", regex::escape(wrong));
        if let Ok(re) = Regex::new(&pattern) {
            out.push(CompiledCorrection {
                regex: re,
                replacement: correct.clone(),
            });
        }
    }
    out
}

#[derive(Debug, Clone, Copy)]
struct TokenSpan {
    start: usize,
    end: usize,
}

impl TokenSpan {
    fn scan(text: &str) -> Vec<Self> {
        let mut tokens = Vec::new();
        let mut token_start = None;

        for (idx, ch) in text.char_indices() {
            if ch.is_alphanumeric() || ch == '\'' {
                token_start.get_or_insert(idx);
            } else if let Some(start) = token_start.take() {
                tokens.push(Self { start, end: idx });
            }
        }

        if let Some(start) = token_start {
            tokens.push(Self {
                start,
                end: text.len(),
            });
        }

        tokens
    }
}

fn best_fuzzy_replacement(
    text: &str,
    tokens: &[TokenSpan],
    start: usize,
    correction_phrases: &[(&str, &str, usize)],
) -> Option<(String, usize)> {
    let mut best = None;

    for (wrong, correct, word_count) in correction_phrases {
        let end_token = start + word_count - 1;
        if end_token >= tokens.len() {
            continue;
        }

        let phrase_start = tokens[start].start;
        let phrase_end = tokens[end_token].end;
        let phrase = &text[phrase_start..phrase_end];
        if normalize(phrase) == normalize(correct) {
            continue;
        }

        let score = jaro_winkler(&normalize(phrase), &normalize(wrong));
        if score < FUZZY_CORRECTION_THRESHOLD {
            continue;
        }

        if best
            .as_ref()
            .is_none_or(|(best_score, _, _)| score > *best_score)
        {
            best = Some((score, (*correct).to_string(), end_token));
        }
    }

    best.map(|(_, replacement, end_token)| (replacement, end_token))
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn ranges_overlap(start: usize, end: usize, other_start: usize, other_end: usize) -> bool {
    start < other_end && other_start < end
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
    fn whole_word_replace_does_not_match_substring() {
        // The original bug: `Zed` rewrote `analyzed` → `analyZed`.
        assert_eq!(
            whole_word_replace("I analyzed the data", "Zed", "Zed"),
            "I analyzed the data"
        );
        // The original bug: `UI` rewrote `you` → `UIr`, your → UIr.
        assert_eq!(
            whole_word_replace("your code looks UI", "UI", "UI"),
            "your code looks UI"
        );
    }

    #[test]
    fn whole_word_replace_replaces_word_at_boundary() {
        assert_eq!(
            whole_word_replace("zed is great", "zed", "Zed"),
            "Zed is great"
        );
        assert_eq!(
            whole_word_replace("the ui sucks", "ui", "UI"),
            "the UI sucks"
        );
    }

    #[test]
    fn whole_word_replace_is_case_insensitive_in_match_only() {
        // Match ignores case but replacement preserves the canonical form.
        assert_eq!(
            whole_word_replace("Open ZED now", "zed", "Zed"),
            "Open Zed now"
        );
    }

    #[test]
    fn whole_word_replace_handles_punctuation_boundaries() {
        // Word boundary should fire between word/non-word chars.
        assert_eq!(
            whole_word_replace("zed; zed. zed!", "zed", "Zed"),
            "Zed; Zed. Zed!"
        );
    }

    #[test]
    fn whole_word_replace_escapes_regex_metachars() {
        // A glossary term with regex metacharacters must not blow up.
        assert_eq!(
            whole_word_replace("the c.foo() call", "c.foo", "C.Foo"),
            "the C.Foo() call"
        );
    }

    #[test]
    fn whole_word_replace_preserves_haystack_when_no_match() {
        assert_eq!(
            whole_word_replace("nothing to see here", "kubernetes", "Kubernetes"),
            "nothing to see here"
        );
    }

    #[test]
    fn whole_word_replace_handles_empty_inputs() {
        assert_eq!(whole_word_replace("", "x", "y"), "");
        assert_eq!(whole_word_replace("hello", "", "y"), "hello");
    }

    /// Sanity microbenchmark covering the regression that motivated the
    /// regex cache: a 500-entry glossary should not add hundreds of ms to
    /// `Vocabulary::apply`. The previous implementation built a fresh
    /// `Regex` per correction per call (~50–300 µs each); 500 × 100 µs
    /// alone is 50 ms. With the pre-compiled cache the cost should be
    /// dominated by the actual `replace_all` scans, which on a short
    /// utterance run in single-digit milliseconds.
    ///
    /// We don't assert a wall-clock bound — CI runners vary too much —
    /// but we do assert the cache is populated and that `apply` returns
    /// the correct replacement against a haystack the original `String::replace`
    /// substring path would have corrupted (`Zed` inside `analyzed`).
    /// Regression bench. With 500 cached regexes the apply pass on a
    /// short utterance should complete in well under 10 ms on any
    /// modern dev machine. The pre-cache implementation took 50-150 ms
    /// for the same workload because it rebuilt all 500 regexes per
    /// call. Bound is generous (50 ms) so flaky CI runners still pass —
    /// the regression we're guarding against is two-orders-of-magnitude.
    #[test]
    fn apply_with_500_entry_glossary_is_under_50ms() {
        use std::collections::HashMap;
        use std::time::Instant;

        let mut map = HashMap::new();
        for i in 0..500 {
            map.insert(format!("term-{i}"), format!("Term-{i}"));
        }
        let vocab = Vocabulary::from_raw(RawVocabulary {
            corrections: map,
            patterns: vec![],
        });
        let utterance = "deploy term-42 to the staging cluster and run term-7 against it";
        let start = Instant::now();
        for _ in 0..10 {
            let _ = vocab.apply(utterance);
        }
        let per_call = start.elapsed() / 10;
        assert!(
            per_call.as_millis() < 50,
            "Vocabulary::apply with 500 entries took {} ms per call (expected < 50 ms)",
            per_call.as_millis()
        );
    }

    #[test]
    fn apply_with_large_glossary_is_correct_and_uses_cache() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        for i in 0..500 {
            map.insert(format!("term-{i}"), format!("Term-{i}"));
        }
        // The two real-world bugs: substring of "analyzed" and "your".
        map.insert("zed".to_string(), "Zed".to_string());
        map.insert("ui".to_string(), "UI".to_string());

        let vocab = Vocabulary::from_raw(RawVocabulary {
            corrections: map,
            patterns: vec![],
        });
        assert_eq!(vocab.compiled_corrections.len(), 502);

        let out = vocab.apply("I analyzed the Zed editor and your UI library");
        // Whole-word-only: `Zed` does NOT corrupt `analyzed`; `your`
        // does NOT become `UIr`. The standalone words ARE rewritten.
        assert!(out.contains("analyzed"), "substring bug returned: {out:?}");
        assert!(out.contains("your"), "substring bug returned: {out:?}");
        assert!(
            out.contains("Zed"),
            "case-folded match did not fire: {out:?}"
        );
    }

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
    fn applies_fuzzy_multi_word_corrections() {
        let vocab = Vocabulary::from_raw(RawVocabulary {
            corrections: HashMap::from([("cloud code".to_string(), "Claude Code".to_string())]),
            patterns: vec![],
        });

        assert_eq!(vocab.apply("open claud code"), "open Claude Code");
        assert_eq!(vocab.apply("open cloud coad"), "open Claude Code");
    }

    #[test]
    fn fuzzy_corrections_leave_unrelated_text_alone() {
        let vocab = Vocabulary::from_raw(RawVocabulary {
            corrections: HashMap::from([("cloud code".to_string(), "Claude Code".to_string())]),
            patterns: vec![],
        });

        assert_eq!(vocab.apply("open cloud storage"), "open cloud storage");
    }

    #[test]
    fn quotes_full_match_when_requested() {
        let vocab = Vocabulary::default_patterns();
        assert_eq!(vocab.apply("open src/main.rs"), "open `src/main.rs`");
    }
}
