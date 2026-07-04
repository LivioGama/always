//! Global keyboard shortcut handler — pause toggle, auto-enter toggle, force-paste.
//!
//! Default shortcuts: ⌃⌥P (pause), ⌃⌥A (auto-enter), ⌃⌥V (paste-anyway last filtered).
//! All configurable via `always config set shortcut_pause ctrl+alt+p`
//! (takes effect on daemon restart).
//!
//! ## Cross-platform layout
//!
//! `Combo` (parsing + matching) is portable — modifier flags + a single
//! key character string. The actual global hotkey listener that converts
//! OS key events into `Combo::matches_name` calls is platform-specific:
//!
//! * **macOS** (`feature = "macos"`): uses `rdev` to subscribe to system
//!   keyboard events. (P2 will replace `rdev` with a thin `CGEventTap`
//!   wrapper to drop the unmaintained dep.)
//! * **Linux / Windows**: stub — `start_keyboard_listener` returns `Ok(())`
//!   without registering anything. Voice activation still works; users
//!   must toggle pause/auto-enter via the CLI for now.

#[cfg(feature = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "macos")]
use std::sync::mpsc;
#[cfg(feature = "macos")]
use std::thread;
#[cfg(feature = "macos")]
use std::time::Duration;

use anyhow::Result;

#[cfg(feature = "macos")]
use super::{clipboard_watcher, config as always_config, correction, event, log, paste, pause};

/// Event-based tracking of Command key state as a fallback when
/// CGEventSourceFlagsState is unreliable. Updated by the keyboard listener.
#[cfg(feature = "macos")]
static CMD_HELD_EVENT: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "macos")]
static OPTION_HELD_EVENT: AtomicBool = AtomicBool::new(false);

/// Raw check of CGEventSourceFlagsState - can be unreliable on some systems.
#[cfg(feature = "macos")]
fn is_cmd_held_cg() -> bool {
    unsafe extern "C" {
        fn CGEventSourceFlagsState(state_id: i32) -> u64;
    }
    const HID_SYSTEM_STATE: i32 = 1;
    const CG_EVENT_FLAG_COMMAND: u64 = 0x0010_0000;
    let flags = unsafe { CGEventSourceFlagsState(HID_SYSTEM_STATE) };
    flags & CG_EVENT_FLAG_COMMAND != 0
}

/// True when the user is physically holding ⌘ right now.
///
/// Uses a conservative hybrid approach to minimize false positives:
/// 1. Primary: CGEventSourceFlagsState for real hardware state (avoids stuck events)
/// 2. Fallback: Event-based tracking when CG is unreliable
/// 3. Strict debounce: Requires BOTH methods to agree across multiple checks
///
/// This addresses the issue where CGEventSourceFlagsState can report false
/// positives on some systems. We now require both methods to agree AND
/// maintain that state across multiple checks to avoid transient false positives.
#[cfg(feature = "macos")]
pub fn is_cmd_held() -> bool {
    // Quick single check first - if either method definitely says not held, return fast
    let cg_result = is_cmd_held_cg();
    let event_result = CMD_HELD_EVENT.load(Ordering::Relaxed);

    // If both agree it's not held, we're confident
    if !cg_result && !event_result {
        return false;
    }

    // If they disagree, be conservative and assume not held (avoid false positives)
    if cg_result != event_result {
        return false;
    }

    // Both agree it's held - do debounced checks to confirm it's stable
    let mut held_count = 0;
    let checks = 5;
    let delay = Duration::from_millis(10);

    for _ in 0..checks {
        let cg = is_cmd_held_cg();
        let event = CMD_HELD_EVENT.load(Ordering::Relaxed);

        // Require both methods to still agree and report held
        if cg && event {
            held_count += 1;
        } else {
            // If either method changes its mind, immediately return false
            return false;
        }
        std::thread::sleep(delay);
    }

    // Require unanimous agreement across all checks
    held_count == checks
}

#[cfg(not(feature = "macos"))]
pub fn is_cmd_held() -> bool {
    false
}

