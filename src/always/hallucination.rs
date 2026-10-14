//! Hallucination filter for Whisper transcription results.
//!
//! Whisper frequently emits canned phrases ("Thank you", "Bye.", subtitle credits,
//! mixed-script gibberish) when given silence or noise. This module inspects a
//! [`crate::stt::TranscriptionResult`] across several independent layers and rejects
//! results that look like hallucinations rather than real speech.

use std::collections::HashSet;
use std::sync::LazyLock;

use crate::stt::TranscriptionResult;

/// Phrases that Whisper hallucinates when given silence or noise.
/// Matched against the *fully normalized* text (lowercased, trimmed, leading/trailing
/// ASCII punctuation stripped, internal whitespace collapsed).
const HALLUCINATION_PHRASES: &[&str] = &[
    // "Thank you" family — Whisper's most common hallucination
    "thank you",
    "thanks",
    "thank you very much",
    "thanks a lot",
    "thank you for watching",
    "thanks for watching",
    "thank you for listening",
    "thanks for listening",
    "thank you for your attention",
    "thank you so much",
    // YouTube outro
    "please subscribe",
    "like and subscribe",
    "subscribe for more",
    "don't forget to subscribe",
    "subscribe to my channel",
    "see you next time",
    "see you in the next video",
    "see you soon",
    "see you later",
    "have a great day",
    "that's all",
    "that's it",
    "the end",
    "welcome to a new video",
    // Goodbye family
    "bye",
    "bye bye",
    "goodbye",
    "amen",
    "peace",
    // Filler sounds (real noise — hesitation markers Whisper invents
    // from silence or breathing).
    "uh",
    "um",
    "ah",
    "mmm",
    "hmm",
    "uhh",
    "umm",
    "ahh",
    "err",
    "mm hmm",
    "uh huh",
    "mm-hmm",
    "uh-huh",
    "huh",
    "ugh",
    "sigh",
    "boo",
    "blah",
    // Function-word hallucinations — Whisper sometimes emits one of
    // these alone from silence. They're never meaningful as a single
    // word in dictation context.
    "the",
    "and",
    "as",
    // NOTE: explicit affirmatives / negatives / greetings that earlier
    // versions of this list rejected (`yes`, `no`, `yeah`, `yep`,
    // `nope`, `ok`, `okay`, `oh`, `hey`, `hi`, `hello`, `wow`,
    // `oh my god`, `you`, `what`) have been removed. They are valid
    // single-word commands and the user is the authority on whether
    // they meant to say them. Hallucination detection still catches
    // these via the segment-level no_speech_prob + avg_logprob layers
    // when the audio was actually silent.
    // Subtitle/caption credits (also covered by substring list)
    "subtitles by the amara.org community",
    "captions by the amara.org community",
    "transcription by castingwords",
];

/// Substrings whose mere presence (in the lowercased, whitespace-collapsed text)
/// indicates a hallucination. Used in addition to exact-match phrase blocking so
/// we still catch credits embedded inside surrounding chatter.
const HALLUCINATION_SUBSTRINGS: &[&str] = &[
    "subtitles by",
    "captions by",
    "subtitle by",
    "caption by",
    "closed captioning by",
    "transcription by",
    "transcription provided by",
    "amara.org",
    "rev.com",
    "otter.ai",
    "youtube transcription",
    "auto-generated subtitles",
    "automatic captions",
    "please subscribe",
    "like and subscribe",
    "subscribe for more",
    "thanks for watching",
    "thank you for watching",
    "thanks for listening",
    "thank you for listening",
    "see you in the next video",
    "see you next time",
];

static PHRASE_SET: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| HALLUCINATION_PHRASES.iter().copied().collect());

/// Short standalone fragments that are almost never useful pasted text in
/// dictation mode, but commonly appear when STT tries to decode breath,
/// room noise, or a clipped syllable.
const CONTEXTLESS_SHORT_PHRASES: &[&str] = &[
    "oh", "you", "but", "so", "well", "bleh", "meh", "heh", "ha", "haha",
];

