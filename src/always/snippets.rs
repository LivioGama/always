//! Voice snippets: trigger phrases that expand into user-authored text.
//! Saying "grill me" — alone or anywhere inside an utterance — replaces
//! the trigger words with the configured expansion.
//!
//! Snippets are deterministic text substitution — no LLM involved. An
//! utterance containing an expansion bypasses the grammar/correction
//! stack so the expansion is pasted byte-exact.
//!
//! File format (`~/.always/snippets.json`):
//! ```json
//! [
//!   { "trigger": "grill me", "expansion": "Interview me relentlessly…" }
//! ]
//! ```
//!
//! The file is re-read on every lookup. Lookups only happen when an
//! utterance reaches the paste stage (a few times a minute at most), so
//! the cost is negligible — and it means edits apply to the very next
//! utterance without restarting the daemon.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize)]
struct Entry {
    trigger: String,
    expansion: String,
}

/// Canonical user-facing location for snippets.json, next to
/// glossary.json.
pub fn user_snippets_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".always").join("snippets.json"))
}

fn locate_snippets() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ALWAYS_SNIPPETS_PATH") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    user_snippets_path().filter(|p| p.exists())
}

/// A word in the original transcript: its lowercase alphanumeric form
/// plus the byte range it occupies, so replacements can splice the
/// original text without touching surrounding words or spacing.
struct Word {
    norm: String,
    start: usize,
    end: usize,
}

/// Split into alphanumeric words, keeping original byte offsets.
/// Whisper decorates trigger phrases unpredictably ("Grill me.",
/// "grill me!", "Grill, me") — matching happens on the normalized
/// words, splicing on the original bytes.
fn words_with_spans(text: &str) -> Vec<Word> {
    let mut words = Vec::new();
    let mut current: Option<(usize, String)> = None;
    for (i, c) in text.char_indices() {
        if c.is_alphanumeric() {
            match &mut current {
                Some((_, buf)) => buf.extend(c.to_lowercase()),
                None => {
                    let mut buf = String::new();
                    buf.extend(c.to_lowercase());
                    current = Some((i, buf));
                }
            }
        } else if let Some((start, buf)) = current.take() {
            words.push(Word {
                norm: buf,
                start,
                end: i,
            });
        }
    }
    if let Some((start, buf)) = current.take() {
        words.push(Word {
            norm: buf,
            start,
            end: text.len(),
        });
    }
    words
}