#[cfg(feature = "macos")]
fn is_option_held_cg() -> bool {
    unsafe extern "C" {
        fn CGEventSourceFlagsState(state_id: i32) -> u64;
    }
    const HID_SYSTEM_STATE: i32 = 1;
    const CG_EVENT_FLAG_ALTERNATE: u64 = 0x0008_0000;
    let flags = unsafe { CGEventSourceFlagsState(HID_SYSTEM_STATE) };
    flags & CG_EVENT_FLAG_ALTERNATE != 0
}

#[cfg(feature = "macos")]
pub fn is_option_held() -> bool {
    is_option_held_cg() || OPTION_HELD_EVENT.load(Ordering::Relaxed)
}

#[cfg(not(feature = "macos"))]
pub fn is_option_held() -> bool {
    false
}

/// A parsed keyboard combo: modifier flags + a single character key.
#[allow(dead_code)] // fields are unused on non-macos until Linux/Windows listeners land
#[derive(Clone, Debug)]
struct Combo {
    ctrl: bool,
    shift: bool,
    alt: bool,
    key_char: String,
}

#[allow(dead_code)] // listener body uses these on macOS only
impl Combo {
    /// Parse `"ctrl+alt+p"` (case-insensitive, any modifier order).
    /// Requires at least one modifier and exactly one key character.
    fn from_str(s: &str) -> Option<Self> {
        let mut ctrl = false;
        let mut shift = false;
        let mut alt = false;
        let mut key_char: Option<String> = None;

        for part in s.to_lowercase().split('+') {
            match part.trim() {
                "ctrl" | "control" => ctrl = true,
                "shift" => shift = true,
                "alt" | "option" => alt = true,
                "meta" | "cmd" | "command" => {}
                c if !c.is_empty() => key_char = Some(c.to_string()),
                _ => {}
            }
        }

        let key_char = key_char?;
        if !ctrl && !shift && !alt {
            return None;
        }
        if key_char.len() != 1 && key_char != "space" {
            return None;
        }

        Some(Combo {
            ctrl,
            shift,
            alt,
            key_char,
        })
    }

    /// Match a fired event by its modifier state + the key's portable
    /// shortcut name (e.g. `"p"`, `"space"`, `"a"`).
    fn matches_name(&self, ctrl: bool, shift: bool, alt: bool, key_name: &str) -> bool {
        self.ctrl == ctrl && self.shift == shift && self.alt == alt && self.key_char == key_name
    }
}

#[allow(dead_code)] // referenced only in tests + macos listener
fn default_shortcuts() -> (Combo, Combo, Combo, Combo, Combo, Combo) {
    (
        // ⌃⌥P is strictly per-app: toggle the focused app's place on
        // the resumed allowlist. Global pause lives on ⌃⌥⇧P below.
        Combo::from_str("ctrl+alt+p").expect("default pause shortcut is valid"),
        Combo::from_str("ctrl+alt+a").expect("default auto-enter shortcut is valid"),
        Combo::from_str("ctrl+alt+v").expect("default force-paste shortcut is valid"),
        // ⌃⌥X is the hotkey-driven correction capture: read the user's
        // current selection, diff it against the most recent paste, and
        // append the resulting `(wrong → right)` pairs to the glossary.
        Combo::from_str("ctrl+alt+x").expect("default log-correction shortcut is valid"),
        // ⌃⌥W opens the dialog-driven correction path: type the
        // intended spelling, daemon diffs against the last transcript.
        Combo::from_str("ctrl+alt+w").expect("default correction-dialog shortcut is valid"),
        // ⌃⌥⇧P flips the master (global) pause switch.
        Combo::from_str("ctrl+alt+shift+p").expect("default master-pause shortcut is valid"),
    )
}

