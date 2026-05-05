//! Global keyboard shortcut handler — pause toggle, auto-enter toggle, force-paste.
//!
//! Default shortcuts: ⌃⌥P (pause), ⌃⌥A (auto-enter), ⌃⌥V (paste-anyway last filtered).
//! All configurable via `always config set shortcut_pause ctrl+alt+p`
//! (takes effect on daemon restart).
//!
//! ## Cross-platform layout
//!
//! [`Combo`] (parsing + matching) is portable — modifier flags + a single
//! key character string. The actual global hotkey listener that converts
//! OS key events into [`Combo::matches`] calls is platform-specific:
//!
//! * **macOS** (`feature = "macos"`): uses `rdev` to subscribe to system
//!   keyboard events. (P2 will replace `rdev` with a thin `CGEventTap`
//!   wrapper to drop the unmaintained dep.)
//! * **Linux / Windows**: stub — `start_keyboard_listener` returns `Ok(())`
//!   without registering anything. Voice activation still works; users
//!   must toggle pause/auto-enter via the CLI for now.

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

static CMD_HELD: std::sync::LazyLock<AtomicBool> =
    std::sync::LazyLock::new(|| AtomicBool::new(false));

pub fn is_cmd_held() -> bool {
    CMD_HELD.load(Ordering::Relaxed)
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
fn default_shortcuts() -> (Combo, Combo, Combo, Combo) {
    (
        Combo::from_str("ctrl+alt+p").expect("default pause shortcut is valid"),
        Combo::from_str("ctrl+alt+a").expect("default auto-enter shortcut is valid"),
        Combo::from_str("ctrl+alt+v").expect("default force-paste shortcut is valid"),
        // ⌃⌥X is the hotkey-driven correction capture: read the user's
        // current selection, diff it against the most recent paste, and
        // append the resulting `(wrong → right)` pairs to the glossary.
        // Default chosen so it doesn't collide with the existing trio
        // and stays inside the "ctrl+alt+<letter>" pattern users already
        // know.
        Combo::from_str("ctrl+alt+x").expect("default log-correction shortcut is valid"),
    )
}

#[cfg(feature = "macos")]
fn resolve_shortcuts(
    pause: Option<&str>,
    auto_enter: Option<&str>,
    force_paste: Option<&str>,
    log_correction: Option<&str>,
) -> (Combo, Combo, Combo, Combo) {
    let (default_pause, default_auto_enter, default_force_paste, default_log_correction) =
        default_shortcuts();
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

    (
        pause_combo,
        auto_enter_combo,
        force_paste_combo,
        log_correction_combo,
    )
}

#[cfg(feature = "macos")]
fn load_shortcuts() -> (Combo, Combo, Combo, Combo) {
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

#[cfg(feature = "macos")]
pub fn start_keyboard_listener() -> Result<()> {
    use rdev::{EventType, Key, listen};

    let (pause_combo, auto_enter_combo, force_paste_combo, log_correction_combo) = load_shortcuts();
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
            }
            EventType::KeyRelease(Key::Alt) | EventType::KeyRelease(Key::AltGr) => {
                alt_pressed = false;
            }
            EventType::KeyPress(Key::MetaLeft) | EventType::KeyPress(Key::MetaRight) => {
                CMD_HELD.store(true, Ordering::Relaxed);
            }
            EventType::KeyRelease(Key::MetaLeft) | EventType::KeyRelease(Key::MetaRight) => {
                CMD_HELD.store(false, Ordering::Relaxed);
            }
            EventType::KeyPress(ref key) => {
                let Some(name) = key_to_shortcut_name(key) else {
                    return;
                };
                if pause_combo.matches_name(ctrl_pressed, shift_pressed, alt_pressed, name) {
                    let new_state = pause::toggle_pause();
                    if new_state {
                        event::global_broadcaster().paused();
                    } else {
                        event::global_broadcaster().resumed();
                    }
                    if let Ok(log_path) = always_config::configured_log_path()
                        && let Ok(mut logger) = log::Logger::open(&log_path)
                    {
                        logger.write(log::Event::PauseToggled { paused: new_state });
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
                            event::global_broadcaster()
                                .correction_capture_result("applied");
                            let _ = applied; // count surfaced via per-pair CorrectionLogged events
                        }
                        Ok(correction::CaptureOutcome::NoRecentPaste) => {
                            tracing::debug!("log_correction_no_recent_paste");
                            event::global_broadcaster()
                                .correction_capture_result("no_recent_paste");
                        }
                        Ok(correction::CaptureOutcome::NoChange) => {
                            tracing::debug!("log_correction_no_change");
                            event::global_broadcaster()
                                .correction_capture_result("no_change");
                        }
                        Ok(correction::CaptureOutcome::NoCorrectionPairs) => {
                            tracing::debug!("log_correction_no_correction_pairs");
                            event::global_broadcaster()
                                .correction_capture_result("no_correction_pairs");
                        }
                        Err(error) => {
                            tracing::error!(?error, "log_correction_failed");
                            event::global_broadcaster()
                                .correction_capture_result("error");
                        }
                    }
                }
            }
            _ => {}
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
        // Each invalid string falls through to the corresponding
        // hard-coded default. We feed one bogus value per slot to make
        // sure the per-shortcut fallback is wired independently — a
        // single shared default would silently mask a regression where
        // e.g. force-paste's combo overrode log-correction's.
        let (pause, auto_enter, force_paste, log_correction) = super::resolve_shortcuts(
            Some("ctrl+alt+return"),
            Some("a"),
            Some("v"),
            Some("not a combo"),
        );
        assert!(pause.matches_name(true, false, true, "p"));
        assert!(auto_enter.matches_name(true, false, true, "a"));
        assert!(force_paste.matches_name(true, false, true, "v"));
        assert!(log_correction.matches_name(true, false, true, "x"));
    }

    #[cfg(feature = "macos")]
    #[test]
    fn uses_configured_shortcuts_when_valid() {
        let (pause, auto_enter, force_paste, log_correction) = super::resolve_shortcuts(
            Some("shift+x"),
            Some("ctrl+space"),
            Some("ctrl+shift+v"),
            Some("ctrl+alt+l"),
        );
        assert!(pause.matches_name(false, true, false, "x"));
        assert!(auto_enter.matches_name(true, false, false, "space"));
        assert!(force_paste.matches_name(true, true, false, "v"));
        assert!(log_correction.matches_name(true, false, true, "l"));
    }

    #[cfg(feature = "macos")]
    #[test]
    fn log_correction_default_is_ctrl_alt_x() {
        // Sanity: nothing configured → all four slots resolve to their
        // documented defaults. Guards against silent default drift.
        let (pause, auto_enter, force_paste, log_correction) =
            super::resolve_shortcuts(None, None, None, None);
        assert!(pause.matches_name(true, false, true, "p"));
        assert!(auto_enter.matches_name(true, false, true, "a"));
        assert!(force_paste.matches_name(true, false, true, "v"));
        assert!(log_correction.matches_name(true, false, true, "x"));
    }
}
