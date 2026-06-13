//! Pause state and auto-enter state management for the always-on mode.
//!
//! Two-tier pause model (changed 2026-05-17 — allowlist UX):
//!
//! - **MASTER_PAUSED** — the user's explicit global force-pause switch.
//!   Set by `TogglePause`, mic-conflict, and audio-output monitors. When
//!   true, EFFECTIVE is forced to true regardless of per-app overrides.
//! - **IDLE_AUTO_PAUSED** — watchdog fired after `idle_pause_secs` with no
//!   voice. Does *not* flip MASTER so focus changes and wake-on-voice can
//!   resume allowlisted apps without a global "lift pause".
//! - **EFFECTIVE_PAUSED** — what the audio pipeline gates on. Derived
//!   from `MASTER_PAUSED || per_app::effective_paused(current_app)`.
//!   The per-app fallback now defaults to **paused for unlisted apps**,
//!   so a fresh install treats every bundle as off-by-default and the
//!   user explicitly resumes the apps they want voice typing in.
//!
//! Callers should still call `is_paused()` for the gating check — its
//! semantics ("am I effectively paused right now?") didn't change.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

use parking_lot::Mutex;

/// User's explicit global force-pause switch. When `true`, EFFECTIVE is
/// unconditionally `true`. The audio/mic watchdogs used to overload this
/// flag, which meant their auto-resume (`set_paused(false)`) silently
/// wiped a pause the USER had set — they now have their own sources
/// below and MASTER carries user intent only.
static MASTER_PAUSED: AtomicBool = AtomicBool::new(false);

/// Watchdog source: the Mac is playing audio (Swift's
/// `NotifySystemAudioState`). Auto-clears when playback stops.
static AUDIO_OUTPUT_PAUSED: AtomicBool = AtomicBool::new(false);

/// Watchdog source: another app (Zoom, FaceTime, …) holds the
/// microphone. Auto-clears when the app releases it.
static MIC_CONFLICT_PAUSED: AtomicBool = AtomicBool::new(false);

/// Derived "what the audio pipeline gates on". Recomputed by
/// `recompute_effective()` whenever MASTER, current_app, or the per-app
/// overrides change. Starts `true` so a fresh launch is paused-by-default
/// until the user resumes either globally or for a specific app.
static EFFECTIVE_PAUSED: AtomicBool = AtomicBool::new(true);

/// Recompute the effective pause state from MASTER + per-app rules.
/// Returns `(new_effective, changed)`. Callers broadcast `Paused` /
/// `Resumed` only when `changed` is true to keep the UDS log quiet.
pub fn recompute_effective() -> (bool, bool) {
    let new_effective = compute_effective();
    let old = EFFECTIVE_PAUSED.swap(new_effective, Ordering::Relaxed);
    (new_effective, new_effective != old)
}

fn compute_effective() -> bool {
    if MASTER_PAUSED.load(Ordering::Relaxed) {
        return true;
    }
    if AUDIO_OUTPUT_PAUSED.load(Ordering::Relaxed) || MIC_CONFLICT_PAUSED.load(Ordering::Relaxed)
    {
        return true;
    }
    if IDLE_AUTO_PAUSED.load(Ordering::Relaxed) {
        return true;
    }
    crate::always::per_app::effective_paused_for_current_app()
}

/// Shared mute state that can be toggled from anywhere
pub struct MuteState {
    muted: AtomicBool,
}

impl MuteState {
    pub fn new() -> Self {
        Self {
            muted: AtomicBool::new(false),
        }
    }

    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }
}