#[cfg(feature = "macos")]
#[allow(clippy::too_many_arguments)]
fn resolve_shortcuts(
    pause: Option<&str>,
    auto_enter: Option<&str>,
    force_paste: Option<&str>,
    log_correction: Option<&str>,
    correction_dialog: Option<&str>,
    master_pause: Option<&str>,
) -> (Combo, Combo, Combo, Combo, Combo, Combo) {
    let (
        default_pause,
        default_auto_enter,
        default_force_paste,
        default_log_correction,
        default_correction_dialog,
        default_master_pause,
    ) = default_shortcuts();
    let pause_combo = pause.and_then(Combo::from_str).unwrap_or(default_pause);
    let auto_enter_combo = auto_enter
        .and_then(Combo::from_str)
        .unwrap_or(default_auto_enter);
    let force_paste_combo = force_paste
        .and_then(Combo::from_str)
        .unwrap_or(default_force_paste);
    let log_correction_combo = log_correction
        .and_then(Combo::from_str)
        .unwrap_or(default_log_correction);
    let correction_dialog_combo = correction_dialog
        .and_then(Combo::from_str)
        .unwrap_or(default_correction_dialog);
    let master_pause_combo = master_pause
        .and_then(Combo::from_str)
        .unwrap_or(default_master_pause);

    (
        pause_combo,
        auto_enter_combo,
        force_paste_combo,
        log_correction_combo,
        correction_dialog_combo,
        master_pause_combo,
    )
}

#[cfg(feature = "macos")]
fn load_shortcuts() -> (Combo, Combo, Combo, Combo, Combo, Combo) {
    let defaults = default_shortcuts();

    let Ok(conn) = crate::db::open() else {
        return defaults;
    };
    let Ok(prefs) = crate::db::get_preferences(&conn) else {
        return defaults;
    };

    resolve_shortcuts(
        prefs.shortcut_pause.as_deref(),
        prefs.shortcut_auto_enter.as_deref(),
        prefs.shortcut_force_paste.as_deref(),
        prefs.shortcut_log_correction.as_deref(),
        prefs.shortcut_correction_dialog.as_deref(),
        prefs.shortcut_master_pause.as_deref(),
    )
}

// ----------------------------------------------------------------------
// macOS implementation (rdev-backed; will become CGEventTap in P2.2)
// ----------------------------------------------------------------------
#[cfg(feature = "macos")]
fn key_to_shortcut_name(key: &rdev::Key) -> Option<&'static str> {
    use rdev::Key;
    match key {
        Key::KeyA => Some("a"),
        Key::KeyB => Some("b"),
        Key::KeyC => Some("c"),
        Key::KeyD => Some("d"),
        Key::KeyE => Some("e"),
        Key::KeyF => Some("f"),
        Key::KeyG => Some("g"),
        Key::KeyH => Some("h"),
        Key::KeyI => Some("i"),
        Key::KeyJ => Some("j"),
        Key::KeyK => Some("k"),
        Key::KeyL => Some("l"),
        Key::KeyM => Some("m"),
        Key::KeyN => Some("n"),
        Key::KeyO => Some("o"),
        Key::KeyP => Some("p"),
        Key::KeyQ => Some("q"),
        Key::KeyR => Some("r"),
        Key::KeyS => Some("s"),
        Key::KeyT => Some("t"),
        Key::KeyU => Some("u"),
        Key::KeyV => Some("v"),
        Key::KeyW => Some("w"),
        Key::KeyX => Some("x"),
        Key::KeyY => Some("y"),
        Key::KeyZ => Some("z"),
        Key::Space => Some("space"),
        _ => None,
    }
}

/// Resolution of a per-app pause chord press. Pure data so the
/// decision is unit-testable without a live focus monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // consumed by the macos listener only
enum ChordAction {
    /// Toggle this bundle's place on the resumed-allowlist.
    TogglePerApp(String),
    /// No real app is focused (none reported yet, or Always itself is
    /// frontmost) — nothing sensible to toggle; give feedback instead.
    NoFocusedApp,
}

/// Decide what the per-app pause chord should do for the currently
/// focused app. Strictly per-app: master pause never hijacks this
/// chord (it has its own), and Always's own windows are never a valid
/// toggle target.
#[allow(dead_code)] // referenced only in tests + macos listener
fn pause_chord_action(current_app: Option<&str>) -> ChordAction {
    match current_app {
        Some(bundle) if !crate::always::per_app::is_own_bundle_id(bundle) => {
            ChordAction::TogglePerApp(bundle.to_string())
        }
        _ => ChordAction::NoFocusedApp,
    }
}

