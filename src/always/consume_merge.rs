//! Consume-mode utterance merging — give Iris the whole request, not the
//! fragment the VAD happened to cut.
//!
//! # Why this exists
//!
//! The paste path already rejoins fragments. When the user pauses briefly
//! mid-sentence the VAD ends the utterance, a second final arrives moments
//! later, and `event_loop` merges it into the in-flight dictation
//! (`merging resumed utterance into in-flight dictation`) so the clipboard
//! receives one sentence instead of two half-sentences.
//!
//! Consume mode never reached that code. `event_loop` broadcasts
//! `TranscriptFinal` and then, when a controller holds a consume lease,
//! returns *above* the merge — so the clipboard got stitched text and Iris
//! got the pieces. She would act on "open the" while the rest of the
//! sentence was still being spoken. That asymmetry is the mechanism behind
//! "the Always↔Iris integration is very poor": the daemon knew how to
//! rejoin the sentence and declined to do it for the one consumer that
//! most needed it.
//!
//! # The shape, and the latency trade
//!
//! Merging inherently means a commit cannot be issued the instant a
//! fragment lands — until the window expires, the daemon does not yet know
//! whether the sentence is finished. Making a consumer wait in silence for
//! that window would be its own bug, so nothing waits in silence:
//!
//! - every fragment immediately widens a `TranscriptChunk` carrying the
//!   **cumulative** text, which is the frame consumers already treat as
//!   "the utterance so far" — so the words appear the moment they are
//!   decoded and a live transcript keeps updating;
//! - exactly one `TranscriptFinal` is emitted, carrying the whole request,
//!   `MERGE_WINDOW_MS` after the last fragment.
//!
//! So the visible text is never delayed; only the *commit* — the point a
//! consumer starts doing work — waits. That is the right way round: acting
//! on half a request is worse than acting on all of it a moment later.
//!
//! Emitting the fragments as finals *and* the merged text as a final was
//! considered and rejected: the consumer's dedupe is an exact-string match,
//! so it would deliver both the pieces and the whole, i.e. the same
//! sentence twice.
//!
//! # Why the window is what it is
//!
//! Measured over every `transcription_received` pair in
//! `~/Library/Logs/Always/always.2026-08-2*..09-01` (n=78 gaps under 60s),
//! the distribution is cleanly bimodal:
//!
//! - fragments of one thought: 0.78s, 1.81s, 1.96s, 2.14s, 2.30s, 2.71s,
//!   2.98s, 3.87s, … up to ~8s
//! - genuinely separate requests: p25 = 11.6s, p50 = 20.3s
//!
//! with an empty band between them. Any window in 8–11s would separate the
//! populations perfectly, but 8s is far too long to hold a live assistant,
//! and a window that is too *generous* has its own cost: a controller that
//! armed itself on a partial stays armed until the final arrives, so a long
//! window widens the interval in which a lost final would strand it.
//!
//! 2500ms is the compromise, and it is chosen against a floor rather than
//! guessed: a resumed sentence cannot produce its next final sooner than
//! the end-of-utterance silence window (~0.9s) plus decode (~1.3s) ≈ 2.2s,
//! so anything below ~2.3s cannot merge a genuine continuation at all. It
//! sits above that floor and below the 3.87s gap that begins the sparse
//! tail. Set `ALWAYS_CONSUME_MERGE_WINDOW_MS=0` to disable merging and
//! restore one-final-per-fragment exactly.
//!
//! # Guarantees
//!
//! A buffered fragment is ALWAYS committed — by the timer, by
//! [`flush_now`] when the lease drops or the daemon stops. Nothing is left
//! stranded, because a consumer that armed on a partial is waiting for that
//! final to release it.

use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use tokio::runtime::Handle;

use super::event::global_broadcaster;
use super::localization::Localization;
use super::speech_action::merge_dictation_with;
use super::transcript_stream;

/// How long a merged utterance stays open for a continuation. See the
/// module docs for the measurement behind this number. 0 disables merging.
const MERGE_WINDOW_MS: u64 = 2500;

/// Escape hatch for tests and for a user who wants the old behaviour.
fn merge_window_ms() -> u64 {
    match std::env::var("ALWAYS_CONSUME_MERGE_WINDOW_MS") {
        Ok(v) if !v.is_empty() => v.parse().unwrap_or(MERGE_WINDOW_MS),
        _ => MERGE_WINDOW_MS,
    }
}

/// The sentence being assembled, with the stream setting captured when it
/// opened, so [`flush_now`] can commit it without a config in hand.
struct Pending {
    text: String,
    stream_enabled: bool,
}

/// The sentence being assembled. `None` between requests.
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);
/// Bumped by every fragment. A commit task holding a stale value knows a
/// newer fragment superseded it and exits without emitting — this is the
/// whole of the cancellation logic, and it cannot race: the generation is
/// bumped under the same lock that installs the text.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Pure core: what one fragment does to the sentence so far.
///
/// Returns the cumulative text. Delegates casing and spacing to
/// `merge_dictation_with`, the same helper the paste path uses, so a
/// merged request reads identically in both places.
pub fn fold(loc: &Localization, previous: Option<&str>, addition: &str) -> String {
    match previous {
        Some(prev) if !prev.is_empty() => merge_dictation_with(loc, prev, addition).0,
        _ => addition.trim_start().to_string(),
    }
}

