//! Dictation session — the rolling buffer of text recently pasted into
//! the user's focused app, independent of the auto-enter countdown.
//!
//! Why this exists: each utterance used to be grammar-corrected and
//! pasted in isolation. Natural pauses longer than the silence window
//! split one thought into several utterances, each re-capitalized as a
//! fresh sentence — long-form dictation came out as choppy fragments.
//! The session gives two things to the next utterance:
//!
//! 1. **LLM context** — `session_context()` returns the tail of the
//!    recently pasted text so the grammar model can choose
//!    capitalization/punctuation that *continues* the document.
//! 2. **A merge anchor** — `session_text()` returns the full buffer so
//!    `handle_speech` can join the new utterance as a continuation
//!    (lowercase-safe delta) instead of a standalone sentence.
//!
//! Forward-only by design: text already pasted is never retro-edited
//! (the undo-repaste path corrupted Slack/terminals and was retired).
//!
//! Invalidates itself when:
//! - the focused app changed since the last paste (compared lazily on
//!   read — no extra focus hooks needed),
//! - more than [`SESSION_TIMEOUT`] passed since the last paste,
//! - `clear()` is called (wired into `pause::dictation_buffer_clear`,
//!   the single choke point hit by Return-commit, user keystrokes,
//!   pause transitions, and focus-driven pauses).

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// A pause longer than this ends the session: the user has moved on and
/// the next utterance should start a fresh sentence.
pub const SESSION_TIMEOUT: Duration = Duration::from_secs(20);
/// Tail length handed to the grammar LLM as `<context_before>`.
const CONTEXT_MAX_CHARS: usize = 400;
/// Cap on the retained buffer; trimmed from the front (oldest text).
const BUFFER_MAX_CHARS: usize = 2000;

#[derive(Debug)]
struct Session {
    buffer: String,
    last_paste_at: Instant,
    app: Option<String>,
}

static SESSION: LazyLock<Mutex<Option<Session>>> = LazyLock::new(|| Mutex::new(None));

/// Record the authoritative on-screen dictation text after a paste.
/// REPLACES the session buffer (the caller passes the full joined text,
/// not a delta) — replace semantics make this safe to call after any
/// clear/branch ordering in `handle_speech`.
pub fn note_pasted(total_text: &str) {
    note_pasted_for_app(total_text, super::pause::current_app());
}

fn note_pasted_for_app(total_text: &str, current_app: Option<String>) {
    if total_text.trim().is_empty() {
        return;
    }
    let mut buffer = total_text.to_string();
    if buffer.len() > BUFFER_MAX_CHARS {
        let cut = floor_char_boundary(&buffer, buffer.len() - BUFFER_MAX_CHARS);
        buffer.drain(..cut);
    }
    *SESSION.lock() = Some(Session {
        buffer,
        last_paste_at: Instant::now(),
        app: current_app,
    });
}

/// Full session buffer when the session is still live (fresh + same
/// app). Used as the merge anchor for continuation joining.
pub fn session_text() -> Option<String> {
    session_text_for_app(super::pause::current_app())
}

fn session_text_for_app(current_app: Option<String>) -> Option<String> {
    let mut slot = SESSION.lock();
    match slot.as_ref() {
        Some(s) if s.last_paste_at.elapsed() < SESSION_TIMEOUT && s.app == current_app => {
            Some(s.buffer.clone())
        }
        Some(_) => {
            // Stale or different app — drop it so it can't resurrect.
            *slot = None;
            None
        }
        None => None,
    }
}

/// Tail of the live session buffer (word-boundary cut, max
/// [`CONTEXT_MAX_CHARS`]) for the grammar LLM's `<context_before>`.
pub fn session_context() -> Option<String> {
    session_text().map(|full| tail_on_word_boundary(&full, CONTEXT_MAX_CHARS))
}

/// End the session. Wired into `pause::dictation_buffer_clear` so every
/// existing "user took control" signal (Return fired, keystroke, pause,
/// focus-driven pause) also ends the dictation session.
pub fn clear() {
    *SESSION.lock() = None;
}

fn tail_on_word_boundary(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    let byte_start = text
        .char_indices()
        .nth(char_count - max_chars)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let tail = &text[byte_start..];
    // Advance to the next whitespace so we never start mid-word.
    match tail.find(char::is_whitespace) {
        Some(ws) => tail[ws..].trim_start().to_string(),
        None => tail.to_string(),
    }
}

fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize tests in this module — they share the SESSION global.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn note_pasted_replaces_with_authoritative_text() {
        let _guard = TEST_LOCK.lock();
        clear();
        note_pasted_for_app("First sentence.", None);
        note_pasted_for_app("First sentence. and it continues", None);
        assert_eq!(
            session_text_for_app(None).as_deref(),
            Some("First sentence. and it continues")
        );
    }

    #[test]
    fn app_switch_invalidates_session() {
        let _guard = TEST_LOCK.lock();
        clear();
        note_pasted_for_app("typed into editor", Some("com.editor".into()));
        assert!(session_text_for_app(Some("com.other.app".into())).is_none());
        // And the stale session was dropped, not kept around.
        assert!(session_text_for_app(Some("com.editor".into())).is_none());
    }

    #[test]
    fn context_tail_cuts_on_word_boundary() {
        let long = "word ".repeat(200); // 1000 chars
        let tail = tail_on_word_boundary(&long, 400);
        assert!(tail.chars().count() <= 400);
        assert!(tail.starts_with("word"));
    }

    #[test]
    fn buffer_is_capped_from_the_front() {
        let _guard = TEST_LOCK.lock();
        clear();
        note_pasted_for_app(&format!("{}{}", "a".repeat(1500), "b".repeat(1500)), None);
        let text = session_text_for_app(None).unwrap();
        assert!(text.len() <= 2000);
        assert!(text.ends_with('b'));
    }

    #[test]
    fn clear_ends_session() {
        let _guard = TEST_LOCK.lock();
        clear();
        note_pasted_for_app("something", None);
        clear();
        assert!(session_text_for_app(None).is_none());
    }
}
