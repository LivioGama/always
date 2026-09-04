//! Pure decision logic for the speech pipeline.
//!
//! Extracted from `event_loop.rs` so the testable, side-effect-free
//! parts of the pipeline live separate from the daemon's orchestration
//! code. Nothing here touches the pasteboard, the network, or the
//! filesystem — everything is a pure function over its inputs, which
//! means every branch can be exercised from a unit test without a
//! daemon, a microphone, or a Groq key.
//!
//! Localization seam: sentence-terminator detection and the
//! "safe to lowercase mid-sentence" word list both live behind
//! [`Localization`]. The default ([`Localization::ENGLISH`]) preserves
//! the historic behavior; a future config option can swap in a
//! language-specific instance without touching callers.
//!
//! Public surface (stable):
//!   - [`SpeechAction`] — classifier output
//!   - [`classify_transcription`] — pure decision tree
//!   - [`merge_dictation`] — append-with-case-fixup for resume merges
//!   - [`Localization`] — i18n seam for the heuristics above

use std::time::{Duration, Instant};

use crate::always::config::AlwaysConfig;
use crate::always::filter;
// Re-export so existing call sites and tests that imported
// `crate::always::speech_action::Localization` keep working without
// churn — the canonical definition now lives in the dedicated
// `crate::always::localization` module so `config.rs` can depend on it.
pub use crate::always::localization::{
    ENGLISH_SAFE_LOWERCASE_STARTERS, ENGLISH_SENTENCE_TERMINATORS, Localization,
};

/// Classification of an incoming transcription. Keeping this an enum
/// instead of the original tangled control flow makes the decision tree
/// testable in isolation — we can feed in any combination of text +
/// transcription metadata and assert the outcome without spawning a
/// daemon, calling Groq, or touching the user's pasteboard.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "a SpeechAction must be inspected; dropping it silently throws away the daemon's decision"]
pub enum SpeechAction {
    /// In cooldown window (anti-double-paste). No-op.
    InCooldown,
    /// Hard or AI filter rejected the text. The reason is the human-readable
    /// label emitted on the UDS stream so the GUI can display it.
    Rejected { reason: String },
    /// Whisper hallucination detector rejected the text.
    Hallucinated { reason: String },
    /// Accepted — daemon should copy + paste this final text.
    Paste { text: String },
}

/// Minimum duplicate-paste suppression window — independent of `cooldown_ms`
/// so a low cooldown setting cannot weaken back-to-back dedupe.
pub const PASTE_DEDUPE_MIN_WINDOW_MS: u64 = 3000;

/// Levenshtein similarity threshold for near-duplicate suppression (0–1).
pub const PASTE_NEAR_DUPLICATE_SIMILARITY: f64 = 0.92;

/// Effective dedupe window: at least [`PASTE_DEDUPE_MIN_WINDOW_MS`].
#[must_use]
pub fn paste_dedupe_window(cooldown_ms: u32) -> Duration {
    Duration::from_millis(cooldown_ms.max(PASTE_DEDUPE_MIN_WINDOW_MS as u32) as u64)
}

/// Normalize paste text for duplicate comparison only (not for display).
#[must_use]
pub fn normalize_for_paste_dedupe(text: &str) -> String {
    let trimmed = text.trim();
    let mut out = String::with_capacity(trimmed.len());
    let mut prev_space = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
            }
            prev_space = true;
        } else {
            prev_space = false;
            out.push(ch.to_ascii_lowercase());
        }
    }
    while out.ends_with(|c: char| c.is_ascii_punctuation() || c.is_whitespace()) {
        out.pop();
    }
    out
}

/// Similarity ratio in `[0.0, 1.0]` after normalization.
#[must_use]
pub fn paste_similarity(a: &str, b: &str) -> f64 {
    let na = normalize_for_paste_dedupe(a);
    let nb = normalize_for_paste_dedupe(b);
    if na.is_empty() && nb.is_empty() {
        return 1.0;
    }
    if na == nb {
        return 1.0;
    }
    let max_len = na.len().max(nb.len());
    if max_len == 0 {
        return 1.0;
    }
    let dist = strsim::levenshtein(&na, &nb) as f64;
    1.0 - dist / max_len as f64
}

