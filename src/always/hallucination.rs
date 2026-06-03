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
    // Filler sounds
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
    "oh",
    "okay",
    "ok",
    "yeah",
    "yes",
    "no",
    "yep",
    "nope",
    "huh",
    "you",
    "what",
    "as",
    "the",
    "and",
    "ugh",
    "sigh",
    "boo",
    "blah",
    "hey",
    "hi",
    "hello",
    "oh my god",
    "wow",
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
    if result.duration > 0.0 {
        let chars_per_sec = text.chars().count() as f64 / result.duration;
        if chars_per_sec > 25.0 {
            return Some("too many chars per second");
        }
        let word_count = text.split_whitespace().count();
        if result.duration < 1.0 && word_count > 4 {
            return Some("too many words for short clip");
        }
    }

    // ---- Layer D: hallucination phrase blocklist ----
    let normalized = normalize(text);
    if !normalized.is_empty() {
        if PHRASE_SET.contains(normalized.as_str()) {
            return Some("hallucination phrase (exact match)");
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
    let chars: Vec<char> = text.chars().collect();
    if chars.windows(4).any(|w| {
        w[0] == w[1] && w[1] == w[2] && w[2] == w[3] && w[0].is_alphabetic()
    }) {
        return Some("repeated char run");
    }

    // ---- Layer G: suspicious single-token "domain" ----
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() == 1 && text.contains('.') {
        let has_non_ascii = text.chars().any(|c| !c.is_ascii());
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
        }
    }

    fn make_with_duration(text: &str, duration: f64) -> TranscriptionResult {
        TranscriptionResult {
            text: text.to_string(),
            duration,
            language: "en".to_string(),
            segments: Vec::new(),
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
        assert!(is_hallucination(&make("aaaaah what was that")).is_some());
        assert!(is_hallucination(&make("noooo way that happened")).is_some());
    }

    #[test]
    fn rejects_malformed_domain() {
        assert!(is_hallucination(&make("expenseÁê•-tee.com")).is_some());
        assert!(is_hallucination(&make("日本.com")).is_some());
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

    #[test]
    fn normalize_strips_punctuation_and_collapses_whitespace() {
        assert_eq!(normalize("  Thank   you!  "), "thank you");
        assert_eq!(normalize("...Bye..."), "bye");
        assert_eq!(normalize("HELLO\tWORLD"), "hello world");
    }
}