fn trigger_tokens(trigger: &str) -> Vec<String> {
    trigger
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Replace every trigger occurrence in `transcript` with its expansion.
/// Triggers match on word boundaries, case- and punctuation-insensitive,
/// anywhere in the utterance. Returns `None` when nothing matched —
/// proceed with the normal pipeline.
pub fn expand(transcript: &str) -> Option<String> {
    let entries = load_entries()?;
    expand_with(transcript, &entries)
}

fn expand_with(transcript: &str, entries: &[Entry]) -> Option<String> {
    let tokenized: Vec<(Vec<String>, &str)> = entries
        .iter()
        .map(|e| (trigger_tokens(&e.trigger), e.expansion.as_str()))
        .filter(|(tokens, _)| !tokens.is_empty())
        .collect();
    if tokenized.is_empty() {
        return None;
    }

    let words = words_with_spans(transcript);
    let mut out = String::new();
    let mut cursor = 0; // byte position in transcript consumed so far
    let mut i = 0; // word index
    let mut matched_any = false;

    while i < words.len() {
        // First entry that matches at this word position wins.
        let hit = tokenized.iter().find(|(tokens, _)| {
            i + tokens.len() <= words.len()
                && tokens
                    .iter()
                    .zip(&words[i..i + tokens.len()])
                    .all(|(t, w)| *t == w.norm)
        });
        match hit {
            Some((tokens, expansion)) => {
                let span_start = words[i].start;
                let mut span_end = words[i + tokens.len() - 1].end;
                // Swallow punctuation glued to the trigger ("grill me,"
                // / "grill me." / "grill me!") — the expansion carries
                // its own punctuation.
                for (j, c) in transcript[span_end..].char_indices() {
                    if c.is_whitespace() || c.is_alphanumeric() {
                        span_end += j;
                        break;
                    }
                    if span_end + j + c.len_utf8() == transcript.len() {
                        span_end = transcript.len();
                        break;
                    }
                }
                out.push_str(&transcript[cursor..span_start]);
                out.push_str(expansion);
                cursor = span_end;
                matched_any = true;
                i += tokens.len();
            }
            None => i += 1,
        }
    }

    if !matched_any {
        return None;
    }
    out.push_str(&transcript[cursor..]);
    Some(out)
}

fn load_entries() -> Option<Vec<Entry>> {
    let path = locate_snippets()?;
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to read snippets.json");
            return None;
        }
    };
    match serde_json::from_str::<Vec<Entry>>(&raw) {
        Ok(entries) => Some(entries),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to parse snippets.json");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(trigger: &str, expansion: &str) -> Entry {
        Entry {
            trigger: trigger.to_string(),
            expansion: expansion.to_string(),
        }
    }

    #[test]
    fn whole_utterance_trigger_expands() {
        let entries = vec![entry("grill me", "Interview me relentlessly.")];
        assert_eq!(
            expand_with("grill me", &entries).as_deref(),
            Some("Interview me relentlessly.")
        );
    }

    #[test]
    fn trigger_matches_despite_case_and_punctuation() {
        let entries = vec![entry("grill me", "EXPANSION")];
        for heard in ["Grill me.", "GRILL ME!", "Grill, me", "  grill   me  "] {
            let got = expand_with(heard, &entries);
            assert!(
                got.as_deref().is_some_and(|s| s.contains("EXPANSION")),
                "should match transcript variant {heard:?}, got {got:?}"
            );
        }
    }

    #[test]
    fn inline_trigger_replaces_in_place() {
        let entries = vec![entry("grill me", "EXPANSION")];
        assert_eq!(
            expand_with("Okay, let's put it to the test now. Grill me.", &entries).as_deref(),
            Some("Okay, let's put it to the test now. EXPANSION")
        );
        assert_eq!(
            expand_with("Please grill me, then summarize.", &entries).as_deref(),
            Some("Please EXPANSION then summarize.")
        );
    }

    #[test]
    fn multiple_occurrences_all_expand() {
        let entries = vec![entry("go", "RUN")];
        assert_eq!(
            expand_with("go and go again", &entries).as_deref(),
            Some("RUN and RUN again")
        );
    }

    #[test]
    fn multiple_entries_expand_independently() {
        let entries = vec![entry("grill me", "GRILL"), entry("commit skill", "COMMIT")];
        assert_eq!(
            expand_with("First grill me. Then commit skill.", &entries).as_deref(),
            Some("First GRILL Then COMMIT")
        );
    }

    #[test]
    fn partial_word_does_not_match() {
        // Word-boundary matching: "grill" inside "grilling" must not fire.
        let entries = vec![entry("grill me", "EXPANSION")];
        assert_eq!(expand_with("I was grilling meat", &entries), None);
        assert_eq!(expand_with("grill", &entries), None);
    }

    #[test]
    fn no_match_returns_none() {
        let entries = vec![entry("grill me", "EXPANSION")];
        assert_eq!(expand_with("nothing to see here", &entries), None);
        assert_eq!(expand_with("", &entries), None);
    }

    #[test]
    fn empty_trigger_never_matches() {
        let entries = vec![entry("   ", "EXPANSION")];
        assert_eq!(expand_with("anything at all", &entries), None);
    }

    #[test]
    fn first_matching_entry_wins_at_same_position() {
        let entries = vec![entry("go", "FIRST"), entry("go", "SECOND")];
        assert_eq!(expand_with("Go!", &entries).as_deref(), Some("FIRST"));
    }

    #[test]
    fn expansion_containing_trigger_is_not_rescanned() {
        let entries = vec![entry("loop", "loop loop")];
        assert_eq!(expand_with("loop", &entries).as_deref(), Some("loop loop"));
    }

    #[test]
    fn missing_file_is_silent() {
        let _ = expand("anything");
    }
}