/// True when `candidate` is identical or near-identical to `recent`.
#[must_use]
pub fn is_near_duplicate_paste(candidate: &str, recent: &str) -> bool {
    if candidate.trim() == recent.trim() {
        return true;
    }
    paste_similarity(candidate, recent) >= PASTE_NEAR_DUPLICATE_SIMILARITY
}

/// True when `now` is still inside the post-paste cooldown window
/// measured from `last_process`. Pure arithmetic — extracted so the
/// classifier can be tested without a real clock.
///
/// # Examples
///
/// ```
/// use std::time::{Duration, Instant};
/// use always::always::speech_action::in_cooldown;
///
/// let now = Instant::now();
/// assert!(in_cooldown(now, now - Duration::from_millis(500), 1500));
/// assert!(!in_cooldown(now, now - Duration::from_millis(2000), 1500));
/// ```
#[must_use]
pub fn in_cooldown(now: Instant, last_process: Instant, cooldown_ms: u32) -> bool {
    now.duration_since(last_process).as_millis() < cooldown_ms as u128
}

/// Pure decision function — no I/O, no globals. Takes the raw
/// transcription and returns what the daemon should do next.
///
/// Referentially transparent: same `(cfg, text, transcription, now,
/// last_process)` always yields the same `SpeechAction`. This is the
/// property that makes the decision tree fully unit-testable.
pub fn classify_transcription(
    cfg: &AlwaysConfig,
    text: &str,
    transcription: &crate::stt::TranscriptionResult,
    now: Instant,
    last_process: Instant,
) -> SpeechAction {
    if in_cooldown(now, last_process, cfg.cooldown_ms) {
        return SpeechAction::InCooldown;
    }

    // Deterministic CONTENT filtering (hard phrase filter + hallucination
    // heuristics) is REMOTE-model cleanup only — see
    // `AlwaysConfig::content_filtering_enabled`. On local models the system
    // never judges content: it inserts raw verbatim what was said. We keep
    // only an empty-guard below (nothing was said → nothing to paste), which
    // is not a content judgment.
    if cfg.content_filtering_enabled() {
        let filter_result = filter::should_accept_with_reason(text, cfg);
        if !matches!(filter_result, filter::FilterReason::None) {
            return SpeechAction::Rejected {
                reason: filter_result.to_log_string(),
            };
        }

        if let Some(reason) = crate::always::hallucination::is_hallucination(transcription) {
            return SpeechAction::Hallucinated {
                reason: reason.to_string(),
            };
        }
    }

    // Empty / whitespace-only → nothing to insert. Not a content judgment;
    // there is simply no speech to paste. Everything with real text passes.
    if text.trim().is_empty() {
        return SpeechAction::Rejected {
            reason: "empty".to_string(),
        };
    }

    SpeechAction::Paste {
        text: text.to_string(),
    }
}

/// Append `addition` to `previous` for the dictation-merge case,
/// using the default English localization.
///
/// Rules:
///   - If `previous` already ends with a sentence terminator
///     (`. ! ?` by default — see [`Localization`]), the addition
///     starts a new sentence — keep its capitalization.
///   - Else, attempt to lowercase the first word IF and ONLY IF it's
///     a known "safe" sentence starter (see
///     [`ENGLISH_SAFE_LOWERCASE_STARTERS`]). This biases toward
///     preserving proper nouns when in doubt.
///   - Always insert a single space between the two pieces unless
///     `previous` already ends with whitespace.
///
/// Returns `(joined_full_text, delta_to_paste)`. The delta is what
/// should be pasted at the cursor; the joined text is what becomes
/// the new dictation buffer.
///
/// # Invariant
///
/// `joined == previous + delta` always holds — see the `merge_delta_concat_equals_joined`
/// test for the formal check.
///
/// # Examples
///
/// ```
/// use always::always::speech_action::merge_dictation;
///
/// // Mid-sentence: "And" gets lowercased.
/// let (joined, delta) = merge_dictation("I went to the store", "And bought milk");
/// assert_eq!(joined, "I went to the store and bought milk");
/// assert_eq!(delta, " and bought milk");
///
/// // After sentence terminator: capitalization preserved.
/// let (joined, _) = merge_dictation("Done.", "And now we continue");
/// assert_eq!(joined, "Done. And now we continue");
/// ```
#[must_use = "the returned delta is what gets pasted at the cursor; dropping it loses the user's continuation"]
pub fn merge_dictation(previous: &str, addition: &str) -> (String, String) {
    merge_dictation_with(&Localization::ENGLISH, previous, addition)
}

