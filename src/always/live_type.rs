//! Live typing (opt-in `live_typing`, macOS and Linux): words appear in the
//! focused app while the user is still speaking, and the final transcript
//! corrects them in place.
//!
//! Source: the live preview — the same text the overlay shows: the preview
//! loop in vad.rs, or with the Apple engine the instant-preview merger
//! (live_preview.rs, verified previews only). A
//! word is typed once two consecutive previews agree on it; the last,
//! still-forming word is never typed early. When the utterance is final,
//! `deliver_paste` hands over the exact payload it would have pasted, and the
//! typed text is corrected to it (Backspace, then type) instead of pasting.
//!
//! Safety (SPEC §2): previews only exist for speech captured while listening
//! and, with My Voice on, verified (I3/I6). Typing stops the moment any pause
//! is active, and a microphone conflict ends the session without another
//! keystroke (I7). Nothing is ever sent once the focused window differs from
//! the one typing started in: the typed words stay where they were spoken and
//! the final takes the recoverable path (⌃⌥V).

use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::always::ax_edit::{self, KeyTarget, Mods, Stroke, keys};
use crate::always::localization::Localization;

#[derive(Default)]
struct Session {
    /// Utterance this session belongs to; stale previews are ignored.
    generation: u64,
    /// Exactly what this session typed, in order.
    typed: String,
    /// The previous preview, for agreement.
    last_preview: String,
    /// How a final payload is formatted (merge with in-flight dictation).
    loc: Option<Localization>,
    /// What had focus when typing began.
    window: Option<Focus>,
    /// No more keystrokes for this session (pause, conflict, focus moved).
    stopped: bool,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How an utterance's final was delivered.
#[derive(Debug, PartialEq, Eq)]
pub enum Finish {
    /// Nothing was typed live: paste the payload as usual.
    NotTyped,
    /// The typed words now read exactly the payload.
    Corrected,
    /// The focused window changed after typing began: no keystrokes were
    /// sent; the caller keeps the final recoverable.
    FocusMoved,
}

/// Live typing applies (the preference). Windows has no capture pipeline,
/// so it never begins a session there.
pub fn enabled(live_typing_pref: bool) -> bool {
    live_typing_pref
}

/// A new utterance began. Returns its generation, which the preview thread
/// carries so a late preview of an earlier utterance cannot type into this one.
pub fn begin(loc: Localization) -> u64 {
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    *lock() = Some(Session {
        generation,
        loc: Some(loc),
        ..Session::default()
    });
    generation
}

/// A whole-utterance preview for the open session (Apple's instant preview
/// merger publishes committed chunks + open speech as one text).
pub fn preview_latest(text: &str) {
    let generation = lock().as_ref().map(|s| s.generation);
    if let Some(generation) = generation {
        preview(generation, None, text);
    }
}

/// One live preview of the utterance: `settled` is the committed chunks'
/// text (final quality, never changes again), `open` the preview of the
/// speech after them. Types the words two previews agree on, formatted
/// exactly as the final payload will be (a continuation of in-flight
/// dictation is merged the way `deliver_paste` merges it); when the
/// committed text (or an agreed word) differs from what was typed, the
/// typed text is corrected right away, so typing never stalls behind one
/// changed word.
pub fn preview(generation: u64, settled: Option<&str>, open: &str) {
    let Some(loc) = lock()
        .as_ref()
        .filter(|s| s.generation == generation)
        .and_then(|s| s.loc)
    else {
        return;
    };
    let previous = crate::always::pause::dictation_buffer_text()
        .or_else(crate::always::dictation::session_text);
    preview_while(
        crate::always::pause::is_paused() || crate::always::pause::is_consume_mode(),
        generation,
        |stable| match &previous {
            Some(prev) => crate::always::speech_action::merge_dictation_with(&loc, prev, stable).1,
            None => stable.to_string(),
        },
        settled,
        open,
    );
}

fn preview_while(
    paused: bool,
    generation: u64,
    formatted: impl Fn(&str) -> String,
    settled: Option<&str>,
    open: &str,
) {
    let mut guard = lock();
    let Some(s) = guard
        .as_mut()
        .filter(|s| s.generation == generation && !s.stopped)
    else {
        return;
    };
    if paused {
        s.stopped = true;
        return;
    }
    let agreed = agreed_prefix(&s.last_preview, open);
    s.last_preview = open.to_string();
    let text = match (settled.filter(|t| !t.is_empty()), agreed.is_empty()) {
        (Some(settled), true) => settled.to_string(),
        (Some(settled), false) => format!("{settled} {agreed}"),
        (None, _) => agreed,
    };
    if text.is_empty() {
        return;
    }
    let target = formatted(&text);
    let (backspaces, rest) = plan(&s.typed, &target);
    if rest.is_empty() {
        // Nothing new, or the target is a shorter form of what is typed
        // (a preview dropped its last agreed word): leave it for the final.
        return;
    }
    if !same_window(&mut s.window) {
        s.stopped = true;
        tracing::info!("live_type_stopped_focus_moved");
        return;
    }
    match inject(backspaces, &rest) {
        Ok(()) => {
            s.typed = target;
            tracing::debug!(backspaces, chars = rest.chars().count(), "live_type_typed");
        }
        Err(error) => {
            s.stopped = true;
            tracing::warn!(%error, "live_type_failed");
        }
    }
}

/// The final payload for this utterance. Corrects the typed words to it, or
/// reports that the normal paste should run.
pub fn finish(payload: &str) -> Finish {
    let Some(mut s) = lock().take() else {
        return Finish::NotTyped;
    };
    if s.typed.is_empty() {
        return Finish::NotTyped;
    }
    if !same_window(&mut s.window) {
        tracing::info!(
            typed_chars = s.typed.chars().count(),
            "live_type_final_focus_moved"
        );
        return Finish::FocusMoved;
    }
    let (backspaces, rest) = plan(&s.typed, payload);
    if let Err(error) = inject(backspaces, &rest) {
        tracing::warn!(%error, "live_type_correction_failed");
    }
    tracing::info!(
        typed_chars = s.typed.chars().count(),
        backspaces,
        typed_after = rest.chars().count(),
        "live_type_corrected"
    );
    Finish::Corrected
}

/// The utterance was dropped (filtered, noise, too quiet): remove what was
/// typed for it — only in the window it was typed into.
pub fn retract() {
    if finish("") == Finish::Corrected {
        tracing::info!("live_type_retracted");
    }
}

/// End the session without another keystroke (microphone conflict, paused
/// final, clipboard failure): the typed words stay.
pub fn abandon() {
    lock().take();
}

fn lock() -> std::sync::MutexGuard<'static, Option<Session>> {
    SESSION.lock().unwrap_or_else(|p| p.into_inner())
}