/// Accept one accepted-and-final fragment while a consume lease is held.
///
/// Widens the pending sentence, publishes it as a cumulative
/// `TranscriptChunk`, and (re)arms the commit. When merging is disabled
/// this degrades to exactly the previous behaviour: one immediate
/// `TranscriptFinal` per fragment.
pub fn accept(rt: &Handle, loc: &Localization, stream_enabled: bool, text: &str) {
    let window = merge_window_ms();
    if window == 0 {
        commit_text(stream_enabled, text.to_string());
        return;
    }

    let (joined, generation) = {
        let mut pending = PENDING.lock();
        let joined = fold(loc, pending.as_ref().map(|p| p.text.as_str()), text);
        *pending = Some(Pending {
            text: joined.clone(),
            stream_enabled,
        });
        // Bump under the lock so a commit task can never observe a
        // generation that belongs to a different text.
        let generation = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
        (joined, generation)
    };

    tracing::info!(
        stage = "consume_merge",
        addition = %text,
        joined = %joined,
        window_ms = window,
        "holding the utterance open for a continuation"
    );
    // Timely: the words are on the wire now, as the utterance-so-far.
    global_broadcaster().transcript_chunk(joined);

    rt.spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(window)).await;
        if GENERATION.load(Ordering::Acquire) != generation {
            return; // a continuation arrived; that fragment owns the commit
        }
        let Some(pending) = PENDING.lock().take() else {
            return; // already flushed
        };
        tracing::info!(
            stage = "consume_merge",
            chars = pending.text.chars().count(),
            "utterance complete"
        );
        commit_text(pending.stream_enabled, pending.text);
    });
}

/// Commit whatever is pending right now, if anything. Called when the
/// consume lease drops and on shutdown so a half-assembled sentence is
/// never stranded in the buffer.
pub fn flush_now() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    let Some(pending) = PENDING.lock().take() else {
        return;
    };
    tracing::info!(
        stage = "consume_merge",
        chars = pending.text.chars().count(),
        "flushing the held utterance"
    );
    commit_text(pending.stream_enabled, pending.text);
}

fn commit_text(stream_enabled: bool, text: String) {
    global_broadcaster().transcript_final(text.clone());
    if stream_enabled {
        transcript_stream::append(&text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc() -> Localization {
        Localization::ENGLISH
    }

    /// The bug, stated as the behaviour that replaces it: three fragments
    /// of one spoken request become one sentence, cased and spaced the way
    /// the paste path would have done it.
    #[test]
    fn fragments_of_one_request_fold_into_one_sentence() {
        let loc = loc();
        let a = fold(&loc, None, "Iris, open the");
        assert_eq!(a, "Iris, open the");
        let b = fold(&loc, Some(&a), "browser and then");
        let c = fold(&loc, Some(&b), "close the tab");
        assert_eq!(c, "Iris, open the browser and then close the tab");
    }

    /// The wake word must survive folding, because the consumer re-checks
    /// it on the final's own text to decide whether the request was for
    /// her. Losing the leading word would silently reroute the request.
    #[test]
    fn folding_preserves_the_leading_wake_word() {
        let loc = loc();
        let joined = fold(&loc, Some("Iris, what is"), "on my calendar");
        assert!(
            joined.starts_with("Iris,"),
            "wake word must stay at the front: {joined:?}"
        );
    }

    /// A sentence that already ended keeps its capital on the next
    /// fragment; one cut mid-thought does not get a spurious one. This is
    /// `merge_dictation_with`'s behaviour and the reason to reuse it
    /// rather than concatenate with a space.
    #[test]
    fn folding_matches_the_paste_paths_casing() {
        let loc = loc();
        assert_eq!(
            fold(&loc, Some("Open the browser."), "And close the tab"),
            "Open the browser. And close the tab"
        );
        assert_eq!(
            fold(&loc, Some("Open the browser"), "And close the tab"),
            "Open the browser and close the tab"
        );
    }

    #[test]
    fn first_fragment_is_left_alone_apart_from_leading_space() {
        let loc = loc();
        assert_eq!(fold(&loc, None, "  hello there"), "hello there");
        assert_eq!(fold(&loc, Some(""), "hello there"), "hello there");
    }

    #[test]
    fn an_empty_continuation_cannot_shrink_the_sentence() {
        let loc = loc();
        let held = "Iris, open the browser";
        assert_eq!(fold(&loc, Some(held), "   "), held);
    }

    /// 0 means "behave exactly as before this module existed".
    #[test]
    fn window_can_be_disabled() {
        // SAFETY: single-threaded test, restored before returning.
        unsafe { std::env::set_var("ALWAYS_CONSUME_MERGE_WINDOW_MS", "0") };
        let w = merge_window_ms();
        unsafe { std::env::remove_var("ALWAYS_CONSUME_MERGE_WINDOW_MS") };
        assert_eq!(w, 0);
        assert_eq!(merge_window_ms(), MERGE_WINDOW_MS);
    }

    /// The window must clear the floor a genuine continuation cannot beat:
    /// end-of-utterance silence (~0.9s) plus decode (~1.3s). Below that it
    /// could never merge anything and would only add latency.
    #[test]
    fn window_clears_the_continuation_floor_and_stays_responsive() {
        assert!(
            MERGE_WINDOW_MS >= 2300,
            "below the ~2.2s silence+decode floor a continuation can never arrive in time"
        );
        assert!(
            MERGE_WINDOW_MS < 3870,
            "3.87s begins the sparse tail; beyond it the wait stops paying for itself"
        );
    }
}
