//! Rejected-by-My-Voice utterances, kept recoverable.
//!
//! When the speaker gate rejects an utterance it is never pasted (I3). But a
//! false reject of the user's OWN voice (e.g. music playing in the room)
//! used to lose the words entirely. Now the capture path stashes the audio
//! here (`stash_audio`), the event loop transcribes it in the background
//! (never pasting it), keeps the last [`RING_MAX`] results in memory, and
//! broadcasts `TranscriptRejected { id, text, score }`. A consumer (the
//! Iris notch: "Not sure that was you — paste anyway?") answers with
//! `PasteRejected { id }`, which runs the normal paste path.
//!
//! Nothing here touches disk: rejected audio may be a stranger's speech.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Recoverable rejected transcripts kept in memory.
pub const RING_MAX: usize = 8;
/// Older entries may no longer be pasted (focus has long moved on).
pub const RECOVER_TTL: Duration = Duration::from_secs(10 * 60);
/// Audio shorter than this is not worth a transcription (≈0.4 s).
const MIN_SAMPLES: usize = 6_400;
/// Cap the stashed audio (2 min) — rejected media can run long.
const MAX_SAMPLES: usize = 120 * 16_000;

pub struct StashedAudio {
    pub samples: Vec<i16>,
    pub score: f32,
}

static PENDING: Mutex<Option<StashedAudio>> = Mutex::new(None);

#[derive(Clone, Debug)]
pub struct Rejected {
    pub id: u64,
    pub text: String,
    pub score: f32,
    pub at: Instant,
}

static RING: Mutex<VecDeque<Rejected>> = Mutex::new(VecDeque::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn lock<T>(m: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Called by the capture loop right before it returns `DroppedSpeaker`.
pub fn stash_audio(samples: &[i16], score: f32) {
    if samples.len() < MIN_SAMPLES {
        return;
    }
    let start = samples.len().saturating_sub(MAX_SAMPLES);
    *lock(&PENDING) = Some(StashedAudio {
        samples: samples[start..].to_vec(),
        score,
    });
}

/// Taken by the event loop when it handles `DroppedSpeaker`.
pub fn take_stashed() -> Option<StashedAudio> {
    lock(&PENDING).take()
}

/// Record a transcribed rejected utterance. Returns its id.
pub fn push(text: &str, score: f32) -> u64 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut ring = lock(&RING);
    ring.push_back(Rejected {
        id,
        text: text.to_string(),
        score,
        at: Instant::now(),
    });
    while ring.len() > RING_MAX {
        ring.pop_front();
    }
    id
}

/// Score carried by entries that were not speaker-gate rejects (expired
/// held finals, utterances withheld by a pause). Serialized as `"-1.000"`.
pub const NO_SCORE: f32 = -1.0;

/// Keep an utterance whose paste was withheld because Always paused while
/// it was being spoken (per-app, mic conflict, …). Never pasted from here:
/// it lands in the ring and is announced as `TranscriptRejected`, so a
/// consumer can offer "paste anyway?" (`PasteRejected`). `reason` is the
/// pause-source tag, logged at info. Returns the ring id, or `None` for
/// empty text.
pub fn keep_withheld(text: &str, reason: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let id = push(text, NO_SCORE);
    tracing::info!(
        id,
        reason,
        chars = text.chars().count(),
        "paused_utterance_kept_recoverable"
    );
    crate::always::event::global_broadcaster().transcript_rejected_for(
        id,
        text.to_string(),
        NO_SCORE,
        crate::always::event::RejectReason::Paused,
    );
    Some(id)
}

/// Remove and return a recoverable entry (`id` 0 = most recent).
pub fn take(id: u64) -> Option<Rejected> {
    let mut ring = lock(&RING);
    ring.retain(|r| r.at.elapsed() < RECOVER_TTL);
    let pos = if id == 0 {
        ring.len().checked_sub(1)?
    } else {
        ring.iter().position(|r| r.id == id)?
    };
    ring.remove(pos)
}

/// Snapshot, newest last (for `ListRejected`).
pub fn list() -> Vec<Rejected> {
    let mut ring = lock(&RING);
    ring.retain(|r| r.at.elapsed() < RECOVER_TTL);
    ring.iter().cloned().collect()
}

/// The ring is process-global: every test that clears it or asserts on its
/// contents takes this lock (also used by `pause.rs` tests).
#[cfg(test)]
pub(crate) static TEST_SERIAL: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::TEST_SERIAL as SERIAL;
    use super::*;

    #[test]
    fn ring_keeps_last_n_and_take_removes() {
        let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        lock(&RING).clear();
        let ids: Vec<u64> = (0..RING_MAX + 3)
            .map(|i| push(&format!("t{i}"), 0.1))
            .collect();
        let l = list();
        assert_eq!(l.len(), RING_MAX);
        assert_eq!(l.last().unwrap().text, format!("t{}", RING_MAX + 2));
        assert!(take(ids[0]).is_none(), "oldest evicted");
        let last = take(0).expect("most recent");
        assert_eq!(last.id, *ids.last().unwrap());
        assert!(take(last.id).is_none(), "taken once");
    }

    #[test]
    fn stash_ignores_tiny_and_caps_long_audio() {
        let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        stash_audio(&[0i16; 100], 0.1);
        assert!(take_stashed().is_none());
        stash_audio(&vec![1i16; MAX_SAMPLES + 10], 0.2);
        let s = take_stashed().unwrap();
        assert_eq!(s.samples.len(), MAX_SAMPLES);
        assert!(take_stashed().is_none());
    }
}
