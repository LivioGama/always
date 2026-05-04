//! Pause state and auto-enter state management for the always-on mode.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

/// Shared pause state that can be toggled from anywhere
pub struct PauseState {
    paused: AtomicBool,
}

impl PauseState {
    pub fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn toggle(&self) -> bool {
        let old_value = self.paused.load(Ordering::Relaxed);
        let new_value = !old_value;
        self.paused.store(new_value, Ordering::Relaxed);
        new_value
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }
}

impl Default for PauseState {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared auto-enter state that can be toggled from anywhere
pub struct AutoEnterState {
    auto_enter: AtomicBool,
}

impl AutoEnterState {
    pub fn new(initial: bool) -> Self {
        Self {
            auto_enter: AtomicBool::new(initial),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.auto_enter.load(Ordering::Relaxed)
    }

    pub fn toggle(&self) -> bool {
        let old_value = self.auto_enter.load(Ordering::Relaxed);
        let new_value = !old_value;
        self.auto_enter.store(new_value, Ordering::Relaxed);
        new_value
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.auto_enter.store(enabled, Ordering::Relaxed);
    }
}

/// Global pause state instance
static PAUSE_STATE: std::sync::LazyLock<Arc<PauseState>> =
    std::sync::LazyLock::new(|| Arc::new(PauseState::new()));

/// Global auto-enter state instance
static AUTO_ENTER_STATE: std::sync::LazyLock<Arc<AutoEnterState>> =
    std::sync::LazyLock::new(|| Arc::new(AutoEnterState::new(false)));

/// Initialize the auto-enter state with the config value
pub fn init_auto_enter(initial_value: bool) {
    AUTO_ENTER_STATE.set_enabled(initial_value);
}

/// Get the global pause state
pub fn global_pause_state() -> Arc<PauseState> {
    Arc::clone(&PAUSE_STATE)
}

/// Get the global auto-enter state
pub fn global_auto_enter_state() -> Arc<AutoEnterState> {
    Arc::clone(&AUTO_ENTER_STATE)
}

/// Check if the system is currently paused
pub fn is_paused() -> bool {
    PAUSE_STATE.is_paused()
}

/// Toggle the pause state and return the new state
pub fn toggle_pause() -> bool {
    PAUSE_STATE.toggle()
}

/// Set the pause state
pub fn set_paused(paused: bool) {
    PAUSE_STATE.set_paused(paused);
}

/// Check if auto-enter is currently enabled
pub fn is_auto_enter_enabled() -> bool {
    AUTO_ENTER_STATE.is_enabled()
}

/// Toggle the auto-enter state and return the new state
pub fn toggle_auto_enter() -> bool {
    AUTO_ENTER_STATE.toggle()
}

/// Set the auto-enter state
pub fn set_auto_enter_enabled(enabled: bool) {
    AUTO_ENTER_STATE.set_enabled(enabled);
}

/// Last filtered transcript — single-slot buffer for the "paste anyway" shortcut.
/// Set every time a transcription is rejected (filter or hallucination); cleared by `take`.
static LAST_FILTERED: std::sync::LazyLock<Mutex<Option<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

pub fn set_last_filtered(text: impl Into<String>) {
    *LAST_FILTERED.lock() = Some(text.into());
}

pub fn take_last_filtered() -> Option<String> {
    LAST_FILTERED.lock().take()
}

#[cfg(test)]
pub fn clear_last_filtered_for_test() {
    *LAST_FILTERED.lock() = None;
}

/// Last successfully-pasted transcript + the moment it was pasted.
///
/// Captured by `event_loop::handle_speech` immediately after the daemon
/// commits a paste to the user's foreground app. Read by:
///
/// 1. The `⌃⌥X` correction-capture hotkey (`correction::capture_via_hotkey`)
///    to diff the user's current selection against what we pasted.
/// 2. The passive clipboard watcher (`clipboard_watcher`) to recognize a
///    user-corrected re-copy of recently-pasted text.
///
/// Only the most recent paste is retained; an utterance entering this
/// slot evicts the previous one. Reads use [`take_last_pasted_within`]
/// to enforce a freshness window so an unrelated selection captured
/// minutes after the original paste isn't mistaken for a correction.
static LAST_PASTED: std::sync::LazyLock<Mutex<Option<(String, std::time::Instant)>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

/// Record the text we just pasted. Called from `event_loop::handle_speech`
/// after `paste::paste_text` succeeds.
pub fn set_last_pasted(text: impl Into<String>) {
    *LAST_PASTED.lock() = Some((text.into(), std::time::Instant::now()));
}

/// Read the most recent paste if it's still within `window` seconds.
/// Does NOT clear the slot — multiple consumers (hotkey + watcher) may
/// inspect it. Returns `(text, paste_instant)`.
pub fn take_last_pasted_within(
    window: std::time::Duration,
) -> Option<(String, std::time::Instant)> {
    let guard = LAST_PASTED.lock();
    let (text, ts) = guard.as_ref()?;
    if ts.elapsed() <= window {
        Some((text.clone(), *ts))
    } else {
        None
    }
}

/// Test helper: clear the slot so tests don't leak state between cases.
#[cfg(test)]
pub fn clear_last_pasted_for_test() {
    *LAST_PASTED.lock() = None;
}
