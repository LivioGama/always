//! Held finals — consume-mode utterances waiting for the consumer's verdict.
//!
//! In consume mode the daemon does not paste a final; it broadcasts it and
//! parks the paste job here under a fresh id (`DaemonEvent::TranscriptHeld`).
//! The consumer (Iris) then decides:
//!
//! - `DaemonCommand::ReleaseHeld { id }` — it was dictation after all. The
//!   parked job runs the daemon's own native paste + auto-enter path, the
//!   same one an unheld utterance takes. No re-typing on the consumer side.
//! - `DaemonCommand::DiscardHeld { id }` — the consumer claimed it (a
//!   command, ambient speech). Dropped; nothing is inserted.
//!
//! An entry nobody decides on expires after [`HELD_TTL`]: pasting minutes
//! later into whatever has focus by then is worse than dropping it, and the
//! pre-handback behaviour of consume mode was "never paste" anyway.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long a held final waits for a verdict before it is dropped.
pub const HELD_TTL: Duration = Duration::from_secs(30);

/// Upper bound on parked entries — a consumer that never answers must not
/// grow this without limit.
const HELD_MAX: usize = 16;

pub type PasteJob = Box<dyn FnOnce(Instant) + Send + 'static>;

struct Held {
    id: u64,
    text: String,
    at: Instant,
    job: PasteJob,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static HELD: Mutex<VecDeque<Held>> = Mutex::new(VecDeque::new());

fn lock() -> std::sync::MutexGuard<'static, VecDeque<Held>> {
    HELD.lock().unwrap_or_else(|p| p.into_inner())
}

fn purge(q: &mut VecDeque<Held>, now: Instant) {
    while let Some(front) = q.front() {
        if now.saturating_duration_since(front.at) >= HELD_TTL || q.len() > HELD_MAX {
            let dropped = q.pop_front().expect("front exists");
            // Never silence: a final nobody answered for stays recoverable
            // in the same ring as gate-rejected transcripts ("paste
            // anyway?" → `PasteRejected`). Not pasted automatically — focus
            // has likely moved on.
            let rid = crate::always::rejected::push(&dropped.text, -1.0);
            crate::always::event::global_broadcaster().transcript_rejected_for(
                rid,
                dropped.text.clone(),
                -1.0,
                crate::always::event::RejectReason::Expired,
            );
            tracing::info!(id = dropped.id, recoverable_id = rid, "held_final_expired_recoverable");
        } else {
            break;
        }
    }
}

/// Park a paste job for `text`. Returns the id announced to consumers.
pub fn park(text: &str, job: PasteJob) -> u64 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let now = Instant::now();
    let mut q = lock();
    q.push_back(Held {
        id,
        text: text.to_string(),
        at: now,
        job,
    });
    purge(&mut q, now);
    drop(q);
    // Expiry must happen even if nothing else touches the queue, or an
    // unanswered final would vanish silently instead of becoming
    // recoverable.
    let _ = std::thread::Builder::new().name("held-expiry".into()).spawn(|| {
        std::thread::sleep(HELD_TTL + Duration::from_millis(200));
        purge(&mut lock(), Instant::now());
    });
    id
}

/// Remove an entry by id, or — when `id` is 0 / unknown — by exact text
/// (oldest match first). The text fallback lets a consumer that missed the
/// `TranscriptHeld` frame still hand an utterance back.
fn take(id: u64, text: Option<&str>) -> Option<Held> {
    let mut q = lock();
    purge(&mut q, Instant::now());
    let pos = q
        .iter()
        .position(|h| id != 0 && h.id == id)
        .or_else(|| text.and_then(|t| q.iter().position(|h| h.text == t)))?;
    q.remove(pos)
}

/// Run the parked native paste for `id` on its own thread (the paste path
/// sleeps for settle windows; the UDS reader must never block on it).
/// Returns false when nothing matched (already decided, or expired).
pub fn release(id: u64, text: Option<&str>) -> bool {
    let received = Instant::now();
    let Some(held) = take(id, text) else {
        tracing::info!(id, "held_final_release_unknown");
        return false;
    };
    tracing::info!(
        id = held.id,
        held_ms = received.saturating_duration_since(held.at).as_secs_f64() * 1000.0,
        "held_final_released"
    );
    let job = held.job;
    std::thread::Builder::new()
        .name("held-paste".into())
        .spawn(move || job(received))
        .map(|_| true)
        .unwrap_or(false)
}

/// Drop the parked final for `id` (the consumer claimed it).
pub fn discard(id: u64, text: Option<&str>) -> bool {
    match take(id, text) {
        Some(h) => {
            tracing::info!(id = h.id, "held_final_discarded");
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    // The queue is process-global; serialize the tests that touch it.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn expired_final_becomes_recoverable_not_silent() {
        let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let mut q = VecDeque::new();
        let now = Instant::now();
        q.push_back(Held {
            id: 99,
            text: "parked and forgotten".into(),
            at: now - HELD_TTL - Duration::from_secs(1),
            job: Box::new(|_| {}),
        });
        purge(&mut q, now);
        assert!(q.is_empty());
        let rec = crate::always::rejected::list();
        assert!(rec.iter().any(|r| r.text == "parked and forgotten"), "must land in the recoverable ring");
    }

    #[test]
    fn release_runs_the_parked_job_once() {
        let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let ran = Arc::new(AtomicBool::new(false));
        let r = Arc::clone(&ran);
        let id = park("hello there", Box::new(move |_| r.store(true, Ordering::SeqCst)));
        assert!(release(id, None));
        for _ in 0..200 {
            if ran.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(ran.load(Ordering::SeqCst), "job must run");
        assert!(!release(id, None), "second release finds nothing");
    }

    #[test]
    fn release_by_text_when_id_unknown() {
        let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let ran = Arc::new(AtomicBool::new(false));
        let r = Arc::clone(&ran);
        park("by text only", Box::new(move |_| r.store(true, Ordering::SeqCst)));
        assert!(release(0, Some("by text only")));
        for _ in 0..200 {
            if ran.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(ran.load(Ordering::SeqCst));
    }

    #[test]
    fn discard_never_runs_the_job() {
        let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let ran = Arc::new(AtomicBool::new(false));
        let r = Arc::clone(&ran);
        let id = park("open terminal", Box::new(move |_| r.store(true, Ordering::SeqCst)));
        assert!(discard(id, None));
        assert!(!release(id, None));
        std::thread::sleep(Duration::from_millis(20));
        assert!(!ran.load(Ordering::SeqCst));
    }
}