/// Toggle the per-app allowlist for `bundle`. Adds the bundle as
/// resumed (override `paused: false`) when it's not yet listed;
/// removes the override otherwise. Recomputes the effective state and
/// broadcasts the appropriate UDS events so every connected GUI
/// surface updates immediately.
#[cfg(feature = "macos")]
fn handle_per_app_pause_hotkey(bundle: &str) {
    use crate::always::per_app;
    let was_resumed = per_app::is_app_resumed(bundle);
    let new_paused: Option<bool> = if was_resumed { None } else { Some(false) };
    if let Err(e) = per_app::set_app_paused_override(bundle, new_paused) {
        tracing::error!(error = %e, bundle, "hotkey_set_app_paused_failed");
        return;
    }
    let (effective, changed) = pause::recompute_effective();
    if changed {
        if effective {
            pause::dictation_buffer_clear();
            event::global_broadcaster().paused();
        } else {
            pause::mark_voice_seen();
            event::global_broadcaster().resumed();
        }
    }
    event::global_broadcaster().resumed_apps_changed(per_app::resumed_apps());
    // Scope feedback: tell the GUI exactly what this chord did
    // ("Resumed in Safari"), independent of the effective-state
    // events above. `paused` is from the app's perspective: it was
    // resumed → now paused again, and vice versa.
    event::global_broadcaster().pause_scope_toggled("app", Some(bundle.to_string()), was_resumed);
    if let Ok(log_path) = always_config::configured_log_path()
        && let Ok(mut logger) = log::Logger::open(&log_path)
    {
        // Reuse the existing PauseToggled event so log scrapers don't
        // need a new variant — `paused` reflects the new effective
        // state for the focused app.
        logger.write(log::Event::PauseToggled { paused: effective });
    }
    tracing::info!(
        bundle,
        was_resumed,
        new_resumed = !was_resumed,
        effective,
        scope = "app",
        "hotkey_pause_chord"
    );
}

/// ctrl+option+shift+P: global pause/resume. When ANY global source is
/// pausing (user master pause, audio-output watchdog, mic conflict) the
/// chord clears them ALL — an explicit user resume overrides the
/// watchdogs until their next transition, so dictating over music or
/// into notes during a call is one chord away. Otherwise it sets the
/// user master pause.
#[cfg(feature = "macos")]
fn handle_master_pause_hotkey() {
    let (effective, changed) = if pause::is_any_global_pause() {
        pause::clear_global_pauses()
    } else {
        pause::set_paused(true)
    };
    let master = pause::is_master_paused();
    if !master {
        pause::set_idle_auto_paused(false);
        pause::mark_voice_seen();
    }
    event::global_broadcaster().master_pause_changed(master);
    event::global_broadcaster().pause_scope_toggled("master", None, master);
    if changed {
        if effective {
            pause::dictation_buffer_clear();
            event::global_broadcaster().paused();
        } else {
            event::global_broadcaster().resumed();
        }
    }
    if let Ok(log_path) = always_config::configured_log_path()
        && let Ok(mut logger) = log::Logger::open(&log_path)
    {
        logger.write(log::Event::PauseToggled { paused: effective });
    }
    tracing::info!(
        master,
        effective,
        changed,
        scope = "master",
        "hotkey_pause_chord"
    );
}