/// Words two consecutive previews agree on, from the start — never the
/// last word of the newer one, which may still be forming.
fn agreed_prefix(previous: &str, current: &str) -> String {
    let p: Vec<&str> = previous.split_whitespace().collect();
    let c: Vec<&str> = current.split_whitespace().collect();
    let agreed = p.iter().zip(&c).take_while(|(a, b)| a == b).count();
    c[..agreed.min(c.len().saturating_sub(1))].join(" ")
}

/// Backspaces and text that turn `typed` into `target`.
/// ponytail: one Backspace per char — a grapheme cluster (emoji + modifier)
/// would need fewer; transcripts are plain text.
fn plan(typed: &str, target: &str) -> (usize, String) {
    let common = typed
        .chars()
        .zip(target.chars())
        .take_while(|(a, b)| a == b)
        .count();
    (
        typed.chars().count() - common,
        target.chars().skip(common).collect(),
    )
}

/// True while focus is where typing started (recording it on first use).
/// When focus cannot be read it is assumed unchanged.
fn same_window(window: &mut Option<Focus>) -> bool {
    let Some(now) = focus_now() else {
        return true;
    };
    match window {
        Some(start) => start.same_as(&now),
        None => {
            *window = Some(now);
            true
        }
    }
}

/// macOS: the frontmost app and its focused Accessibility element (the text
/// field itself, when the app exposes one).
#[cfg(target_os = "macos")]
struct Focus {
    app: Option<String>,
    field: Option<ax_edit::AxElem>,
}

