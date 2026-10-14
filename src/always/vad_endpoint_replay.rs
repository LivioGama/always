//! Replay of the 2026-10-14 dictation fragmentation through the
//! end-of-utterance decision (`classify_pause_text` +
//! `final_silence_cut_frames`).
//!
//! Source: ~/Library/Logs/Always/always.2026-10-14, 10:55-11:25Z. With
//! `auto_enter_delay_ms = 0`, every utterance cut is one paste and one
//! Return, so "cuts" below = Enters. Pieces are the recogniser's raw text
//! (tails as logged) and the pause after each piece:
//! - intra-utterance pauses are the logged `speech_resumed_after_pause`
//!   values (30-600 ms);
//! - fragment boundaries are upper bounds measured from the log
//!   (`voice_detected` of the next fragment − `preview_asr_utterance_end`
//!   of this one + the 0.6 s window that cut it).

use super::{
    FRAME_MS, MIDSENTENCE_DECISION_GRACE_FRAMES, PauseText, PauseVerdict, classify_pause_text,
    extended_silence_frames, final_silence_cut_frames, hesitation_silence_frames,
    partial_says_hesitating,
};
use crate::always::AlwaysConfig;
use crate::always::localization::Localization;

/// Prod: `stt_silence = 0.6` → 20 frames of 30 ms.
const BASE: usize = 20;

fn hesitation() -> usize {
    hesitation_silence_frames(&AlwaysConfig::default())
}

/// How the loop decides one pause, frame by frame: the deciding text
/// lands `text_latency_ms` into the pause (speculative STT or the flushed
/// chunk's raw STT). Returns `true` if the pause ends the utterance before
/// speech resumes at `pause_ms` (`None` = never resumes).
fn pause_cuts(text: &str, pause_ms: Option<u32>, text_latency_ms: u32) -> bool {
    let loc = &Localization::ENGLISH;
    let pause_frames = pause_ms.map(|ms| (ms / FRAME_MS) as usize);
    let latency_frames = (text_latency_ms / FRAME_MS) as usize;
    for frame in 1..=10_000usize {
        if pause_frames.is_some_and(|p| frame > p) {
            return false; // speech resumed first: same utterance
        }
        let decided = frame >= latency_frames;
        let cut = final_silence_cut_frames(
            BASE,
            BASE,
            hesitation(),
            PauseVerdict {
                text_pending: !decided,
                text: decided.then(|| classify_pause_text(loc, text)),
                partial_hesitating: false,
            },
        );
        if frame >= cut {
            return true;
        }
    }
    panic!("a pause that never resumes must end the utterance");
}

/// Replays one spoken sequence; returns the number of Enters (= cuts).
fn enters_for(pieces: &[(&str, Option<u32>)], text_latency_ms: u32) -> usize {
    pieces
        .iter()
        .filter(|(text, pause)| pause_cuts(text, *pause, text_latency_ms))
        .count()
}

/// Logged Apple STT for a 6-10 s chunk landed 152-551 ms after the cut
/// (`stt_wait_ms`), i.e. inside the hold.
const TEXT_LATENCY_MS: u32 = 450;

#[test]
fn hesitation_hold_default_is_2_5_s() {
    assert_eq!(hesitation(), 84); // ceil(2500 / 30)
    assert!(hesitation() > extended_silence_frames(BASE));
}

#[test]
fn today_fragment_endings_classify() {
    let loc = &Localization::ENGLISH;
    // Fragments that were pasted+Entered while the user kept talking,
    // and that end on positive hesitation evidence.
    for t in [
        "I think we should show the, uh, my uh, transcription in the website that goes all the...",
        "not disturb anything on the screen would just show in the upside and after like a tiny...",
        "Just, uh, go away, uh, it would just uh, collapse back and if I want to.",
        "but it would be really nice to...",
        "Maybe, uh, and, uh, I think it's that, uh,",
        "or the entire had no delay, but it was still so great and.",
        "it's not even clear what, you know",
        "it was, like",
        "so we should, um",
    ] {
        assert_eq!(classify_pause_text(loc, t), PauseText::Hesitating, "{t}");
    }
    // Complete sentences: unchanged, finish at the base window.
    for t in [
        "What time is it?",
        "Send the email now.",
        "It is a pretty good idea. Basically, it would feel the whole lip screen as need.",
        "I don't know.",
    ] {
        assert_eq!(classify_pause_text(loc, t), PauseText::Complete, "{t}");
    }
    assert_eq!(classify_pause_text(loc, "Very nice to have"), PauseText::MidSentence);
    // Partials (no punctuation): only a positive hesitation counts.
    assert!(partial_says_hesitating(loc, "show it on the left and uh"));
    assert!(partial_says_hesitating(loc, "it should go all the way to the"));
    assert!(!partial_says_hesitating(loc, "what time is it"));
}