/// Ask macOS for Input Monitoring (listen-event) access.
///
/// `rdev`'s listener is a *listen-only* `CGEventTap`, which macOS gates behind
/// the **Input Monitoring** permission (`kTCCServiceListenEvent`) — NOT
/// Accessibility. `rdev` only calls `CGEventTapCreate` and never requests this
/// access, so without an explicit request the tap silently receives no events
/// and "Always" never even appears in System Settings → Privacy & Security →
/// Input Monitoring. Calling `CGRequestListenEventAccess` triggers the one-time
/// system prompt and registers the responsible app in that list so the user can
/// enable it. It also self-heals after a bundle-id bump (which resets TCC
/// grants). Safe to call repeatedly — once granted it's a cheap status read.
#[cfg(feature = "macos")]
fn request_input_monitoring_access() {
    // Both symbols live in CoreGraphics (linked via the `core-graphics` crate)
    // and exist since macOS 10.15.
    unsafe extern "C" {
        fn CGPreflightListenEventAccess() -> bool;
        fn CGRequestListenEventAccess() -> bool;
    }
    let granted = unsafe {
        // Preflight first so we don't re-trigger the prompt once granted.
        CGPreflightListenEventAccess() || CGRequestListenEventAccess()
    };
    if granted {
        tracing::info!("input_monitoring_access_granted");
    } else {
        tracing::warn!(
            "input_monitoring_access_missing: global shortcuts are inert until \
             you enable Always in System Settings → Privacy & Security → \
             Input Monitoring"
        );
    }
}