#[cfg(target_os = "macos")]
impl Focus {
    fn same_as(&self, now: &Focus) -> bool {
        self.app == now.app
            && match (&self.field, &now.field) {
                (Some(a), Some(b)) => a.same_as(b),
                _ => true,
            }
    }
}

#[cfg(target_os = "macos")]
fn focus_now() -> Option<Focus> {
    if dry_run() {
        return None;
    }
    Some(Focus {
        app: crate::always::pause::current_app(),
        field: ax_edit::focused_element(),
    })
}

/// Linux: the focused window's Hyprland address. Other compositors offer no
/// focus query.
#[cfg(not(target_os = "macos"))]
struct Focus(String);

#[cfg(not(target_os = "macos"))]
impl Focus {
    fn same_as(&self, now: &Focus) -> bool {
        self.0 == now.0
    }
}

#[cfg(not(target_os = "macos"))]
fn focus_now() -> Option<Focus> {
    if dry_run() {
        return None;
    }
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let out = std::process::Command::new("hyprctl")
        .args(["activewindow", "-j"])
        .output()
        .ok()?;
    let window: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    window["address"].as_str().map(|a| Focus(a.to_string()))
}

/// `ALWAYS_LIVE_TYPE_LOG=<file>`: record keystrokes instead of sending them
/// (sandbox tests, together with `ALWAYS_NO_PASTE=1`).
fn dry_run_log() -> Option<String> {
    std::env::var("ALWAYS_LIVE_TYPE_LOG")
        .ok()
        .filter(|p| !p.is_empty())
}

pub fn dry_run() -> bool {
    dry_run_log().is_some()
}