/// 11:04:32-11:04:47Z: "...after like a tiny..." → 1.21 s → "Just, uh, go
/// away, ... if I want to." → 1.44 s → "I could just uh go to the
/// detailed view uh in the notch and everything." Logged: 3 pastes, 3
/// Enters (each 51 ms after its paste). Now: one paste, one Enter.
#[test]
fn replay_1104_tiny_go_away_detailed_view_is_one_enter() {
    let pieces: &[(&str, Option<u32>)] = &[
        ("Basically, because it would not disturb anything on", Some(540)),
        ("the screen would just show in the upside and after like a tiny...", Some(1_210)),
        ("Just, uh, go away,", Some(300)),
        ("uh, it would just uh, collapse back and if I want to.", Some(1_440)),
        ("I could just uh go to the detailed view uh", Some(90)),
        ("in the notch and everything.", None),
    ];
    assert_eq!(enters_for(pieces, TEXT_LATENCY_MS), 1);
}

/// 11:09:01-11:09:11Z: "...but it would be really nice to..." → 1.63 s →
/// "The transcript, uh, to the right, the strip, ..." Logged: 2 Enters.
#[test]
fn replay_1109_really_nice_to_is_one_enter() {
    let pieces: &[(&str, Option<u32>)] = &[
        ("And basically, Iris spoke, and after she was done speaking,", Some(480)),
        ("I saw the transcript at the bottom, but it would be really nice to...", Some(1_630)),
        ("The transcript, uh, to the right, the strip,", Some(30)),
        ("uh, not the bottom, the right strip,", Some(660)),
        ("and as she was speaking, not, uh, after.", None),
    ];
    assert_eq!(enters_for(pieces, TEXT_LATENCY_MS), 1);
}

/// 11:04:10-11:04:17Z: "...that goes all the..." → ≤2.43 s → "To the left
/// as needed, ..." Logged: 2 Enters.
#[test]
fn replay_1104_goes_all_the_is_one_enter() {
    let pieces: &[(&str, Option<u32>)] = &[
        ("I think we should show the, uh, my uh, transcription in the website that goes all the...", Some(2_430)),
        ("To the left as needed, and uh, the Irish uh, transcript,", Some(30)),
        ("uh, it should go all the way to it.", None),
    ];
    assert_eq!(enters_for(pieces, TEXT_LATENCY_MS), 1);
}

/// The live partial hears "uh" before any final text exists: the hold
/// applies from the first check, even if the chunk's STT is slow.
#[test]
fn partial_hesitation_holds_before_text_lands() {
    let v = PauseVerdict { text_pending: true, text: None, partial_hesitating: true };
    assert_eq!(final_silence_cut_frames(BASE, BASE, hesitation(), v), hesitation());
}

/// Intra-utterance pauses of 30-600 ms (all logged values today) never
/// cut, whatever the text says.
#[test]
fn short_pauses_never_cut() {
    for ms in [30, 60, 90, 120, 150, 180, 210, 240, 270, 300, 330, 360, 420, 450, 480, 540, 570] {
        assert!(!pause_cuts("Send the email now.", Some(ms), 0), "{ms} ms");
        assert!(!pause_cuts("and uh", Some(ms), 0), "{ms} ms");
    }
}

/// A complete short command still ends at the base window — no added
/// delay: cut at frame 20 (600 ms) once its text is in, same as before.
#[test]
fn complete_command_enters_at_base_window() {
    let loc = &Localization::ENGLISH;
    let decided = |t: &str| PauseVerdict {
        text_pending: false,
        text: Some(classify_pause_text(loc, t)),
        partial_hesitating: false,
    };
    for t in ["What time is it?", "Send the email now.", "Stop!"] {
        assert_eq!(final_silence_cut_frames(BASE, BASE, hesitation(), decided(t)), BASE, "{t}");
    }
    // Text still in flight: the pre-existing decision grace, unchanged.
    let pending = PauseVerdict { text_pending: true, text: None, partial_hesitating: false };
    assert_eq!(
        final_silence_cut_frames(BASE, BASE, hesitation(), pending),
        BASE + MIDSENTENCE_DECISION_GRACE_FRAMES
    );
    // No text at all (adaptive off / short utterance): plain base window.
    let none = PauseVerdict { text_pending: false, text: None, partial_hesitating: false };
    assert_eq!(final_silence_cut_frames(BASE, BASE, hesitation(), none), BASE);
    assert_eq!(enters_for(&[("What time is it?", None)], 0), 1);
    assert!(pause_cuts("What time is it?", Some(10_000), 0));
}

/// Never hangs: a hesitation followed by real silence (the 10.4 s gap
/// at 11:17:29Z) still ends the utterance — at the hesitation window.
#[test]
fn hesitation_then_long_silence_still_ends() {
    assert!(pause_cuts("Maybe, uh, and, uh, I think it's that, uh,", Some(10_400), TEXT_LATENCY_MS));
    assert!(pause_cuts("goes all the...", None, TEXT_LATENCY_MS));
}

/// Honest boundary: a fragment that ends as a COMPLETE sentence still
/// ends at the base window even if the user resumes 1.0-1.8 s later —
/// e.g. 11:03:38Z "...the whole lip screen as need." → ≤1.07 s. No text
/// says "more is coming" there, and the user ruled out a blanket delay.
#[test]
fn complete_sentence_then_resume_still_cuts() {
    assert!(pause_cuts(
        "It is a pretty good idea. Basically, it would feel the whole lip screen as need.",
        Some(1_070),
        TEXT_LATENCY_MS
    ));
}