#[cfg(feature = "macos")]
pub fn start_keyboard_listener() -> Result<()> {
    use rdev::{EventType, Key, listen};

    // Must run before the tap is created — see `request_input_monitoring_access`.
    request_input_monitoring_access();

    let (
        pause_combo,
        auto_enter_combo,
        force_paste_combo,
        log_correction_combo,
        correction_dialog_combo,
        master_pause_combo,
    ) = load_shortcuts();
    let (tx, _rx) = mpsc::channel();

    thread::spawn(move || {
        let mut ctrl_pressed = false;
        let mut shift_pressed = false;
        let mut alt_pressed = false;

        if let Err(error) = listen(move |event| match event.event_type {
            EventType::KeyPress(Key::ControlLeft) | EventType::KeyPress(Key::ControlRight) => {
                ctrl_pressed = true;
            }
            EventType::KeyRelease(Key::ControlLeft) | EventType::KeyRelease(Key::ControlRight) => {
                ctrl_pressed = false;
            }
            EventType::KeyPress(Key::ShiftLeft) | EventType::KeyPress(Key::ShiftRight) => {
                shift_pressed = true;
            }
            EventType::KeyRelease(Key::ShiftLeft) | EventType::KeyRelease(Key::ShiftRight) => {
                shift_pressed = false;
            }
            EventType::KeyPress(Key::Alt) | EventType::KeyPress(Key::AltGr) => {
                alt_pressed = true;
                OPTION_HELD_EVENT.store(true, Ordering::Relaxed);
            }
            EventType::KeyRelease(Key::Alt) | EventType::KeyRelease(Key::AltGr) => {
                alt_pressed = false;
                OPTION_HELD_EVENT.store(false, Ordering::Relaxed);
            }
            // ⌘ state is read live from the OS in `is_cmd_held` — we update
            // the event-based fallback tracking here and swallow the events
            // so they don't fall through to the general key handler and cancel
            // the auto-enter countdown.
            EventType::KeyPress(Key::MetaLeft) | EventType::KeyPress(Key::MetaRight) => {
                CMD_HELD_EVENT.store(true, Ordering::Relaxed);
            }
            EventType::KeyRelease(Key::MetaLeft) | EventType::KeyRelease(Key::MetaRight) => {
                CMD_HELD_EVENT.store(false, Ordering::Relaxed);
            }
            EventType::KeyPress(ref key) => {
                let Some(name) = key_to_shortcut_name(key) else {
                    // Even unknown keys cancel an in-flight countdown
                    // (e.g. Tab, arrow keys) — the user typed *something*
                    // and intended for that to take precedence over the
                    // auto-Return.
                    if pause::countdown_active() {
                        pause::countdown_request_cancel();
                        // User typed: drop dictation merge buffer — they've
                        // taken control of the field.
                        pause::dictation_buffer_clear();
                    }
                    return;
                };
                if pause::countdown_active() {
                    // Any keypress that isn't a pure modifier cancels
                    // the active auto-enter countdown. The user is
                    // typing — they don't want the synthesized Return.
                    pause::countdown_request_cancel();
                    pause::dictation_buffer_clear();
                }
                if master_pause_combo.matches_name(ctrl_pressed, shift_pressed, alt_pressed, name) {
                    handle_master_pause_hotkey();
                } else if pause_combo.matches_name(ctrl_pressed, shift_pressed, alt_pressed, name) {
                    // ⌃⌥P is strictly per-app: toggle the focused app's
                    // place on the resumed-allowlist. Master pause has
                    // its own chord (⌃⌥⇧P) — the old context-aware
                    // dispatch silently retargeted this chord to master
                    // whenever any watchdog (audio, idle, mic) had set
                    // master pause, which read as "the shortcut is
                    // broken" in the field.
                    match pause_chord_action(pause::current_app().as_deref()) {
                        ChordAction::TogglePerApp(bundle) => {
                            handle_per_app_pause_hotkey(&bundle);
                        }
                        ChordAction::NoFocusedApp => {
                            tracing::info!("hotkey_pause_chord_no_focused_app");
                            event::global_broadcaster().pause_scope_toggled(
                                "none",
                                None,
                                pause::is_paused(),
                            );
                        }
                    }
                } else if auto_enter_combo.matches_name(
                    ctrl_pressed,
                    shift_pressed,
                    alt_pressed,
                    name,
                ) {
                    let new_state = pause::toggle_auto_enter();
                    if new_state {
                        event::global_broadcaster().auto_enter_enabled();
                    } else {
                        event::global_broadcaster().auto_enter_disabled();
                    }
                    if let Ok(log_path) = always_config::configured_log_path()
                        && let Ok(mut logger) = log::Logger::open(&log_path)
                    {
                        logger.write(log::Event::AutoEnterToggled { enabled: new_state });
                    }
                } else if force_paste_combo.matches_name(
                    ctrl_pressed,
                    shift_pressed,
                    alt_pressed,
                    name,
                ) {
                    if let Some(text) = pause::take_last_filtered() {
                        let chars = text.len();
                        tracing::info!(chars, "force_paste_filtered");
                        if let Err(error) = paste::copy_to_clipboard(format!("{} ", text))
                            .and_then(|_| paste::paste_text(pause::is_auto_enter_enabled()))
                        {
                            tracing::error!(?error, "force_paste_failed");
                        } else {
                            event::global_broadcaster().transcript_final(text.clone());
                            if let Ok(log_path) = always_config::configured_log_path()
                                && let Ok(mut logger) = log::Logger::open(&log_path)
                            {
                                logger.write(log::Event::ForcePastedFiltered { text: &text });
                            }
                        }
                    } else {
                        tracing::debug!("force_paste_no_text");
                    }
                } else if log_correction_combo.matches_name(
                    ctrl_pressed,
                    shift_pressed,
                    alt_pressed,
                    name,
                ) {
                    // Active manual-correction capture path. The user
                    // selected text they just edited and pressed ⌃⌥X;
                    // the daemon snapshots that selection, diffs it
                    // against the freshest `last_pasted` (within
                    // `clipboard_watcher::PASTE_WINDOW`), and appends
                    // the extracted pairs to the glossary directly.
                    // Each pair fires a `CorrectionLogged` event so the
                    // GUI can surface confirmation toast / sound.
                    match correction::capture_via_hotkey(clipboard_watcher::PASTE_WINDOW) {
                        Ok(correction::CaptureOutcome::Applied { pairs, applied }) => {
                            for p in &pairs {
                                event::global_broadcaster()
                                    .correction_logged(p.wrong.clone(), p.right.clone());
                            }
                            event::global_broadcaster().correction_capture_result("applied");
                            let _ = applied; // count surfaced via per-pair CorrectionLogged events
                        }
                        Ok(correction::CaptureOutcome::NoRecentPaste) => {
                            tracing::debug!("log_correction_no_recent_paste");
                            event::global_broadcaster()
                                .correction_capture_result("no_recent_paste");
                        }
                        Ok(correction::CaptureOutcome::NoChange) => {
                            tracing::debug!("log_correction_no_change");
                            event::global_broadcaster().correction_capture_result("no_change");
                        }
                        Ok(correction::CaptureOutcome::NoCorrectionPairs) => {
                            tracing::debug!("log_correction_no_correction_pairs");
                            event::global_broadcaster()
                                .correction_capture_result("no_correction_pairs");
                        }
                        Err(error) => {
                            tracing::error!(?error, "log_correction_failed");
                            event::global_broadcaster().correction_capture_result("error");
                        }
                    }
                } else if correction_dialog_combo.matches_name(
                    ctrl_pressed,
                    shift_pressed,
                    alt_pressed,
                    name,
                ) {
                    // Dialog-driven correction: surface the last
                    // transcript to the Mac app, which then opens a
                    // sheet for the user to type the intended word.
                    // Selection on the screen is irrelevant here —
                    // the diff happens against `last_pasted` inside
                    // the daemon when `LogCorrection { intended }`
                    // comes back.
                    let last = pause::last_transcript_for_correction().unwrap_or_default();
                    event::global_broadcaster().correction_dialog_requested(last);
                }
            }
            _ => {
                // Non-key events (e.g. MouseMove, MouseClick) also
                // cancel an in-flight countdown — they reflect
                // intentional user input directed at the focused app,
                // not a quiescent "let auto-enter run" state.
                if pause::countdown_active()
                    && matches!(event.event_type, EventType::ButtonPress(_))
                {
                    pause::countdown_request_cancel();
                    pause::dictation_buffer_clear();
                }
            }
        }) {
            tracing::error!(?error, "keyboard_listener_error");
            let _ = tx.send(());
        }
    });

    thread::sleep(Duration::from_millis(100));
    Ok(())
}