impl Default for MuteState {
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

/// Global mute state instance
static MUTE_STATE: std::sync::LazyLock<Arc<MuteState>> =
    std::sync::LazyLock::new(|| Arc::new(MuteState::new()));

/// Global auto-enter state instance
static AUTO_ENTER_STATE: std::sync::LazyLock<Arc<AutoEnterState>> =
    std::sync::LazyLock::new(|| Arc::new(AutoEnterState::new(false)));

/// Initialize the auto-enter state with the config value
pub fn init_auto_enter(initial_value: bool) {
    AUTO_ENTER_STATE.set_enabled(initial_value);
}

/// Get the global mute state
pub fn global_mute_state() -> Arc<MuteState> {
    Arc::clone(&MUTE_STATE)
}

/// Get the global auto-enter state
pub fn global_auto_enter_state() -> Arc<AutoEnterState> {
    Arc::clone(&AUTO_ENTER_STATE)
}

/// Is the daemon **effectively** paused right now? This is what the
/// audio capture / VAD / transcribe pipeline gates on.
///
/// Returns the cached `EFFECTIVE_PAUSED` value — callers who just
/// mutated MASTER or the current app must call `recompute_effective()`
/// first.
pub fn is_paused() -> bool {
    EFFECTIVE_PAUSED.load(Ordering::Relaxed)
}

/// Is the global master force-pause flag set? Distinct from
/// `is_paused()` — a fresh-install app with no overrides is paused
/// (effective) but not master-paused. Master-paused is a stronger
/// signal: every app is force-paused, including ones with an
/// override that says "active".
pub fn is_master_paused() -> bool {
    MASTER_PAUSED.load(Ordering::Relaxed)
}

/// Toggle MASTER and return `(new_effective, changed)`. Caller
/// broadcasts `Paused` / `Resumed` only when `changed`.
pub fn toggle_pause() -> (bool, bool) {
    let old_master = MASTER_PAUSED.load(Ordering::Relaxed);
    MASTER_PAUSED.store(!old_master, Ordering::Relaxed);
    recompute_effective()
}

/// Set MASTER explicitly. Returns `(new_effective, changed)` so the
/// caller can decide whether to broadcast a UDS event.
pub fn set_paused(paused: bool) -> (bool, bool) {
    MASTER_PAUSED.store(paused, Ordering::Relaxed);
    recompute_effective()
}

/// Set/clear the audio-output watchdog pause source. Never touches
/// MASTER — a manual pause survives playback stopping.
pub fn set_audio_output_paused(paused: bool) -> (bool, bool) {
    AUDIO_OUTPUT_PAUSED.store(paused, Ordering::Relaxed);
    recompute_effective()
}

pub fn is_audio_output_paused() -> bool {
    AUDIO_OUTPUT_PAUSED.load(Ordering::Relaxed)
}

/// Set/clear the mic-conflict watchdog pause source. Never touches
/// MASTER — a manual pause survives the call ending.
pub fn set_mic_conflict_paused(paused: bool) -> (bool, bool) {
    MIC_CONFLICT_PAUSED.store(paused, Ordering::Relaxed);
    recompute_effective()
}

pub fn is_mic_conflict_paused() -> bool {
    MIC_CONFLICT_PAUSED.load(Ordering::Relaxed)
}

/// True when ANY global pause source is active (user master pause or a
/// watchdog). The global pause chord resumes by clearing all of them —
/// an explicit user resume overrides the watchdogs until their next
/// state transition (so you CAN force dictation while music plays).
pub fn is_any_global_pause() -> bool {
    is_master_paused() || is_audio_output_paused() || is_mic_conflict_paused()
}

/// Clear every global pause source (user + watchdogs) at once. Used by
/// the explicit global-resume chord.
pub fn clear_global_pauses() -> (bool, bool) {
    MASTER_PAUSED.store(false, Ordering::Relaxed);
    AUDIO_OUTPUT_PAUSED.store(false, Ordering::Relaxed);
    MIC_CONFLICT_PAUSED.store(false, Ordering::Relaxed);
    recompute_effective()
}

/// Check if the system is currently muted
pub fn is_muted() -> bool {
    MUTE_STATE.is_muted()
}

/// Set the mute state
pub fn set_muted(muted: bool) {
    MUTE_STATE.set_muted(muted);
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

/// Read the most recent paste text if still within `window`.
pub fn last_pasted_text_within(window: std::time::Duration) -> Option<String> {
    let guard = LAST_PASTED.lock();
    let (text, ts) = guard.as_ref()?;
    if ts.elapsed() <= window {
        Some(text.clone())
    } else {
        None
    }
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

/// True while a paste pipeline (copy → Cmd+V → optional grammar patch) is
/// in flight. Prevents overlapping pastes from VAD double-fire.
static PASTE_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Acquire the paste-in-flight lock. Returns `false` if another paste is
/// already running (e.g. async grammar patch still holding the lock).
pub fn try_begin_paste() -> bool {
    PASTE_IN_FLIGHT
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .is_ok()
}

/// Release the paste-in-flight lock.
pub fn end_paste() {
    PASTE_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
pub fn clear_paste_in_flight_for_test() {
    end_paste();
}

/// True when `candidate` matches a recent paste within `window` (exact or
/// near-duplicate after normalization).
pub fn should_suppress_duplicate_paste(candidate: &str, window: std::time::Duration) -> bool {
    let Some(recent) = last_pasted_text_within(window) else {
        return false;
    };
    crate::always::speech_action::is_near_duplicate_paste(candidate, &recent)
}

/// Last moment voice activity was detected. Used by the idle-pause
/// watchdog: if more than `idle_pause_secs` elapses without voice, the
/// daemon auto-pauses. Initialized to `Instant::now()` at startup so we
/// don't immediately auto-pause on boot.
static LAST_VOICE: std::sync::LazyLock<Mutex<Instant>> =
    std::sync::LazyLock::new(|| Mutex::new(Instant::now()));

/// Update the last-voice timestamp to now. Called by the VAD whenever
/// it detects speech. Also called on manual resume so unpausing always
/// resets the idle window.
pub fn mark_voice_seen() {
    *LAST_VOICE.lock() = Instant::now();
}

/// How long since voice was last seen.
pub fn since_last_voice() -> std::time::Duration {
    LAST_VOICE.lock().elapsed()
}

/// True iff the daemon paused itself because of the idle-pause
/// watchdog. Manual resumes / mic-conflict / audio-output reasons set
/// this flag distinctly so the resume path can pick the right log line.
static IDLE_AUTO_PAUSED: AtomicBool = AtomicBool::new(false);

pub fn is_idle_auto_paused() -> bool {
    IDLE_AUTO_PAUSED.load(Ordering::Relaxed)
}

pub fn set_idle_auto_paused(v: bool) {
    IDLE_AUTO_PAUSED.store(v, Ordering::Relaxed);
}

/// Set when an auto-enter countdown is in flight. The keyboard
/// listener consults this to decide whether to cancel on the next
/// keystroke; the post-paste hook consults it before scheduling a new
/// countdown so two pastes in quick succession don't pile up.
static COUNTDOWN_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Cancel flag — set by the keyboard listener (any key) or by an
/// explicit `CancelAutoEnterCountdown` UDS command. The countdown
/// task polls this and aborts before pressing Return.
static COUNTDOWN_CANCEL: AtomicBool = AtomicBool::new(false);
/// Remaining-ms snapshot exposed for diagnostics. Not consulted by
/// the countdown loop itself.
static COUNTDOWN_REMAINING_MS: AtomicU32 = AtomicU32::new(0);

pub fn countdown_active() -> bool {
    COUNTDOWN_ACTIVE.load(Ordering::Relaxed)
}

pub fn countdown_set_active(v: bool) {
    COUNTDOWN_ACTIVE.store(v, Ordering::Relaxed);
    if !v {
        COUNTDOWN_REMAINING_MS.store(0, Ordering::Relaxed);
    }
}

pub fn countdown_request_cancel() {
    COUNTDOWN_CANCEL.store(true, Ordering::Relaxed);
}

pub fn countdown_take_cancel() -> bool {
    COUNTDOWN_CANCEL.swap(false, Ordering::Relaxed)
}

pub fn countdown_set_remaining_ms(ms: u32) {
    COUNTDOWN_REMAINING_MS.store(ms, Ordering::Relaxed);
}

pub fn countdown_remaining_ms() -> u32 {
    COUNTDOWN_REMAINING_MS.load(Ordering::Relaxed)
}

/// Bundle identifier of the currently-focused macOS application
/// (Swift app pushes this via `NotifyFocusedAppChanged`). `None` on
/// non-mac builds or before the first event arrives.
static CURRENT_APP: std::sync::LazyLock<Mutex<Option<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

pub fn current_app() -> Option<String> {
    CURRENT_APP.lock().clone()
}

pub fn set_current_app(bundle_id: Option<String>) {
    *CURRENT_APP.lock() = bundle_id;
}

/// Update the focused app **and** recompute effective. Returns
/// `(new_effective, changed)` like the other mutators so the caller
/// can decide whether to broadcast `Paused`/`Resumed` on the UDS bus.
pub fn set_current_app_and_recompute(bundle_id: Option<String>) -> (bool, bool) {
    *CURRENT_APP.lock() = bundle_id;
    recompute_effective()
}

/// Last final transcript text pasted (or filtered+force-pasted). Same
/// freshness model as `LAST_PASTED` but only the text, no timestamp —
/// used by the manual correction dialog to anchor its diff.
pub fn last_transcript_for_correction() -> Option<String> {
    LAST_PASTED.lock().as_ref().map(|(t, _)| t.clone())
}

/// Active dictation session — the running buffer of text that has been
/// pasted into the user's app since the last "committed" (auto-entered
/// or cancelled) utterance group. Populated by `handle_speech`; consumed
/// by the same function on a follow-up utterance while the auto-enter
/// countdown is still in flight so the daemon can APPEND a continuation
/// instead of pasting it as a fresh, capitalized sentence.
///
/// Cleared when:
///   - the auto-enter countdown fires (Return key is pressed)
///   - the auto-enter countdown is explicitly cancelled
///   - the daemon pauses or the user toggles mute
static DICTATION_BUFFER: std::sync::LazyLock<Mutex<Option<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

pub fn dictation_buffer_text() -> Option<String> {
    DICTATION_BUFFER.lock().clone()
}

pub fn dictation_buffer_set(text: impl Into<String>) {
    *DICTATION_BUFFER.lock() = Some(text.into());
}

pub fn dictation_buffer_clear() {
    *DICTATION_BUFFER.lock() = None;
    // The dictation session rides on the same "user took control"
    // signals (Return commit, keystroke, pause, focus-driven pause) —
    // clearing here keeps a single choke point for both.
    super::dictation::clear();
}

#[cfg(test)]
pub fn clear_dictation_buffer_for_test() {
    dictation_buffer_clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::always::per_app::{self, AppOverride};
    use std::collections::HashMap;

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn reset_pause_state_for_test() {
        MASTER_PAUSED.store(false, Ordering::Relaxed);
        AUDIO_OUTPUT_PAUSED.store(false, Ordering::Relaxed);
        MIC_CONFLICT_PAUSED.store(false, Ordering::Relaxed);
        IDLE_AUTO_PAUSED.store(false, Ordering::Relaxed);
        EFFECTIVE_PAUSED.store(true, Ordering::Relaxed);
        *CURRENT_APP.lock() = None;
        per_app::set_cache_for_test(HashMap::new());
    }

    /// Allowlist the given bundle and focus it so the only thing keeping
    /// us paused is whatever global source the test sets.
    fn focus_allowlisted_app(bundle: &str) {
        per_app::set_cache_for_test(HashMap::from([(
            bundle.to_string(),
            AppOverride {
                paused: Some(false),
                ..Default::default()
            },
        )]));
        set_current_app_and_recompute(Some(bundle.to_string()));
    }

    #[test]
    fn manual_pause_survives_watchdog_resume() {
        let _guard = TEST_LOCK.lock().expect("pause test lock poisoned");
        reset_pause_state_for_test();
        focus_allowlisted_app("com.example.editor");
        assert!(!is_paused());

        // Watchdog pauses (call starts), then the USER also pauses
        // manually, then the call ends. The old code routed the
        // watchdog through MASTER, so its resume wiped the manual
        // pause — the daemon resumed listening against the user's
        // explicit wish.
        set_mic_conflict_paused(true);
        set_paused(true);
        let (effective, _) = set_mic_conflict_paused(false);
        assert!(effective, "manual master pause must survive mic release");
        assert!(is_master_paused());
    }

    #[test]
    fn audio_output_pause_is_its_own_source() {
        let _guard = TEST_LOCK.lock().expect("pause test lock poisoned");
        reset_pause_state_for_test();
        focus_allowlisted_app("com.example.editor");

        let (effective, changed) = set_audio_output_paused(true);
        assert!(effective);
        assert!(changed);
        assert!(!is_master_paused(), "watchdogs must not touch MASTER");

        let (effective, changed) = set_audio_output_paused(false);
        assert!(!effective);
        assert!(changed);
    }

    #[test]
    fn clear_global_pauses_overrides_all_watchdogs() {
        let _guard = TEST_LOCK.lock().expect("pause test lock poisoned");
        reset_pause_state_for_test();
        focus_allowlisted_app("com.example.editor");

        set_paused(true);
        set_audio_output_paused(true);
        set_mic_conflict_paused(true);
        assert!(is_any_global_pause());

        // The global-resume chord: one press clears everything so the
        // user can force dictation over music / during a call.
        let (effective, changed) = clear_global_pauses();
        assert!(!effective);
        assert!(changed);
        assert!(!is_any_global_pause());
    }

    #[test]
    fn effective_paused_when_master_or_idle_or_per_app() {
        let _guard = TEST_LOCK.lock().expect("pause test lock poisoned");
        reset_pause_state_for_test();
        set_current_app(Some("com.example.editor".into()));
        assert!(recompute_effective().0);

        // Allowlisted app, no master/idle → listening.
        per_app::set_cache_for_test(HashMap::from([(
            "com.example.editor".to_string(),
            AppOverride {
                paused: Some(false),
                ..Default::default()
            },
        )]));
        let (eff, changed) = recompute_effective();
        assert!(!eff);
        assert!(changed);

        // Idle auto-pause without master.
        set_idle_auto_paused(true);
        let (eff, _) = recompute_effective();
        assert!(eff);
        assert!(!is_master_paused());

        set_idle_auto_paused(false);
        recompute_effective();

        // Master still wins over allowlist.
        let (_, _) = set_paused(true);
        assert!(is_master_paused());
        assert!(is_paused());
    }

    #[test]
    fn focus_change_recomputes_effective_for_allowlist() {
        let _guard = TEST_LOCK.lock().expect("pause test lock poisoned");
        reset_pause_state_for_test();
        per_app::set_cache_for_test(HashMap::from([(
            "com.example.editor".to_string(),
            AppOverride {
                paused: Some(false),
                ..Default::default()
            },
        )]));
        set_current_app_and_recompute(Some("com.other.app".into()));
        assert!(is_paused());

        set_idle_auto_paused(false);
        let (eff, changed) = set_current_app_and_recompute(Some("com.example.editor".into()));
        assert!(!eff);
        assert!(changed);
    }

    #[test]
    fn own_bundle_id_is_never_resumed() {
        let _guard = TEST_LOCK.lock().expect("pause test lock poisoned");
        reset_pause_state_for_test();
        let own_bundle_id = per_app::ALWAYS_OWN_BUNDLE_IDS[0];
        per_app::set_cache_for_test(HashMap::from([(
            own_bundle_id.to_string(),
            AppOverride {
                paused: Some(false),
                ..Default::default()
            },
        )]));

        let (effective, _) = set_current_app_and_recompute(Some(own_bundle_id.to_string()));
        assert!(effective);
        assert!(is_paused());
    }
}