/// Localization-aware variant of [`merge_dictation`]. Tests, and
/// non-English callers, can construct a [`Localization`] and pass it
/// explicitly without changing the public default behavior.
///
/// # Examples
///
/// ```
/// use always::always::localization::Localization;
/// use always::always::speech_action::merge_dictation_with;
///
/// // Hindi-style locale: Devanagari danda terminates a sentence,
/// // and no starters are "safe to lowercase".
/// const HI: Localization = Localization {
///     safe_lowercase_starters: &[],
///     sentence_terminators: &['।', '?', '!'],
/// };
///
/// // After danda: capitalization preserved.
/// let (joined, _) = merge_dictation_with(&HI, "नमस्ते।", "Aaj ka din");
/// assert_eq!(joined, "नमस्ते। Aaj ka din");
/// ```
#[must_use = "the returned delta is what gets pasted at the cursor; dropping it loses the user's continuation"]
pub fn merge_dictation_with(
    loc: &Localization,
    previous: &str,
    addition: &str,
) -> (String, String) {
    let addition = addition.trim_start();
    if addition.is_empty() {
        return (previous.to_string(), String::new());
    }

    let previous_trimmed = previous.trim_end();
    let ends_sentence = previous_trimmed
        .chars()
        .last()
        .map(|c| loc.sentence_terminators.contains(&c))
        .unwrap_or(false);

    let adjusted = if ends_sentence {
        addition.to_string()
    } else {
        lowercase_if_safe_starter_with(loc, addition)
    };

    let needs_space = !previous.ends_with(char::is_whitespace) && !previous.is_empty();
    let delta = if needs_space {
        format!(" {}", adjusted)
    } else {
        adjusted.clone()
    };
    let joined = format!("{}{}", previous, delta);
    (joined, delta)
}

/// English-default convenience wrapper.
///
/// # Examples
///
/// ```
/// use always::always::speech_action::lowercase_if_safe_starter;
///
/// // "And" is in the safe-starter list → lowercased.
/// assert_eq!(lowercase_if_safe_starter("And then we left"), "and then we left");
///
/// // "Kubernetes" is not → preserved (proper-noun bias).
/// assert_eq!(lowercase_if_safe_starter("Kubernetes runs"), "Kubernetes runs");
/// ```
#[must_use]
pub fn lowercase_if_safe_starter(text: &str) -> String {
    lowercase_if_safe_starter_with(&Localization::ENGLISH, text)
}