static CONTEXTLESS_SHORT_SET: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| CONTEXTLESS_SHORT_PHRASES.iter().copied().collect());

/// Lowercase, trim, strip leading/trailing ASCII punctuation, and collapse runs
/// of whitespace down to single spaces.
fn normalize(text: &str) -> String {
    let lower = text.to_lowercase();
    let trimmed = lower.trim();
    let stripped = trimmed.trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace());
    // Collapse internal whitespace.
    let mut out = String::with_capacity(stripped.len());
    let mut prev_space = false;
    for ch in stripped.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out
}

/// Mean per-word acoustic confidence below which a transcript is treated as
/// speech in a language other than the recognizer's (Layer H).
///
/// Measured 2026-10-14 with Apple SpeechTranscriber (en_GB), mean of
/// `.transcriptionConfidence` over the words:
/// - the user's own saved English dictation audio (~/.always/failed-chunks,
///   60+ clips): 0.59-0.96, typically 0.85+;
/// - the user's saved Nepali speech ("Whisperway, Ma, Nepali, Ma, Boli…"):
///   0.095; other non-English/garbled clips 0.05-0.27;
/// - synthetic Hindi/Nepali (macOS `say -v Lekha`): 0.15-0.38;
///   synthetic English: 0.86-0.95.
pub const FOREIGN_SPEECH_MAX_CONFIDENCE: f32 = 0.40;

/// When the live recognizer heard no words but the file recognizer scored
/// its own transcript at least this confident, trust the transcript — a
/// live-session glitch must not cost genuine dictation. Every non-English
/// clip measured scored below 0.58.
pub const FOREIGN_SPEECH_TRUST_CONFIDENCE: f32 = 0.70;

/// Share of the final transcript's words the English-only Parakeet preview
/// must also have heard for the two engines to count as agreeing.
pub const FOREIGN_SPEECH_PREVIEW_AGREEMENT: f64 = 0.5;

fn agreement_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '\'')
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Fraction of `text`'s words that `preview` also contains (0 when either
/// is empty).
#[must_use]
pub fn preview_agreement(text: &str, preview: &str) -> f64 {
    let words = agreement_words(text);
    if words.is_empty() {
        return 0.0;
    }
    let heard: HashSet<String> = agreement_words(preview).into_iter().collect();
    if heard.is_empty() {
        return 0.0;
    }
    words.iter().filter(|w| heard.contains(*w)).count() as f64 / words.len() as f64
}

/// Layer H — speech in another language (e.g. the user talking Nepali to
/// family) that the recognizer forced into plausible English words
/// ("And a baggy?"). Judged from ACOUSTIC evidence the engines already
/// computed, never from the words themselves, so it is not a content
/// filter and runs on every backend that reports evidence (backends that
/// report none get `None` — no signal, no change):
///
/// 1. the file recognizer's mean word confidence is below
///    [`FOREIGN_SPEECH_MAX_CONFIDENCE`]; or
/// 2. the live Apple session ran over the same audio and finished
///    cleanly with NO words, unless the file recognizer was confident
///    (≥ [`FOREIGN_SPEECH_TRUST_CONFIDENCE`]) or the Parakeet preview heard
///    the same words (≥ [`FOREIGN_SPEECH_PREVIEW_AGREEMENT`]).
///
/// Pure, O(words). Returns the reject reason, or `None` to accept.
///
/// Rollout (2026-10-14): rule 2 is enforced; rule 1 is enforced only with
/// `enforce_confidence` (pref `foreign_speech_confidence_enforce`, default
/// off). When off, the daemon logs [`foreign_speech_low_confidence`] as
/// `foreign_speech_would_reject` and still pastes.
#[must_use]
pub fn foreign_speech(
    result: &TranscriptionResult,
    enforce_confidence: bool,
) -> Option<&'static str> {
    use crate::stt::LiveStreamOutcome;
    let text = result.text.trim();
    if text.is_empty() {
        return None;
    }
    let ev = &result.evidence;
    if enforce_confidence && foreign_speech_low_confidence(result).is_some() {
        return Some("foreign speech: low acoustic confidence");
    }
    if ev.live_stream == LiveStreamOutcome::NoWords {
        let confident = ev
            .confidence
            .is_some_and(|c| c >= FOREIGN_SPEECH_TRUST_CONFIDENCE);
        let preview_agrees = ev
            .preview_text
            .as_deref()
            .is_some_and(|p| preview_agreement(text, p) >= FOREIGN_SPEECH_PREVIEW_AGREEMENT);
        if !confident && !preview_agrees {
            return Some("foreign speech: live recognizer heard no words");
        }
    }
    None
}

