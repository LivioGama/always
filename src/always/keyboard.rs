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
    /// Requires at least one modifier and exactly one key character,
    /// OR a standalone `"fn"` (the Fn/Globe key on macOS keyboards,
    /// which fires no ctrl/shift/alt flags and is the only modifier-less
    /// shortcut we accept).
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
        // `"fn"` is the only modifier-less shortcut we accept — the Fn
        // key on Apple keyboards fires as a flagsChanged event with no
        // ctrl/shift/alt, so the standard "must have a modifier" guard
        // would reject it. The Fn listener is a separate CGEventTap
        // (see `start_fn_listener`), not rdev.
        if key_char == "fn" {
            return Some(Combo {
                ctrl: false,
                shift: false,
                alt: false,
                key_char,
            });
        }
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
        Key::Function => Some("fn"),
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
/// Tri-state Input Monitoring status for the daemon's event tap:
/// 0 = unknown (listener not started yet), 1 = granted, 2 = denied.
/// Read by `uds_server` so every connecting client gets the status in
/// its initial state burst (a startup broadcast would be lost — no
/// clients are connected yet when the listener starts).
static INPUT_MONITORING_STATUS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn input_monitoring_status() -> Option<bool> {
    match INPUT_MONITORING_STATUS.load(std::sync::atomic::Ordering::Relaxed) {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

#[cfg(feature = "macos")]
fn set_input_monitoring_status(granted: bool) {
    INPUT_MONITORING_STATUS.store(
        if granted { 1 } else { 2 },
        std::sync::atomic::Ordering::Relaxed,
    );
    event::global_broadcaster().shortcut_listener_status(granted);
}

#[cfg(feature = "macos")]
fn request_input_monitoring_access() {
    // Preflight ONLY (cheap status read, never prompts). The GUI is the
    // sole requester: it calls IOHIDRequestAccess, which reliably surfaces
    // "Always" in System Settings on macOS 26 — the daemon's own
    // CGRequestListenEventAccess did NOT (faceless helper), so that call
    // was removed. The status is stored + broadcast so the GUI banner can
    // show "shortcuts inactive" instead of leaving hotkeys silently dead.
    unsafe extern "C" {
        fn CGPreflightListenEventAccess() -> bool;
    }
    let granted = unsafe { CGPreflightListenEventAccess() };
    set_input_monitoring_status(granted);
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

    let master_pause_is_fn = master_pause_combo.key_char == "fn";

    // rdev's listen() creates a CGEventTap that macOS disables after
    // ~10-15s (TapDisabledByTimeout). rdev's callback never handles
    // this event type, so the tap stays dead and all shortcuts stop.
    // We wrap listen() in a restart loop with a watchdog that stops
    // the run loop after 10s, forcing listen() to return so we can
    // recreate the tap. This is not a startup retry — the tap starts
    // instantly and restarts with no user-visible delay.
    thread::spawn(move || {
        let mut ctrl_pressed = false;
        let mut shift_pressed = false;
        let mut alt_pressed = false;

        let result = listen(move |event| {
                if matches!(event.event_type, EventType::KeyPress(_) | EventType::KeyRelease(_)) {
                    tracing::info!(?event.event_type, "rdev_event");
                }
                match event.event_type {
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
                EventType::KeyPress(Key::MetaLeft) | EventType::KeyPress(Key::MetaRight) => {
                    CMD_HELD_EVENT.store(true, Ordering::Relaxed);
                }
                EventType::KeyRelease(Key::MetaLeft) | EventType::KeyRelease(Key::MetaRight) => {
                    CMD_HELD_EVENT.store(false, Ordering::Relaxed);
                }
                EventType::KeyPress(Key::Function) => {
                    // Fn key — rdev does see it as KeyPress(Function)
                    // on some macOS versions.
                    tracing::info!("fn_key_pressed");
                    handle_master_pause_hotkey();
                }
                EventType::KeyPress(ref key) => {
                    let Some(name) = key_to_shortcut_name(key) else {
                        if pause::countdown_active() {
                            pause::countdown_request_cancel();
                            pause::dictation_buffer_clear();
                        }
                        return;
                    };
                    if pause::countdown_active() {
                        pause::countdown_request_cancel();
                        pause::dictation_buffer_clear();
                    }
                    if master_pause_combo.matches_name(ctrl_pressed, shift_pressed, alt_pressed, name) {
                        handle_master_pause_hotkey();
                    } else if pause_combo.matches_name(ctrl_pressed, shift_pressed, alt_pressed, name) {
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
                        ctrl_pressed, shift_pressed, alt_pressed, name,
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
                        ctrl_pressed, shift_pressed, alt_pressed, name,
                    ) {
                        if let Some(text) = pause::take_last_filtered() {
                            let chars = text.len();
                            tracing::info!(chars, "force_paste_filtered");
                            if let Err(error) = paste::copy_to_clipboard(format!("{} ", text))
                                .and_then(|_| paste::paste_text(pause::is_auto_enter_enabled()))
                            {
                                tracing::error!(?error, "force_paste_failed");
                                pause::set_last_filtered(text);
                                event::global_broadcaster()
                                    .transcription_filtered("Force-paste failed — try again");
                            } else {
                                event::global_broadcaster().transcript_final(text.clone());
                                if let Ok(log_path) = always_config::configured_log_path()
                                    && let Ok(mut logger) = log::Logger::open(&log_path)
                                {
                                    logger.write(log::Event::ForcePastedFiltered { text: &text });
                                }
                            }
                        } else {
                            tracing::info!("force_paste_no_text");
                            event::global_broadcaster()
                                .transcription_filtered("Nothing to paste — no held transcript");
                        }
                    } else if log_correction_combo.matches_name(
                        ctrl_pressed, shift_pressed, alt_pressed, name,
                    ) {
                        match correction::capture_via_hotkey(clipboard_watcher::PASTE_WINDOW) {
                            Ok(correction::CaptureOutcome::Applied { pairs, applied }) => {
                                for p in &pairs {
                                    event::global_broadcaster()
                                        .correction_logged(p.wrong.clone(), p.right.clone());
                                }
                                event::global_broadcaster().correction_capture_result("applied");
                                let _ = applied;
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
                        ctrl_pressed, shift_pressed, alt_pressed, name,
                    ) {
                        let last = pause::last_transcript_for_correction().unwrap_or_default();
                        event::global_broadcaster().correction_dialog_requested(last);
                    }
                }
                _ => {
                    if pause::countdown_active()
                        && matches!(event.event_type, EventType::ButtonPress(_))
                    {
                        pause::countdown_request_cancel();
                        pause::dictation_buffer_clear();
                    }
                }
            }
            });

        if let Err(error) = result {
            tracing::error!(?error, "keyboard_listener_error");
            set_input_monitoring_status(false);
        } else {
            tracing::info!("keyboard_listener_started");
            set_input_monitoring_status(true);
        }
    });

    thread::sleep(Duration::from_millis(100));

    // Start the Fn key listener alongside rdev. rdev's CGEventTap only
    // forwards keyDown/keyUp events — it drops flagsChanged, which is how
    // macOS delivers the Fn/Globe key. This separate tap catches that.
    if master_pause_is_fn {
        start_fn_listener();
    }

    Ok(())
}

/// Start a CGEventTap that listens for `flagsChanged` events to detect
/// the Fn/Globe key (keycode 63). The Fn key on macOS keyboards fires as
/// a modifier-flag change, not a keyDown — `rdev` never sees it. This
/// mirrors the approach from iris-sama's `shortcut-events.swift`.
#[cfg(feature = "macos")]
fn start_fn_listener() {
    use std::os::raw::{c_int, c_void};
    use std::sync::atomic::AtomicPtr;

    const FN_FLAG: u64 = 0x800000;
    const FN_KEYCODE: i64 = 63;

    static FN_PREVIOUSLY_HELD: AtomicBool = AtomicBool::new(false);
    static FN_TAP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

    unsafe extern "C" {
        fn CGEventTapCreate(
            tap: u32, place: u32, options: u32, events: u64,
            cb: unsafe extern "C" fn(*mut c_void, u32, *mut c_void, *mut c_void) -> *mut c_void,
            info: *mut c_void,
        ) -> *mut c_void;
        fn CGEventTapEnable(tap: *mut c_void, enable: bool);
        fn CGEventGetIntegerValueField(event: *mut c_void, field: u32) -> i64;
        fn CGEventGetFlags(event: *mut c_void) -> u64;
        fn CFMachPortCreateRunLoopSource(alloc: *mut c_void, port: *mut c_void, order: c_int) -> *mut c_void;
        fn CFRunLoopAddSource(rl: *mut c_void, src: *mut c_void, mode: *const c_void);
        fn CFRunLoopGetCurrent() -> *mut c_void;
        fn CFRunLoopRun();
        fn CFRelease(cf: *mut c_void);
        fn CFStringCreateWithCString(alloc: *mut c_void, cs: *const u8, enc: u32) -> *mut c_void;
    }

    const FLAGS_CHANGED: u64 = 1 << 12;

    unsafe extern "C" fn fn_tap_callback(
        _proxy: *mut c_void,
        type_: u32,
        event: *mut c_void,
        _user_info: *mut c_void,
    ) -> *mut c_void {
        // kCGEventTapDisabledByTimeout = 0xFFFFFFFE
        // kCGEventTapDisabledByUserInput = 0xFFFFFFFF
        if type_ == 0xFFFFFFFE || type_ == 0xFFFFFFFF {
            let tap = FN_TAP.load(Ordering::Relaxed);
            if !tap.is_null() {
                CGEventTapEnable(tap, true);
            }
            return event;
        }

        if type_ != 12 {
            return event;
        }
        let keycode = CGEventGetIntegerValueField(event, 9);
        let flags = CGEventGetFlags(event);

        if keycode == FN_KEYCODE {
            let fn_held = (flags & FN_FLAG) != 0;
            let was_held = FN_PREVIOUSLY_HELD.swap(fn_held, Ordering::Relaxed);
            if fn_held && !was_held {
                tracing::info!("fn_key_pressed");
                handle_master_pause_hotkey();
            }
        }
        event
    }

    thread::spawn(move || {
        let mut restart_count = 0u32;
        loop {
            let tap = unsafe {
                CGEventTapCreate(
                    0, // kCGHIDEventTap
                    0, // kCGHeadInsertEventTap
                    1, // kCGEventTapListenOption
                    FLAGS_CHANGED,
                    fn_tap_callback,
                    std::ptr::null_mut(),
                )
            };

            if tap.is_null() {
                tracing::warn!("fn_listener_tap_failed");
                set_input_monitoring_status(false);
                break;
            }

            FN_TAP.store(tap, Ordering::Relaxed);

            unsafe {
                let source = CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
                if source.is_null() {
                    tracing::error!("fn_listener_runloop_source_failed");
                    set_input_monitoring_status(false);
                    CFRelease(tap);
                    break;
                }
                let rl = CFRunLoopGetCurrent();
                let mode_str = b"kCFRunLoopCommonModes\0";
                let mode = CFStringCreateWithCString(
                    std::ptr::null_mut(),
                    mode_str.as_ptr(),
                    0x08000100,
                );
                CFRunLoopAddSource(rl, source, mode as *const c_void);
                CGEventTapEnable(tap, true);
                if restart_count == 0 {
                    tracing::info!("fn_listener_started");
                }
                restart_count += 1;
                CFRunLoopRun();
                // Run loop exited — tap was disabled by timeout.
                // Release and recreate.
                CFRelease(source);
                CFRelease(tap);
                CFRelease(mode);
                tracing::debug!(restart_count, "fn_listener_restarting");
            }
        }
    });
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
    fn parses_fn_only_shortcut() {
        let combo = Combo::from_str("fn").expect("fn is a valid modifier-less shortcut");
        assert!(combo.matches_name(false, false, false, "fn"));
        assert!(!combo.matches_name(true, false, false, "fn"));
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