/// Localization-aware first-word lowercaser. Lowercases the first
/// whitespace-delimited token IF (and only if) — after stripping
/// trailing non-alphanumerics for the lookup — it appears in
/// `loc.safe_lowercase_starters`. Otherwise the input is returned
/// unchanged.
#[must_use]
pub fn lowercase_if_safe_starter_with(loc: &Localization, text: &str) -> String {
    let trimmed = text.trim_start();
    let first_word_end = trimmed
        .find(|c: char| c.is_whitespace())
        .unwrap_or(trimmed.len());
    let first_word = &trimmed[..first_word_end];
    let rest = &trimmed[first_word_end..];

    // Strip trailing punctuation for lookup so "And," still matches "And".
    let lookup = first_word.trim_end_matches(|c: char| !c.is_alphanumeric());
    if loc.safe_lowercase_starters.contains(&lookup) {
        let mut chars = first_word.chars();
        if let Some(first_char) = chars.next() {
            let lowered = first_char.to_lowercase().collect::<String>();
            return format!("{}{}{}", lowered, chars.as_str(), rest);
        }
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::{
        Localization, PASTE_DEDUPE_MIN_WINDOW_MS, SpeechAction, classify_transcription,
        in_cooldown, is_near_duplicate_paste, lowercase_if_safe_starter, merge_dictation,
        merge_dictation_with, normalize_for_paste_dedupe, paste_dedupe_window,
    };
    use crate::always::AlwaysConfig;
    use crate::stt::TranscriptionResult;

    fn test_config() -> AlwaysConfig {
        use crate::always::config::{PostprocessConfig, VocabConfig};

        AlwaysConfig {
            lang: "en".to_string(),
            timeout_secs: 30,
            silence_secs: 2.0,
            adaptive_silence_enabled: true,
            speaker_gate_enabled: false,
            speaker_gate_threshold: crate::always::config::DEFAULT_SPEAKER_GATE_THRESHOLD,
            auto_enter: false,
            filter_enabled: true,
            energy_threshold: 0.05,
            hear_energy_threshold: 0.01,
            onset_ms: 50,
            cooldown_ms: 1500,
            log_path: PathBuf::from("always.log"),
            post_processor: None,
            project_root: None,
            learning_enabled: false,
            auto_learn_corrections: true,
            groq_stt_api_key: Some("test-key".to_string()),
            transcriber_backend: crate::stt_dispatch::TranscriberBackendChoice::Groq,
            vad_mode: crate::always::config::VadMode::Local,
            silero_threshold: 0.5,
            vocab_config: VocabConfig::default(),
            postprocess_config: PostprocessConfig::default(),
            auto_enter_delay_ms: 0,
            idle_pause_secs: 0,
            localization: Localization::ENGLISH,
            transcript_stream_enabled: false,
            audible_status_sound: crate::always::status_sound::StatusSoundSetting::default(),
            stt_live_preview: true,
        }
    }

    fn empty_transcription(text: &str) -> TranscriptionResult {
        TranscriptionResult {
            text: text.to_string(),
            duration: 1.0,
            language: "en".to_string(),
            segments: vec![],
        }
    }

    #[test]
    fn cooldown_uses_millisecond_window() {
        let now = Instant::now();
        assert!(in_cooldown(now, now - Duration::from_millis(1499), 1500));
        assert!(!in_cooldown(now, now - Duration::from_millis(1500), 1500));
    }

    // ----------------------------------------------------------------------
    // classify_transcription — pure decision tree.
    // ----------------------------------------------------------------------

    #[test]
    fn classify_in_cooldown_returns_no_op() {
        let cfg = test_config();
        let now = Instant::now();
        let action = classify_transcription(
            &cfg,
            "open file",
            &empty_transcription("open file"),
            now,
            now,
        );
        assert_eq!(action, SpeechAction::InCooldown);
    }

    #[test]
    fn classify_outside_cooldown_pastes_clean_text() {
        let cfg = test_config();
        let now = Instant::now();
        let action = classify_transcription(
            &cfg,
            "open file",
            &empty_transcription("open file"),
            now,
            now - Duration::from_secs(10),
        );
        match action {
            SpeechAction::Paste { text } => assert_eq!(text, "open file"),
            other => panic!("expected Paste, got {:?}", other),
        }
    }

    #[test]
    fn classify_does_not_rewrite_text_pre_llm() {
        let cfg = test_config();
        let now = Instant::now();
        let action = classify_transcription(
            &cfg,
            "open src/main.rs",
            &empty_transcription("open src/main.rs"),
            now,
            now - Duration::from_secs(10),
        );
        match action {
            SpeechAction::Paste { text } => assert_eq!(text, "open src/main.rs"),
            other => panic!("expected Paste, got {:?}", other),
        }
    }

    #[test]
    fn classify_rejects_filler_phrases() {
        let cfg = test_config();
        let now = Instant::now();
        let action = classify_transcription(
            &cfg,
            "thanks for watching",
            &empty_transcription("thanks for watching"),
            now,
            now - Duration::from_secs(10),
        );
        match action {
            SpeechAction::Rejected { reason } => {
                assert!(!reason.is_empty(), "rejection should carry a reason");
            }
            other => panic!("expected Rejected, got {:?}", other),
        }
    }

    // ----------------------------------------------------------------------
    // Local backend = RAW verbatim. No deterministic content judgment.
    // Regression guard for the lost 5-minute dictation: a stutter run
    // ("o o o o", "uh uh uh uh") inside otherwise-valid long speech was
    // classified as a hallucination and the ENTIRE utterance discarded.
    // ----------------------------------------------------------------------

    fn local_config() -> AlwaysConfig {
        let mut cfg = test_config();
        cfg.transcriber_backend = crate::stt_dispatch::TranscriberBackendChoice::Local {
            model_id: "parakeet-tdt-0.6b-v2".to_string(),
        };
        cfg
    }

    /// The actual text that was lost (representative slice with the two
    /// repeated runs that tripped `hallucination::is_hallucination`). On the
    /// local backend it MUST pass through verbatim.
    const LOST_STUTTER_TEXT: &str = "So, I want a table exactly what happens when I start \
        speaking and all, so we can o o o o o o o o o optimize uh uh uh uh uh uh uh uh exactly \
        what happens from when I start speaking through always to when text gets inserted and \
        what takes how much time, so we can like optimize the process as much as we can";

    #[test]
    fn classify_local_backend_passes_stutter_verbatim() {
        let cfg = local_config();
        let now = Instant::now();
        let action = classify_transcription(
            &cfg,
            LOST_STUTTER_TEXT,
            &empty_transcription(LOST_STUTTER_TEXT),
            now,
            now - Duration::from_secs(10),
        );
        match action {
            SpeechAction::Paste { text } => assert_eq!(
                text, LOST_STUTTER_TEXT,
                "local backend must insert raw verbatim — no content filtering"
            ),
            other => panic!("expected verbatim Paste on local, got {:?}", other),
        }
    }

    #[test]
    fn classify_groq_backend_still_filters_stutter() {
        // The Groq path keeps the heuristic pre-pass (an LLM does the real
        // cleanup there). Same input is still caught — proving we only
        // changed the LOCAL behavior, not Groq's.
        let cfg = test_config(); // Groq
        let now = Instant::now();
        let action = classify_transcription(
            &cfg,
            LOST_STUTTER_TEXT,
            &empty_transcription(LOST_STUTTER_TEXT),
            now,
            now - Duration::from_secs(10),
        );
        assert!(
            matches!(action, SpeechAction::Hallucinated { .. }),
            "groq path should still run the heuristic pre-pass, got {:?}",
            action
        );
    }

    #[test]
    fn classify_local_backend_pastes_single_short_word() {
        // "no too short" — the user can say just "hi" and it must insert.
        let cfg = local_config();
        let now = Instant::now();
        for word in ["hi", "yes", "ok", "no"] {
            let action = classify_transcription(
                &cfg,
                word,
                &empty_transcription(word),
                now,
                now - Duration::from_secs(10),
            );
            match action {
                SpeechAction::Paste { text } => assert_eq!(text, word),
                other => panic!("short word `{word}` must paste on local, got {:?}", other),
            }
        }
    }

    #[test]
    fn classify_local_backend_rejects_only_empty() {
        let cfg = local_config();
        let now = Instant::now();
        let action = classify_transcription(
            &cfg,
            "   ",
            &empty_transcription("   "),
            now,
            now - Duration::from_secs(10),
        );
        assert!(
            matches!(action, SpeechAction::Rejected { .. }),
            "whitespace-only has nothing to paste, got {:?}",
            action
        );
    }

    // ----------------------------------------------------------------------
    // merge_dictation — resume after a pause appends to the in-flight
    // dictation buffer instead of pasting a fresh capitalized sentence.
    // ----------------------------------------------------------------------

    #[test]
    fn merge_lowercases_safe_starter_mid_sentence() {
        let (joined, delta) = merge_dictation("I went to the store", "And bought milk");
        assert_eq!(joined, "I went to the store and bought milk");
        assert_eq!(delta, " and bought milk");
    }

    #[test]
    fn merge_preserves_capitalization_after_sentence_terminator() {
        let (joined, delta) = merge_dictation("Done.", "And now we continue");
        assert_eq!(joined, "Done. And now we continue");
        assert_eq!(delta, " And now we continue");
    }

    #[test]
    fn merge_preserves_proper_noun_at_start_of_continuation() {
        let (joined, _) = merge_dictation("I work on", "Kubernetes clusters");
        assert_eq!(joined, "I work on Kubernetes clusters");
    }

    #[test]
    fn merge_keeps_i_capitalized_mid_sentence() {
        let (joined, _) = merge_dictation("Then", "I went home");
        assert_eq!(joined, "Then I went home");
    }

    #[test]
    fn merge_handles_safe_starter_with_trailing_punctuation() {
        let (joined, _) = merge_dictation("Let's go", "And, well, maybe later");
        assert_eq!(joined, "Let's go and, well, maybe later");
    }

    #[test]
    fn merge_does_not_double_space_when_previous_ends_in_space() {
        let (joined, delta) = merge_dictation("Hello ", "And then");
        assert_eq!(joined, "Hello and then");
        assert_eq!(delta, "and then");
    }

    #[test]
    fn merge_with_empty_addition_is_noop() {
        let (joined, delta) = merge_dictation("Existing text", "");
        assert_eq!(joined, "Existing text");
        assert_eq!(delta, "");
    }

    #[test]
    fn merge_after_question_mark_keeps_capital() {
        let (joined, _) = merge_dictation("Ready?", "Then let's start");
        assert_eq!(joined, "Ready? Then let's start");
    }

    #[test]
    fn merge_after_exclamation_keeps_capital() {
        let (joined, _) = merge_dictation("Wow!", "That was fast");
        assert_eq!(joined, "Wow! That was fast");
    }

    #[test]
    fn lowercase_safe_starter_leaves_acronyms_alone() {
        let out = lowercase_if_safe_starter("API request was made");
        assert_eq!(out, "API request was made");
    }

    #[test]
    fn lowercase_safe_starter_only_touches_first_word() {
        let out = lowercase_if_safe_starter("The Cake is ready");
        assert_eq!(out, "the Cake is ready");
    }

    // ----------------------------------------------------------------------
    // Localization seam — a non-English locale changes behavior without
    // touching call sites.
    // ----------------------------------------------------------------------

    /// Synthetic Hindi-ish locale: Devanagari danda ends a sentence;
    /// no starters are "safe to lowercase". Confirms the seam — the
    /// default English path is unchanged; an override changes behavior.
    const HINDI_ISH: Localization = Localization {
        safe_lowercase_starters: &[],
        sentence_terminators: &['।', '?', '!'],
    };

    #[test]
    fn localization_seam_respects_custom_terminator() {
        // Without override: ASCII period does NOT match `।`, so the
        // baseline still treats the prior text as mid-sentence — but
        // since starters is empty, nothing is lowercased.
        let (joined_ascii, _) = merge_dictation_with(&HINDI_ISH, "नमस्ते.", "Aaj ka din");
        // Period is NOT a Hindi terminator here, so mid-sentence logic
        // applies — but the empty starter set means nothing is lowered.
        assert_eq!(joined_ascii, "नमस्ते. Aaj ka din");

        // Danda IS a terminator: capitalization preserved.
        let (joined_danda, _) = merge_dictation_with(&HINDI_ISH, "नमस्ते।", "Aaj ka din");
        assert_eq!(joined_danda, "नमस्ते। Aaj ka din");
    }

    #[test]
    fn localization_seam_respects_empty_starter_list() {
        // English baseline would lowercase "And"; with the empty
        // starter set, the seam preserves it.
        let (joined, _) = merge_dictation_with(&HINDI_ISH, "Hello", "And bye");
        assert_eq!(joined, "Hello And bye");
    }

    // ----------------------------------------------------------------------
    // Invariants. Property-style: hold over a sweep of inputs rather
    // than a single enumerated case.
    // ----------------------------------------------------------------------

    /// Core merge invariant: the returned `(joined, delta)` always
    /// satisfies `joined == previous + delta`. Without this, the
    /// dictation buffer and the pasted text would drift out of sync
    /// and resume-merge would corrupt the user's text on every
    /// utterance.
    #[test]
    fn merge_delta_concat_equals_joined() {
        let cases: &[(&str, &str)] = &[
            ("", ""),
            ("", "hello"),
            ("hello", ""),
            ("Hello", "And then"),
            ("Hello ", "And then"),
            ("Done.", "And then"),
            ("I went to the store", "And bought milk"),
            ("Ready?", "Then let's start"),
            ("Wow!", "That was fast"),
            ("Kubernetes is", "Running pods"),
            ("a", "b"),
            ("नमस्ते", "Aaj"),
        ];
        for (prev, add) in cases {
            let (joined, delta) = merge_dictation(prev, add);
            assert_eq!(
                joined,
                format!("{prev}{delta}"),
                "invariant violated: previous={prev:?} addition={add:?} joined={joined:?} delta={delta:?}"
            );
        }
    }

    /// Empty addition is identity: never mutates `previous`, never
    /// emits a non-empty delta. Critical because the speech pipeline
    /// can deliver "trim-only-whitespace" continuations that the VAD
    /// passes through.
    #[test]
    fn merge_empty_addition_is_identity_invariant() {
        for prev in ["", "hello", "Hello.", "  trailing whitespace  ", "नमस्ते"] {
            let (joined, delta) = merge_dictation(prev, "");
            assert_eq!(joined, prev, "identity broken for previous={prev:?}");
            assert!(
                delta.is_empty(),
                "empty addition must emit empty delta, got {delta:?}"
            );

            // Also: pure-whitespace addition trims to empty and
            // must hit the same identity path.
            let (joined_ws, delta_ws) = merge_dictation(prev, "   ");
            assert_eq!(joined_ws, prev);
            assert!(delta_ws.is_empty());
        }
    }

    /// No-double-space invariant: the boundary between `previous`
    /// and the inserted addition is exactly one space (or zero, if
    /// `previous` already ends with whitespace or is empty). This
    /// keeps merged text visually clean and prevents the older bug
    /// where back-to-back utterances accumulated stray spaces.
    #[test]
    fn merge_never_emits_double_space_at_boundary() {
        let cases: &[(&str, &str)] = &[
            ("Hello", "World"),
            ("Hello ", "World"),
            ("Hello.", "World"),
            ("Hello. ", "World"),
            ("Hello\t", "World"),
        ];
        for (prev, add) in cases {
            let (joined, _) = merge_dictation(prev, add);
            assert!(
                !joined.contains("  "),
                "double space appeared at boundary: previous={prev:?} addition={add:?} joined={joined:?}"
            );
        }
    }

    /// Classifier is referentially transparent: same inputs → same
    /// output. The pure-decision contract makes this trivially true,
    /// but the test guards against accidental introduction of
    /// globals/time/randomness in a future refactor.
    #[test]
    fn classify_is_referentially_transparent() {
        let cfg = test_config();
        let now = Instant::now();
        let last = now - Duration::from_secs(10);
        let tr = empty_transcription("open file");
        let a = classify_transcription(&cfg, "open file", &tr, now, last);
        let b = classify_transcription(&cfg, "open file", &tr, now, last);
        let c = classify_transcription(&cfg, "open file", &tr, now, last);
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn localization_default_matches_english_const() {
        // Locking the default in so future "let's just change the
        // default" PRs trip a test.
        assert_eq!(
            Localization::default().safe_lowercase_starters.len(),
            Localization::ENGLISH.safe_lowercase_starters.len(),
        );
        assert_eq!(
            Localization::default().sentence_terminators,
            Localization::ENGLISH.sentence_terminators,
        );
    }

    // ----------------------------------------------------------------------
    // Paste dedupe helpers
    // ----------------------------------------------------------------------

    #[test]
    fn paste_dedupe_window_is_at_least_three_seconds() {
        assert!(paste_dedupe_window(0).as_millis() >= u128::from(PASTE_DEDUPE_MIN_WINDOW_MS));
        assert!(paste_dedupe_window(150).as_millis() >= u128::from(PASTE_DEDUPE_MIN_WINDOW_MS));
        assert_eq!(
            paste_dedupe_window(5000).as_millis(),
            5000_u128,
            "values above the floor pass through unchanged"
        );
    }

    #[test]
    fn normalize_for_paste_dedupe_collapses_case_and_punctuation() {
        assert_eq!(
            normalize_for_paste_dedupe("Hello world."),
            normalize_for_paste_dedupe("hello world")
        );
    }

    #[test]
    fn is_near_duplicate_paste_catches_punctuation_variants() {
        assert!(is_near_duplicate_paste("Hello world.", "hello world"));
    }

    #[test]
    fn is_near_duplicate_paste_allows_different_sentences() {
        assert!(!is_near_duplicate_paste(
            "open the settings panel",
            "close the settings panel"
        ));
    }
}