// ----------------------------------------------------------------------
// Linux / Windows: stub
// ----------------------------------------------------------------------
#[cfg(not(feature = "macos"))]
pub fn start_keyboard_listener() -> Result<()> {
    tracing::warn!(
        "global keyboard shortcuts are not yet wired up on this platform; \
         use `always toggle pause` / `always toggle auto-enter` from the CLI"
    );
    Ok(())
}

// ----------------------------------------------------------------------
// Listener abstraction for DI
// ----------------------------------------------------------------------

/// Pluggable global keyboard listener.
///
/// Today the macOS implementation lives entirely in
/// [`start_keyboard_listener`] (rdev-driven). This trait carves out the
/// surface so:
///
/// 1. The `event_loop` can take a `Box<dyn KeyEventListener>` and unit-test
///    pause / auto-enter / force-paste behavior without touching the OS.
/// 2. A future P2.2 `CGEventTap` implementation (and Linux/Windows
///    implementations) can drop in without changing callers.
pub trait KeyEventListener: Send + Sync {
    /// Start listening for global keyboard shortcuts. Implementations
    /// typically spawn a background thread and return immediately.
    fn start(&self) -> Result<()>;
}

/// Production listener — wraps the existing platform-specific
/// [`start_keyboard_listener`] entry point.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlatformKeyEventListener;

impl KeyEventListener for PlatformKeyEventListener {
    fn start(&self) -> Result<()> {
        start_keyboard_listener()
    }
}

#[cfg(test)]
pub mod mock {
    //! In-memory test double for [`KeyEventListener`].

    use super::KeyEventListener;
    use anyhow::Result;
    use parking_lot::Mutex;
    use std::sync::Arc;

    #[derive(Debug, Default, Clone)]
    pub struct MockKeyEventListener {
        starts: Arc<Mutex<u32>>,
    }

    impl MockKeyEventListener {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn start_count(&self) -> u32 {
            *self.starts.lock()
        }
    }

    impl KeyEventListener for MockKeyEventListener {
        fn start(&self) -> Result<()> {
            *self.starts.lock() += 1;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Combo, default_shortcuts};

    #[test]
    fn parses_shortcut_with_modifiers_and_key() {
        let combo = Combo::from_str("ctrl+alt+p").expect("valid shortcut");
        assert!(combo.matches_name(true, false, true, "p"));
        assert!(!combo.matches_name(true, true, true, "p"));
        assert!(!combo.matches_name(true, false, true, "a"));
    }

