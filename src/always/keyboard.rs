//! Global keyboard shortcut handler — pause toggle, auto-enter toggle, force-paste.
//!
//! Default shortcuts: ⌃⌥P (pause), ⌃⌥A (auto-enter), ⌃⌥V (paste-anyway last filtered).
//! All configurable via `always config set shortcut_pause ctrl+alt+p`
//! (takes effect on daemon restart).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use rdev::{EventType, Key, listen};

use super::{config as always_config, event, log, paste, pause};

static CMD_HELD: std::sync::LazyLock<AtomicBool> =
    std::sync::LazyLock::new(|| AtomicBool::new(false));

pub fn is_cmd_held() -> bool {
    CMD_HELD.load(Ordering::Relaxed)
}

/// A parsed keyboard combo: modifier flags + a single character key.
#[derive(Clone, Debug)]
struct Combo {
    ctrl: bool,
    shift: bool,
    alt: bool,
    key_char: String,
}

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

    fn matches(&self, ctrl: bool, shift: bool, alt: bool, key: &Key) -> bool {
        self.ctrl == ctrl
            && self.shift == shift
            && self.alt == alt
            && key_char_matches(key, &self.key_char)
    }
}

fn key_char_matches(key: &Key, s: &str) -> bool {
    key_to_shortcut_name(key).is_some_and(|key_name| key_name == s)
}

fn key_to_shortcut_name(key: &Key) -> Option<&'static str> {
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

fn default_shortcuts() -> (Combo, Combo, Combo) {
    (
        Combo::from_str("ctrl+alt+p").expect("default pause shortcut is valid"),
        Combo::from_str("ctrl+alt+a").expect("default auto-enter shortcut is valid"),
        Combo::from_str("ctrl+alt+v").expect("default force-paste shortcut is valid"),
    )
}

fn resolve_shortcuts(
    pause: Option<&str>,
    auto_enter: Option<&str>,
    force_paste: Option<&str>,
) -> (Combo, Combo, Combo) {
    let (default_pause, default_auto_enter, default_force_paste) = default_shortcuts();
    let pause_combo = pause.and_then(Combo::from_str).unwrap_or(default_pause);
    let auto_enter_combo = auto_enter
        .and_then(Combo::from_str)
        .unwrap_or(default_auto_enter);
    let force_paste_combo = force_paste
        .and_then(Combo::from_str)
        .unwrap_or(default_force_paste);

    (pause_combo, auto_enter_combo, force_paste_combo)
}

fn load_shortcuts() -> (Combo, Combo, Combo) {
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
    )
}

pub fn start_keyboard_listener() -> Result<()> {
    let (pause_combo, auto_enter_combo, force_paste_combo) = load_shortcuts();
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
                if pause_combo.matches(ctrl_pressed, shift_pressed, alt_pressed, key) {
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
                } else if auto_enter_combo.matches(ctrl_pressed, shift_pressed, alt_pressed, key) {
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
                } else if force_paste_combo.matches(ctrl_pressed, shift_pressed, alt_pressed, key) {
                    if let Some(text) = pause::take_last_filtered() {
                        let chars = text.len();
                        tracing::info!(chars, "force_paste_filtered");
                        if let Err(error) =
                            paste::copy_to_clipboard(format!("{} ", text)).and_then(|_| {
                                paste::paste_text(pause::is_auto_enter_enabled())
                            })
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

#[cfg(test)]
mod tests {
    use super::{Combo, key_char_matches, resolve_shortcuts};
    use rdev::Key;

    #[test]
    fn parses_shortcut_with_modifiers_and_key() {
        let combo = Combo::from_str("ctrl+alt+p").expect("valid shortcut");

        assert!(combo.matches(true, false, true, &Key::KeyP));
        assert!(!combo.matches(true, true, true, &Key::KeyP));
        assert!(!combo.matches(true, false, true, &Key::KeyA));
    }

    #[test]
    fn rejects_invalid_shortcuts() {
        assert!(Combo::from_str("p").is_none());
        assert!(Combo::from_str("ctrl+alt").is_none());
        assert!(Combo::from_str("ctrl+alt+return").is_none());
    }

    #[test]
    fn matches_supported_key_names() {
        assert!(key_char_matches(&Key::KeyA, "a"));
        assert!(key_char_matches(&Key::Space, "space"));
        assert!(!key_char_matches(&Key::KeyA, "space"));
        assert!(!key_char_matches(&Key::Return, "return"));
    }

    #[test]
    fn falls_back_to_defaults_for_invalid_configured_shortcuts() {
        let (pause, auto_enter, force_paste) =
            resolve_shortcuts(Some("ctrl+alt+return"), Some("a"), Some("v"));

        assert!(pause.matches(true, false, true, &Key::KeyP));
        assert!(auto_enter.matches(true, false, true, &Key::KeyA));
        assert!(force_paste.matches(true, false, true, &Key::KeyV));
    }

    #[test]
    fn uses_configured_shortcuts_when_valid() {
        let (pause, auto_enter, force_paste) =
            resolve_shortcuts(Some("shift+x"), Some("ctrl+space"), Some("ctrl+shift+v"));

        assert!(pause.matches(false, true, false, &Key::KeyX));
        assert!(auto_enter.matches(true, false, false, &Key::Space));
        assert!(force_paste.matches(true, true, false, &Key::KeyV));
    }
}