/// Rule 1 alone: the mean word confidence when it is below
/// [`FOREIGN_SPEECH_MAX_CONFIDENCE`] (non-empty text only), else `None`.
#[must_use]
pub fn foreign_speech_low_confidence(result: &TranscriptionResult) -> Option<f32> {
    if result.text.trim().is_empty() {
        return None;
    }
    result
        .evidence
        .confidence
        .filter(|c| *c < FOREIGN_SPEECH_MAX_CONFIDENCE)
}

/// Inspect a Whisper result and decide whether to reject it as a hallucination.
///
/// Returns `Some(reason)` if the result should be REJECTED; `None` if it should
/// be accepted. The reason is a stable static string suitable for logging.
pub fn is_hallucination(result: &TranscriptionResult) -> Option<&'static str> {
    let text = result.text.trim();

    // ---- Layer A: empty or near-empty ----
    if text.is_empty() {
        return Some("empty");
    }
    if text.chars().count() < 3 {
        return Some("too short");
    }
    let stripped: String = text
        .chars()
        .filter(|c| !c.is_ascii_punctuation() && !c.is_whitespace())
        .collect();
    if stripped.is_empty() {
        return Some("punctuation only");
    }

    // ---- Layer B: segment-metric thresholds ----
    if !result.segments.is_empty() {
        for seg in &result.segments {
            let no_speech = seg.no_speech_prob > 0.6;
            let low_conf = seg.avg_logprob < -0.5;
            let very_low_conf = seg.avg_logprob < -1.0;
            let repetitive = seg.compression_ratio > 2.4;
            let fallback = seg.temperature > 0.0;

            if no_speech && low_conf {
                return Some("silence-induced hallucination (no_speech + low confidence)");
            }
            if repetitive {
                return Some("repetitive (compression ratio)");
            }
            if fallback && very_low_conf {
                return Some("decoder fallback with very low confidence");
            }
        }
    }

    // ---- Layer C: length sanity ----
    // Real human fast speech tops around 30-35 chars/sec. Whisper hallucinations
    // typically exceed 50+ cps because the model fills silence with dense filler.
    // 25 was too tight — natural fast speakers tripped it. 40 separates fast
    // speech from clear hallucinations.
    if result.duration > 0.0 {
        let chars_per_sec = text.chars().count() as f64 / result.duration;
        if chars_per_sec > 40.0 {
            return Some("too many chars per second");
        }
        let word_count = text.split_whitespace().count();
        if result.duration < 1.0 && word_count > 5 {
            return Some("too many words for short clip");
        }
    }

    // ---- Layer D: hallucination phrase blocklist ----
    let normalized = normalize(text);
    if !normalized.is_empty() {
        if PHRASE_SET.contains(normalized.as_str()) {
            return Some("hallucination phrase (exact match)");
        }
        if CONTEXTLESS_SHORT_SET.contains(normalized.as_str()) {
            return Some("contextless short utterance");
        }
        // Substring scan over the *lowercased* text (with original punctuation
        // intact so "amara.org" still matches).
        let lower = text.to_lowercase();
        for needle in HALLUCINATION_SUBSTRINGS {
            if lower.contains(needle) {
                return Some("hallucination phrase (substring match)");
            }
        }
    }

    // ---- Layer E: n-gram repetition ----
    // Strip per-token punctuation so "Bye." and "Bye" collapse to the same token.
    let tokens: Vec<String> = normalized
        .split_whitespace()
        .map(|s| {
            s.trim_matches(|c: char| c.is_ascii_punctuation())
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect();
    let total_tokens = tokens.len();

    // All tokens identical and at least 2 of them ("Bye. Bye. Bye." -> ["bye","bye","bye"]).
    if total_tokens >= 2 && tokens.iter().all(|t| t == &tokens[0]) {
        return Some("repeated word");
    }

    if total_tokens >= 4 {
        // Same word 4+ consecutive times.
        let mut run = 1usize;
        for i in 1..tokens.len() {
            if tokens[i] == tokens[i - 1] {
                run += 1;
                if run >= 4 {
                    return Some("repeated word");
                }
            } else {
                run = 1;
            }
        }
    }

    if total_tokens >= 6 {
        // Same 2-gram or 3-gram 3+ consecutive times.
        for n in [2usize, 3] {
            if tokens.len() < n * 3 {
                continue;
            }
            let mut i = 0;
            while i + n <= tokens.len() {
                let gram = &tokens[i..i + n];
                let mut reps = 1usize;
                let mut j = i + n;
                while j + n <= tokens.len() && &tokens[j..j + n] == gram {
                    reps += 1;
                    j += n;
                }
                if reps >= 3 {
                    return Some("repeated phrase");
                }
                i += 1;
            }
        }

        // Unique-token ratio.
        let unique: HashSet<&str> = tokens.iter().map(|s| s.as_str()).collect();
        let ratio = unique.len() as f64 / total_tokens as f64;
        if ratio < 0.3 {
            return Some("low vocabulary diversity");
        }
    }

    // ---- Layer F: mixed-script / unicode gibberish ----
    let total_chars = text.chars().count();
    if total_chars > 0 && total_chars < 80 {
        let non_ascii = text
            .chars()
            .filter(|c| !c.is_ascii() && !c.is_whitespace())
            .count();
        if (non_ascii as f64 / total_chars as f64) > 0.3 {
            return Some("mixed-script gibberish");
        }
        // Per-word script mixing: any single word that mixes ASCII letters
        // with non-ASCII letters is almost always Whisper hallucinated noise
        // (e.g. "ünkLöß", "expenseÁê•").
        for word in text.split_whitespace() {
            let has_ascii_alpha = word.chars().any(|c| c.is_ascii_alphabetic());
            let has_non_ascii_alpha = word.chars().any(|c| !c.is_ascii() && c.is_alphabetic());
            if has_ascii_alpha && has_non_ascii_alpha {
                return Some("mixed-script word");
            }
        }
    }
    if text.contains('\u{FFFD}') {
        return Some("unicode replacement char");
    }
    let chars: Vec<char> = text.chars().flat_map(|c| c.to_lowercase()).collect();
    if chars
        .windows(4)
        .any(|w| w[0] == w[1] && w[1] == w[2] && w[2] == w[3] && w[0].is_alphabetic())
    {
        return Some("repeated char run");
    }

    // ---- Layer G: suspicious single-token "domain" ----
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() == 1 && text.contains('.') {
        let has_non_ascii = !text.is_ascii();
        let has_latin = text.chars().any(|c| c.is_ascii_alphabetic());
        if has_non_ascii || !has_latin {
            return Some("malformed domain");
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::TranscriptionResult;

    fn make(text: &str) -> TranscriptionResult {
        TranscriptionResult {
            text: text.to_string(),
            duration: 0.0,
            language: "en".to_string(),
            segments: Vec::new(),
            ..Default::default()
        }
    }

    fn make_with_duration(text: &str, duration: f64) -> TranscriptionResult {
        TranscriptionResult {
            text: text.to_string(),
            duration,
            language: "en".to_string(),
            segments: Vec::new(),
            ..Default::default()
        }
    }

    #[test]
    fn rejects_empty() {
        assert!(is_hallucination(&make("")).is_some());
        assert!(is_hallucination(&make("   ")).is_some());
    }

    #[test]
    fn rejects_too_short() {
        assert!(is_hallucination(&make("a")).is_some());
        assert!(is_hallucination(&make("ok")).is_some());
    }

    #[test]
    fn rejects_punctuation_only() {
        assert!(is_hallucination(&make("...")).is_some());
        assert!(is_hallucination(&make(". . .")).is_some());
        assert!(is_hallucination(&make("!?!?")).is_some());
    }

    #[test]
    fn rejects_thank_you_family() {
        assert!(is_hallucination(&make("Thank you")).is_some());
        assert!(is_hallucination(&make("Thank you.")).is_some());
        assert!(is_hallucination(&make("  Thank you!  ")).is_some());
        assert!(is_hallucination(&make("Thank you for watching")).is_some());
        assert!(is_hallucination(&make("Thanks for watching!")).is_some());
        assert!(is_hallucination(&make("thank you so much")).is_some());
    }

    #[test]
    fn rejects_bye_family() {
        assert!(is_hallucination(&make("Bye.")).is_some());
        assert!(is_hallucination(&make("Bye bye")).is_some());
        assert!(is_hallucination(&make("Goodbye")).is_some());
    }

    #[test]
    fn rejects_repeated_word() {
        // "Bye. Bye. Bye." normalizes to "bye. bye. bye." -> tokens ["bye.","bye.","bye."]
        // We rely on the exact "bye" match for the simple case; a clearer repetition test:
        assert!(is_hallucination(&make("Zed2, Zed2, Zed2, Zed2")).is_some());
        assert!(is_hallucination(&make("yeah yeah yeah yeah yeah")).is_some());
    }

    #[test]
    fn rejects_bye_bye_bye() {
        assert!(is_hallucination(&make("Bye. Bye. Bye.")).is_some());
    }

    #[test]
    fn rejects_subtitle_credits() {
        assert!(is_hallucination(&make("Subtitles by the Amara.org community")).is_some());
        assert!(is_hallucination(&make("Captions by the Amara.org community")).is_some());
        assert!(is_hallucination(&make("Transcription by CastingWords")).is_some());
        assert!(is_hallucination(&make("hello world subtitles by amara.org")).is_some());
    }

    #[test]
    fn rejects_subscribe_phrases() {
        assert!(is_hallucination(&make("Please subscribe and like the video")).is_some());
        assert!(is_hallucination(&make("Like and subscribe!")).is_some());
    }

    #[test]
    fn rejects_mixed_script_gibberish() {
        assert!(is_hallucination(&make("ünkLöß I Nativuchy Dk")).is_some());
    }

    #[test]
    fn rejects_unicode_replacement() {
        assert!(is_hallucination(&make("hello \u{FFFD} world test")).is_some());
    }

    #[test]
    fn rejects_repeated_char_run() {
        assert!(is_hallucination(&make("Ffff")).is_some());
        assert!(is_hallucination(&make("FFFF")).is_some());
        assert!(is_hallucination(&make("aaaaah what was that")).is_some());
        assert!(is_hallucination(&make("noooo way that happened")).is_some());
    }

    #[test]
    fn rejects_malformed_domain() {
        assert!(is_hallucination(&make("expenseÁê•-tee.com")).is_some());
        assert!(is_hallucination(&make("日本.com")).is_some());
    }

    #[test]
    fn rejects_contextless_short_fragments() {
        for text in [
            "Oh.", "you", "But...", "so.", "Well,", "Bleh.", "meh", "haha",
        ] {
            assert!(
                is_hallucination(&make(text)).is_some(),
                "standalone fragment `{text}` should be filtered"
            );
        }
    }

    #[test]
    fn rejects_segment_no_speech_with_low_confidence() {
        let mut r = make("Thank you for joining us today");
        r.segments.push(crate::stt::TranscriptionSegment {
            id: 0,
            start: 0.0,
            end: 1.0,
            text: "Thank you for joining us today".to_string(),
            no_speech_prob: 0.85,
            avg_logprob: -0.8,
            compression_ratio: 1.2,
            temperature: 0.0,
        });
        assert!(is_hallucination(&r).is_some());
    }

    #[test]
    fn rejects_high_compression_ratio() {
        let mut r = make("the quick brown fox jumps");
        r.segments.push(crate::stt::TranscriptionSegment {
            id: 0,
            start: 0.0,
            end: 2.0,
            text: "the quick brown fox jumps".to_string(),
            no_speech_prob: 0.1,
            avg_logprob: -0.2,
            compression_ratio: 3.0,
            temperature: 0.0,
        });
        assert!(is_hallucination(&r).is_some());
    }

    #[test]
    fn rejects_decoder_fallback_with_very_low_confidence() {
        let mut r = make("the quick brown fox jumps");
        r.segments.push(crate::stt::TranscriptionSegment {
            id: 0,
            start: 0.0,
            end: 2.0,
            text: "the quick brown fox jumps".to_string(),
            no_speech_prob: 0.1,
            avg_logprob: -1.5,
            compression_ratio: 1.5,
            temperature: 0.4,
        });
        assert!(is_hallucination(&r).is_some());
    }

    #[test]
    fn rejects_too_many_chars_per_sec() {
        // 30 chars in 0.5 sec = 60 chars/sec
        let r = make_with_duration("abcdefghij abcdefghij abcdefghi", 0.5);
        // But 0.5s with > 4 words also trips Layer C; either reason is fine.
        assert!(is_hallucination(&r).is_some());
    }

    #[test]
    fn accepts_one_word_affirmatives_and_negatives() {
        // Regression: earlier versions of the phrase list rejected
        // every short affirmative ("yes", "no", "yeah", "ok",
        // "okay", "yep", "nope"). Users dictating one-word answers
        // expect them to land. `ok`/`no` still trip the 3-char floor
        // (length < 3) — that gate is intentional and unchanged.
        for word in ["yes", "yeah", "okay", "yep", "nope", "hello", "hey", "wow"] {
            assert!(
                is_hallucination(&make(word)).is_none(),
                "single-word affirmative `{word}` should not be filtered"
            );
        }
    }

    #[test]
    fn accepts_real_sentences() {
        assert!(
            is_hallucination(&make("open the file")).is_none(),
            "should accept 'open the file'"
        );
        assert!(
            is_hallucination(&make("git status please")).is_none(),
            "should accept 'git status please'"
        );
        assert!(
            is_hallucination(&make("write a Rust function that parses JSON")).is_none(),
            "should accept regular sentence"
        );
        assert!(
            is_hallucination(&make("schedule a meeting for tomorrow at three pm")).is_none(),
            "should accept calendar request"
        );
        assert!(
            is_hallucination(&make("open settings")).is_none(),
            "should accept short command"
        );
    }

    #[test]
    fn accepts_real_sentence_with_good_segments() {
        let mut r = make("open the project folder and show me the README");
        r.duration = 3.0;
        r.segments.push(crate::stt::TranscriptionSegment {
            id: 0,
            start: 0.0,
            end: 3.0,
            text: "open the project folder and show me the README".to_string(),
            no_speech_prob: 0.05,
            avg_logprob: -0.2,
            compression_ratio: 1.4,
            temperature: 0.0,
        });
        assert!(is_hallucination(&r).is_none());
    }

    // ------------------------------------------------------------------
    // Layer H — foreign speech (acoustic evidence)
    // ------------------------------------------------------------------

    use crate::stt::{LiveStreamOutcome, RecognitionEvidence};

    fn heard(
        text: &str,
        confidence: Option<f32>,
        live_stream: LiveStreamOutcome,
        preview: Option<&str>,
    ) -> TranscriptionResult {
        TranscriptionResult {
            text: text.to_string(),
            evidence: RecognitionEvidence {
                confidence,
                live_stream,
                preview_text: preview.map(str::to_string),
            },
            ..Default::default()
        }
    }

    /// The reported bug, exactly as logged 2026-10-14T03:45:05Z: user
    /// speaking Nepali to their mom; live Apple session finished with no
    /// words (`streaming_final_empty`), no Parakeet preview text was ever
    /// shown, the speculative file pass returned "And a baggy?".
    #[test]
    fn rejects_and_a_baggy_nepali_misfire() {
        for preview in [None, Some("")] {
            let r = heard("And a baggy?", None, LiveStreamOutcome::NoWords, preview);
            assert_eq!(
                foreign_speech(&r, false),
                Some("foreign speech: live recognizer heard no words"),
                "preview={preview:?}"
            );
        }
    }

    #[test]
    fn live_session_glitch_does_not_cost_confident_english() {
        // Live session heard nothing but the file pass is confident.
        let r = heard(
            "Okay, so do all of that and fix everything.",
            Some(0.94),
            LiveStreamOutcome::NoWords,
            None,
        );
        assert!(foreign_speech(&r, false).is_none());
        // Live session heard nothing but Parakeet heard the same words.
        let r = heard(
            "Okay, so do all of that, fix everything,",
            None,
            LiveStreamOutcome::NoWords,
            Some("okay so do all of that fix everything"),
        );
        assert!(foreign_speech(&r, false).is_none());
    }

    #[test]
    fn parakeet_hearing_different_words_does_not_rescue() {
        let r = heard(
            "And a baggy?",
            None,
            LiveStreamOutcome::NoWords,
            Some("ama khana"),
        );
        assert!(foreign_speech(&r, false).is_some());
    }

    #[test]
    fn no_evidence_means_no_change() {
        // Groq / local backends report no evidence: never judged here.
        for text in ["And a baggy?", "Rings of Nanjani, Baina.", "No"] {
            assert!(
                foreign_speech(&heard(text, None, LiveStreamOutcome::NotRun, None), true).is_none()
            );
        }
        // Failed / timed-out live sessions say nothing about the audio.
        for outcome in [LiveStreamOutcome::Failed, LiveStreamOutcome::Unknown] {
            assert!(foreign_speech(&heard("And a baggy?", None, outcome, None), true).is_none());
        }
        // Live session heard words, no confidence: accept.
        assert!(
            foreign_speech(
                &heard("Whatever", None, LiveStreamOutcome::Heard, None),
                true
            )
            .is_none()
        );
        // Empty text is the empty-guard's job, not this layer's.
        assert!(
            foreign_speech(
                &heard("  ", Some(0.0), LiveStreamOutcome::NoWords, None),
                true
            )
            .is_none()
        );
    }

    /// Mean SpeechTranscriber (en_GB) word confidence MEASURED 2026-10-14
    /// on real audio: the user's own saved dictation clips
    /// (~/.always/failed-chunks) and synthetic `say` clips. Text shortened.
    const MEASURED_ENGLISH: &[(&str, f32)] = &[
        (
            "Can you go through all the failing tests and test out? …",
            0.957,
        ),
        ("without you touching them, commit only those, …", 0.867),
        ("Patience along with it as you go.", 0.765),
        (
            "Uh, uh, and all that at 1st and after that only, uh, we uh, …",
            0.596,
        ),
        (
            "Will the everyone not on? I want to see which is accurate.",
            0.609,
        ),
        ("but we cannot follow without bailing. No, I mean, …", 0.592),
        ("Okay, I am speaking now. Can you hear me, Iris?", 0.666),
        (
            "Uh, it does not look like uh, they are streaming in.",
            0.730,
        ),
        ("Okay, commit push all those changes then.", 0.875),
        ("Please, I need good code.", 0.875),
        ("You cannot do anything otherwise.", 0.862),
        ("Oh, it's not that slow. It's pretty fast actually.", 0.781),
        (
            "Stops always transcribing other people videos in calls.",
            0.892,
        ),
        // synthetic (macOS say, en_GB / en_IN voices)
        ("Yes.", 0.860),
        ("Whatever.", 0.886),
        ("Commit and push it.", 0.953),
        (
            "Why is this preview not showing in WhatsApp when I'm speaking?",
            0.911,
        ),
    ];
    const MEASURED_FOREIGN: &[(&str, f32)] = &[
        // the user's own saved Nepali speech
        (
            "Whisperway, Ma, Nepali, Ma, Boli, Rathu, Tunde, Buzna, …",
            0.095,
        ),
        ("Chaku, the Malay, Mangali.", 0.198),
        (
            "Go to Ludonia, or 10, I need below. See, see, see, …",
            0.209,
        ),
        (
            "So they go on there, they pick up any underwear, so...",
            0.269,
        ),
        // synthetic Nepali / Hindi (macOS say -v Lekha)
        ("Amma, kana, kanugyu, makali, hilo, ai, chuhe.", 0.152),
        ("Hajul Thik Chak, Bholi Bihan phone got to...", 0.341),
        ("Ki, Gharde, Huno, Hunj.", 0.307),
        ("Mummy, me kori, der, me, ata, kana, kalo.", 0.378),
        ("Bad, badme, bad, Karte, Hain.", 0.214),
        ("Huh?", 0.358),
    ];

    #[test]
    fn measured_confidence_separates_english_from_foreign() {
        for live in [LiveStreamOutcome::NotRun, LiveStreamOutcome::Heard] {
            for (text, c) in MEASURED_ENGLISH {
                let r = heard(text, Some(*c), live, None);
                assert!(foreign_speech_low_confidence(&r).is_none(), "{text} ({c})");
                for enforce in [false, true] {
                    assert!(
                        foreign_speech(&r, enforce).is_none(),
                        "genuine English rejected: {text} ({c}) enforce={enforce}"
                    );
                }
            }
            for (text, c) in MEASURED_FOREIGN {
                let r = heard(text, Some(*c), live, None);
                assert_eq!(foreign_speech_low_confidence(&r), Some(*c), "{text}");
                assert_eq!(
                    foreign_speech(&r, true),
                    Some("foreign speech: low acoustic confidence"),
                    "foreign speech accepted when enforced: {text} ({c})"
                );
                // Default rollout: rule 1 is log-only — still pasted.
                assert!(
                    foreign_speech(&r, false).is_none(),
                    "log-only rule 1 must not reject: {text} ({c})"
                );
            }
        }
    }

    #[test]
    fn preview_agreement_counts_shared_words() {
        assert_eq!(preview_agreement("Hello there.", "hello there"), 1.0);
        assert_eq!(preview_agreement("And a baggy?", ""), 0.0);
        assert_eq!(preview_agreement("", "hello"), 0.0);
        assert!((preview_agreement("open the file now", "open the") - 0.5).abs() < 1e-9);
    }

    #[test]
    fn normalize_strips_punctuation_and_collapses_whitespace() {
        assert_eq!(normalize("  Thank   you!  "), "thank you");
        assert_eq!(normalize("...Bye..."), "bye");
        assert_eq!(normalize("HELLO\tWORLD"), "hello world");
    }
}