    #[test]
    fn rejects_invalid_shortcuts() {
        assert!(Combo::from_str("p").is_none());
        assert!(Combo::from_str("ctrl+alt").is_none());
        assert!(Combo::from_str("ctrl+alt+return").is_none());
    }

    #[test]
    fn matches_supported_key_names() {
        let combo = Combo::from_str("ctrl+alt+space").unwrap();
        assert!(combo.matches_name(true, false, true, "space"));
        assert!(!combo.matches_name(true, false, true, "p"));
    }

    #[test]
    fn defaults_parse_cleanly() {
        let _ = default_shortcuts();
    }

    #[cfg(feature = "macos")]
    #[test]
    fn falls_back_to_defaults_for_invalid_configured_shortcuts() {
        let (pause, auto_enter, force_paste, log_correction, correction_dialog, master_pause) =
            super::resolve_shortcuts(
                Some("ctrl+alt+return"),
                Some("a"),
                Some("v"),
                Some("not a combo"),
                Some(""),
                Some("nope"),
            );
        assert!(pause.matches_name(true, false, true, "p"));
        assert!(auto_enter.matches_name(true, false, true, "a"));
        assert!(force_paste.matches_name(true, false, true, "v"));
        assert!(log_correction.matches_name(true, false, true, "x"));
        assert!(correction_dialog.matches_name(true, false, true, "w"));
        assert!(master_pause.matches_name(true, true, true, "p"));
    }

    #[cfg(feature = "macos")]
    #[test]
    fn uses_configured_shortcuts_when_valid() {
        let (pause, auto_enter, force_paste, log_correction, correction_dialog, master_pause) =
            super::resolve_shortcuts(
                Some("shift+x"),
                Some("ctrl+space"),
                Some("ctrl+shift+v"),
                Some("ctrl+alt+l"),
                Some("ctrl+alt+m"),
                Some("ctrl+shift+g"),
            );
        assert!(pause.matches_name(false, true, false, "x"));
        assert!(auto_enter.matches_name(true, false, false, "space"));
        assert!(force_paste.matches_name(true, true, false, "v"));
        assert!(log_correction.matches_name(true, false, true, "l"));
        assert!(correction_dialog.matches_name(true, false, true, "m"));
        assert!(master_pause.matches_name(true, true, false, "g"));
    }

    #[cfg(feature = "macos")]
    #[test]
    fn log_correction_default_is_ctrl_alt_x() {
        let (pause, auto_enter, force_paste, log_correction, correction_dialog, master_pause) =
            super::resolve_shortcuts(None, None, None, None, None, None);
        assert!(pause.matches_name(true, false, true, "p"));
        assert!(auto_enter.matches_name(true, false, true, "a"));
        assert!(force_paste.matches_name(true, false, true, "v"));
        assert!(log_correction.matches_name(true, false, true, "x"));
        assert!(correction_dialog.matches_name(true, false, true, "w"));
        assert!(master_pause.matches_name(true, true, true, "p"));
    }

    #[test]
    fn pause_chord_never_collides_with_master_chord() {
        // The defining regression: ⌃⌥P (no shift) must not match the
        // master chord, and ⌃⌥⇧P must not match the per-app chord.
        let (pause, _, _, _, _, master) = default_shortcuts();
        assert!(pause.matches_name(true, false, true, "p"));
        assert!(!pause.matches_name(true, true, true, "p"));
        assert!(master.matches_name(true, true, true, "p"));
        assert!(!master.matches_name(true, false, true, "p"));
    }

    #[test]
    fn chord_action_targets_focused_real_app() {
        use super::{ChordAction, pause_chord_action};
        assert_eq!(
            pause_chord_action(Some("com.apple.Safari")),
            ChordAction::TogglePerApp("com.apple.Safari".to_string())
        );
    }

    #[test]
    fn chord_action_refuses_own_bundles_and_none() {
        use super::{ChordAction, pause_chord_action};
        assert_eq!(pause_chord_action(None), ChordAction::NoFocusedApp);
        for own in crate::always::per_app::ALWAYS_OWN_BUNDLE_IDS {
            assert_eq!(pause_chord_action(Some(own)), ChordAction::NoFocusedApp);
        }
    }
}