/// Send `backspaces` Backspaces, then type `text`, through the keystroke
/// path voice editing uses (`ax_edit::post_strokes`), marked as Always's own
/// input so it never counts as the user typing.
fn inject(backspaces: usize, text: &str) -> Result<()> {
    if backspaces == 0 && text.is_empty() {
        return Ok(());
    }
    if let Some(path) = dry_run_log() {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {path}"))?;
        return writeln!(f, "BACKSPACE {backspaces} TYPE {text:?}").context("write dry-run log");
    }
    if std::env::var("ALWAYS_NO_PASTE").as_deref() == Ok("1") {
        return Ok(());
    }
    let mut strokes = Vec::with_capacity(2);
    if backspaces > 0 {
        strokes.push(Stroke::Key {
            code: keys::DELETE,
            mods: Mods::NONE,
            repeat: backspaces,
        });
    }
    if !text.is_empty() {
        strokes.push(Stroke::Text(text.to_string()));
    }
    crate::always::voice_edit::mark_self_post();
    ax_edit::post_strokes(KeyTarget::Hid, &strokes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The session is process-global: tests that open one run one at a time.
    static SESSION_TESTS: Mutex<()> = Mutex::new(());

    #[test]
    fn only_words_two_previews_agree_on_are_typed() {
        assert_eq!(agreed_prefix("", "I have a"), "");
        assert_eq!(agreed_prefix("I have a", "I have a dream"), "I have a");
        // The newest word is still forming: never typed early.
        assert_eq!(
            agreed_prefix("I have a dream", "I have a dream"),
            "I have a"
        );
        // A revised word stops agreement there.
        assert_eq!(agreed_prefix("I have to go", "I have a dream"), "I have");
    }

    #[test]
    fn a_pause_stops_typing_for_the_rest_of_the_utterance() {
        let _serial = SESSION_TESTS.lock().unwrap_or_else(|p| p.into_inner());
        let generation = begin(Localization::ENGLISH);
        let same = |t: &str| t.to_string();
        preview_while(true, generation, same, None, "I have");
        preview_while(false, generation, same, None, "I have a dream");
        preview_while(false, generation, same, None, "I have a dream that");
        // Nothing was typed: the final takes the normal paste path.
        assert_eq!(finish("I have a dream that. "), Finish::NotTyped);
    }

    #[test]
    fn a_committed_chunk_corrects_at_once_and_typing_continues() {
        let _serial = SESSION_TESTS.lock().unwrap_or_else(|p| p.into_inner());
        let log = std::env::temp_dir().join(format!("live-type-c-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&log);
        // SAFETY: test-only env, read by this module only.
        unsafe { std::env::set_var("ALWAYS_LIVE_TYPE_LOG", &log) };
        let generation = begin(Localization::ENGLISH);
        let same = |t: &str| t.to_string();
        preview_while(false, generation, same, None, "the duchess and none of");
        preview_while(false, generation, same, None, "the duchess and none of our");
        // Chunk 0 commits with different wording; the open speech is new.
        preview_while(
            false,
            generation,
            same,
            Some("the duchess, none of our"),
            "younger",
        );
        preview_while(
            false,
            generation,
            same,
            Some("the duchess, none of our"),
            "younger servants would",
        );
        let ops = std::fs::read_to_string(&log).unwrap();
        unsafe { std::env::remove_var("ALWAYS_LIVE_TYPE_LOG") };
        let _ = std::fs::remove_file(&log);
        assert_eq!(
            ops.lines().collect::<Vec<_>>(),
            [
                r#"BACKSPACE 0 TYPE "the duchess and none of""#,
                // Corrected as soon as the chunk is final ...
                r#"BACKSPACE 12 TYPE ", none of our""#,
                // ... and the open speech keeps flowing.
                r#"BACKSPACE 0 TYPE " younger""#,
            ]
        );
        abandon();
    }

    #[test]
    fn plan_corrects_the_differing_tail_only() {
        assert_eq!(plan("I have a", "I have a dream. "), (0, " dream. ".into()));
        assert_eq!(
            plan("I have to", "I have a dream. "),
            (2, "a dream. ".into())
        );
        assert_eq!(plan("Thank you", ""), (9, String::new()));
        assert_eq!(plan("", "Hi "), (0, "Hi ".into()));
    }

    #[test]
    fn a_session_types_agreed_words_and_corrects_to_the_final() {
        let _serial = SESSION_TESTS.lock().unwrap_or_else(|p| p.into_inner());
        let log = std::env::temp_dir().join(format!("live-type-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&log);
        // SAFETY: test-only env, read by this module only.
        unsafe { std::env::set_var("ALWAYS_LIVE_TYPE_LOG", &log) };
        let generation = begin(Localization::ENGLISH);
        let same = |t: &str| t.to_string();
        preview_while(false, generation, same, None, "I have");
        preview_while(false, generation, same, None, "I have a dream");
        preview_while(false, generation, same, None, "I have a dream that");
        // A stale preview from an earlier utterance types nothing.
        preview_while(
            false,
            generation - 1,
            same,
            None,
            "unrelated words here now",
        );
        assert_eq!(finish("I have a dream that one day. "), Finish::Corrected);
        assert_eq!(finish("anything"), Finish::NotTyped);
        let ops = std::fs::read_to_string(&log).unwrap();
        unsafe { std::env::remove_var("ALWAYS_LIVE_TYPE_LOG") };
        let _ = std::fs::remove_file(&log);
        assert_eq!(
            ops.lines().collect::<Vec<_>>(),
            [
                r#"BACKSPACE 0 TYPE "I have""#,
                r#"BACKSPACE 0 TYPE " a dream""#,
                r#"BACKSPACE 0 TYPE " that one day. ""#,
            ]
        );
    }
}
