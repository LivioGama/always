//! Live typing (opt-in `live_typing`, macOS and Linux): words appear in the
//! focused app while the user is still speaking, and the final transcript
//! corrects them in place.
//!
//! Source: the live preview — the same text the overlay shows: the preview
//! loop in vad.rs, or with the Apple engine the instant-preview merger
//! (live_preview.rs, verified previews only). A
//! word is typed once every preview of the last 600 ms agrees on it; the
//! last, still-forming word is never typed early. Case or punctuation
//! differences alone never rewrite typed text. When the utterance is final,
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
    /// Recent previews (ms since the session began, open text), for
    /// agreement over time: see `stable_prefix`.
    recent: std::collections::VecDeque<(u64, String)>,
    /// When the session began, for `recent`'s timestamps.
    started: Option<std::time::Instant>,
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
    let now_ms = s
        .started
        .get_or_insert_with(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64;
    let agreed = stable_prefix(&mut s.recent, now_ms, open, stable_ms());
    let text = match (settled.filter(|t| !t.is_empty()), agreed.is_empty()) {
        (Some(settled), true) => settled.to_string(),
        (Some(settled), false) => format!("{settled} {agreed}"),
        (None, _) => agreed,
    };
    if text.is_empty() {
        return;
    }
    let target = keep_typed_formatting(&s.typed, &formatted(&text));
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
    let target = keep_typed_formatting(&s.typed, payload);
    let (backspaces, rest) = plan(&s.typed, &target);
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
/// How long a word must have read the same before it is typed (or before an
/// already-typed word is changed). Two previews agreeing is weak evidence
/// when they are close together (a cheap engine previews every ~150 ms and
/// both have heard nearly the same audio), so agreement is over time.
const STABLE_MS: u64 = 600;

fn stable_ms() -> u64 {
    // Session tests drive previews back to back: the two-preview rule.
    if cfg!(test) { 0 } else { STABLE_MS }
}

/// The words every preview of the last `stable_ms` agrees on (the current
/// one included, its last word excluded). Needs a preview at least
/// `stable_ms` old to compare against; until then nothing is agreed. At a
/// fixed 350 ms cadence this is the old two-consecutive-previews rule.
fn stable_prefix(
    recent: &mut std::collections::VecDeque<(u64, String)>,
    now_ms: u64,
    current: &str,
    stable_ms: u64,
) -> String {
    // Keep the newest preview at least `stable_ms` old as the anchor.
    while recent.len() >= 2 && now_ms.saturating_sub(recent[1].0) >= stable_ms {
        recent.pop_front();
    }
    let anchored = recent
        .front()
        .is_some_and(|(t, _)| now_ms.saturating_sub(*t) >= stable_ms);
    let agreed = if anchored {
        recent.iter().fold(current.to_string(), |acc, (_, p)| {
            agreed_prefix_all(&acc, p)
        })
    } else {
        String::new()
    };
    recent.push_back((now_ms, current.to_string()));
    let c: Vec<&str> = current.split_whitespace().collect();
    let n = agreed
        .split_whitespace()
        .count()
        .min(c.len().saturating_sub(1));
    c[..n].join(" ")
}

/// A word compared without its case and surrounding punctuation.
fn bare(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// `target`, but where its words only differ from what is already typed in
/// case or punctuation, the typed form stays: those differences are not
/// worth deleting and retyping text for (a comma for a full stop five words
/// back rewrote everything after it). Real word changes are kept.
fn keep_typed_formatting(typed: &str, target: &str) -> String {
    let t: Vec<&str> = typed.split_whitespace().collect();
    let g: Vec<&str> = target.split_whitespace().collect();
    if t.is_empty() || g.len() < t.len() || t.iter().zip(&g).any(|(a, b)| bare(a) != bare(b)) {
        return target.to_string();
    }
    let mut out = typed.trim_end().to_string();
    for w in &g[t.len()..] {
        out.push(' ');
        out.push_str(w);
    }
    // Trailing space: the target's when it adds words, else whichever of the
    // two has one (a finished sentence ends with a space).
    let target_tail = &target[target.trim_end().len()..];
    let typed_tail = &typed[typed.trim_end().len()..];
    out.push_str(if g.len() > t.len() || typed_tail.is_empty() {
        target_tail
    } else {
        typed_tail
    });
    out
}

/// Common leading words of two texts.
fn agreed_prefix_all(a: &str, b: &str) -> String {
    a.split_whitespace()
        .zip(b.split_whitespace())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
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
    if backspaces > 0 {
        crate::always::voice_edit::mark_self_post();
        ax_edit::post_strokes(
            KeyTarget::Hid,
            &[Stroke::Key {
                code: keys::DELETE,
                mods: Mods::NONE,
                repeat: backspaces,
            }],
        )?;
    }
    for (i, part) in text_events(text).into_iter().enumerate() {
        if i > 0 {
            std::thread::sleep(TEXT_EVENT_GAP);
        }
        // Per event: a paced burst can outlast one self-post grace window.
        crate::always::voice_edit::mark_self_post();
        ax_edit::post_strokes(KeyTarget::Hid, &[Stroke::Text(part)])?;
    }
    Ok(())
}

/// Pause between two text events. Sent back to back, a busy receiver
/// (Claude Code in a long session, 2026-10-22) re-inserted the tail of the
/// burst once per queued event: "…I don't understand.  having a computer?
/// I don't understand. I don't understand." Spaced, each event lands alone.
const TEXT_EVENT_GAP: std::time::Duration = std::time::Duration::from_millis(25);

/// The text events `text` is typed as: macOS posts at most 20 UTF-16 units
/// per Unicode keyboard event, split here on char boundaries so the events
/// can be paced. Linux types the whole text in one call.
fn text_events(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    if !cfg!(target_os = "macos") {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let (mut cur, mut units) = (String::new(), 0);
    for ch in text.chars() {
        if units + ch.len_utf16() > 20 {
            out.push(std::mem::take(&mut cur));
            units = 0;
        }
        cur.push(ch);
        units += ch.len_utf16();
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn text_events_split_at_20_utf16_units_on_char_boundaries() {
        let text = "Why is retrieve gated by having a computer? 👍 I don't understand.";
        let parts = text_events(text);
        assert_eq!(parts.concat(), text);
        assert!(parts.iter().all(|p| p.encode_utf16().count() <= 20));
        assert_eq!(parts[0], "Why is retrieve gate");
        assert!(text_events("").is_empty());
    }

    #[test]
    fn formatting_differences_keep_what_is_typed() {
        // A comma for a full stop: no rewrite, new words appended.
        assert_eq!(
            keep_typed_formatting(
                "Thanks for the reply. That",
                "Thanks for the reply, that sounds good"
            ),
            "Thanks for the reply. That sounds good"
        );
        // Same words, final punctuation: typed text stands, trailing space kept.
        assert_eq!(
            keep_typed_formatting("Ruf mich an", "Ruf mich an. "),
            "Ruf mich an "
        );
        // A real word change is corrected.
        assert_eq!(
            keep_typed_formatting("Der serve ist", "Der Server ist heute"),
            "Der Server ist heute"
        );
        assert_eq!(
            keep_typed_formatting("würde auf", "wurde auf Donnerstag"),
            "wurde auf Donnerstag"
        );
        // Nothing typed yet: the target as is.
        assert_eq!(keep_typed_formatting("", "Hello. "), "Hello. ");
    }

    #[test]
    fn stable_prefix_needs_agreement_over_time() {
        let mut r = std::collections::VecDeque::new();
        // First preview: nothing to compare against.
        assert_eq!(stable_prefix(&mut r, 0, "I have a", 350), "");
        // 150 ms later: agrees, but not yet for 350 ms.
        assert_eq!(stable_prefix(&mut r, 150, "I have a dream", 350), "");
        // 350 ms after the first: only what the first already had is agreed.
        assert_eq!(
            stable_prefix(&mut r, 350, "I have a dream today", 350),
            "I have a"
        );
        // A flip inside the window holds the flipped word back.
        assert_eq!(
            stable_prefix(&mut r, 500, "I have the dream today now", 350),
            "I have"
        );
        assert_eq!(
            stable_prefix(&mut r, 700, "I have the dream today now", 350),
            "I have"
        );
        // Stable again for 350 ms: the new variant is agreed.
        assert_eq!(
            stable_prefix(&mut r, 900, "I have the dream today now and", 350),
            "I have the dream today now"
        );
        // Fixed 350 ms cadence: the old two-preview rule.
        let mut r = std::collections::VecDeque::new();
        assert_eq!(stable_prefix(&mut r, 0, "a b c", 350), "");
        assert_eq!(stable_prefix(&mut r, 350, "a b c d", 350), "a b c");
    }

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
