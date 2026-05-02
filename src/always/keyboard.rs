//! Keyboard shortcut handler for global pause toggle and auto-enter toggle.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use rdev::{listen, EventType, Key};

use super::{config as always_config, event, log, pause};

/// Tracks whether the Command (⌘) key is currently held.
/// Used by the paste path to suppress keystrokes when the user is mid-shortcut
/// (e.g. ⌘+Tab) so an interjecting paste doesn't corrupt their action.
static CMD_HELD: std::sync::LazyLock<AtomicBool> =
    std::sync::LazyLock::new(|| AtomicBool::new(false));

/// Returns true if either Command key is currently held.
pub fn is_cmd_held() -> bool {
    CMD_HELD.load(Ordering::Relaxed)
}

/// Start listening for keyboard shortcuts:
/// - Ctrl+Shift+P: Toggle pause/resume
/// - Ctrl+Shift+A: Toggle auto-enter
pub fn start_keyboard_listener() -> Result<()> {
    let (tx, _rx) = mpsc::channel();

    // Spawn the keyboard listener thread
    thread::spawn(move || {
        let mut ctrl_pressed = false;
        let mut shift_pressed = false;

        if let Err(error) = listen(move |event| {
            match event.event_type {
                EventType::KeyPress(Key::ControlLeft) | EventType::KeyPress(Key::ControlRight) => {
                    ctrl_pressed = true;
                }
                EventType::KeyRelease(Key::ControlLeft)
                | EventType::KeyRelease(Key::ControlRight) => {
                    ctrl_pressed = false;
                }
                EventType::KeyPress(Key::ShiftLeft) | EventType::KeyPress(Key::ShiftRight) => {
                    shift_pressed = true;
                }
                EventType::KeyRelease(Key::ShiftLeft) | EventType::KeyRelease(Key::ShiftRight) => {
                    shift_pressed = false;
                }
                EventType::KeyPress(Key::MetaLeft) | EventType::KeyPress(Key::MetaRight) => {
                    CMD_HELD.store(true, Ordering::Relaxed);
                }
                EventType::KeyRelease(Key::MetaLeft) | EventType::KeyRelease(Key::MetaRight) => {
                    CMD_HELD.store(false, Ordering::Relaxed);
                }
                EventType::KeyPress(Key::KeyP) => {
                    if ctrl_pressed && shift_pressed {
                        let new_state = pause::toggle_pause();

                        // Send event instead of writing to file
                        if new_state {
                            event::global_broadcaster().paused();
                        } else {
                            event::global_broadcaster().resumed();
                        }

                        // Log the pause toggle
                        if let Ok(log_path) = always_config::configured_log_path() {
                            if let Ok(mut logger) = log::Logger::open(&log_path) {
                                logger.write(log::Event::PauseToggled { paused: new_state });
                            }
                        }
                    }
                }
                EventType::KeyPress(Key::KeyA) => {
                    if ctrl_pressed && shift_pressed {
                        let new_state = pause::toggle_auto_enter();

                        // Send event instead of writing to file
                        if new_state {
                            event::global_broadcaster().auto_enter_enabled();
                        } else {
                            event::global_broadcaster().auto_enter_disabled();
                        }

                        // Log the auto-enter toggle
                        if let Ok(log_path) = always_config::configured_log_path() {
                            if let Ok(mut logger) = log::Logger::open(&log_path) {
                                logger.write(log::Event::AutoEnterToggled { enabled: new_state });
                            }
                        }
                    }
                }
                _ => {}
            }
        }) {
            tracing::error!(?error, "keyboard_listener_error");
            let _ = tx.send(());
        }
    });

    // Give the keyboard listener a moment to start
    thread::sleep(Duration::from_millis(100));

    Ok(())
}
