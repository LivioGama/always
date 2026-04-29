//! Keyboard shortcut handler for global pause toggle and auto-enter toggle.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use rdev::{listen, EventType, Key};

use super::{config as always_config, log, notification, pause};

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
                EventType::KeyPress(Key::KeyP) => {
                    if ctrl_pressed && shift_pressed {
                        let new_state = pause::toggle_pause();

                        // Update state file
                        if let Err(e) = crate::always::state::DaemonState::set_paused(new_state) {
                            eprintln!("Failed to update paused state: {}", e);
                        }

                        // Log the pause toggle
                        if let Ok(log_path) = always_config::configured_log_path() {
                            if let Ok(mut logger) = log::Logger::open(&log_path) {
                                logger.write(log::Event::PauseToggled { paused: new_state });
                            }
                        }

                        // Show visual notification
                        if let Err(e) = notification::show_pause_status_change(new_state) {
                            eprintln!("Failed to show pause notification: {}", e);
                        }
                    }
                }
                EventType::KeyPress(Key::KeyA) => {
                    if ctrl_pressed && shift_pressed {
                        let new_state = pause::toggle_auto_enter();

                        // Update state file
                        if let Err(e) = crate::always::state::DaemonState::set_auto_enter(new_state) {
                            eprintln!("Failed to update auto_enter state: {}", e);
                        }

                        // Log the auto-enter toggle
                        if let Ok(log_path) = always_config::configured_log_path() {
                            if let Ok(mut logger) = log::Logger::open(&log_path) {
                                logger.write(log::Event::AutoEnterToggled { enabled: new_state });
                            }
                        }

                        // Show visual notification
                        if let Err(e) = notification::show_auto_enter_status_change(new_state) {
                            eprintln!("Failed to show auto-enter notification: {}", e);
                        }
                    }
                }
                _ => {}
            }
        }) {
            eprintln!("Error in keyboard listener: {:?}", error);
            let _ = tx.send(());
        }
    });

    // Give the keyboard listener a moment to start
    thread::sleep(Duration::from_millis(100));

    Ok(())
}
