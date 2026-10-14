//! Mic-conflict watchdog — pauses listening while another app (Zoom,
//! FaceTime, Teams, …) holds the microphone, and resumes when it lets go.
//!
//! Replaces the inline check that lived at the top of the event loop:
//! that check only ran BETWEEN `record_utterance` calls, which block for
//! up to `timeout_secs` (30s) waiting for voice — a call starting
//! mid-wait went undetected for the full window and Always kept
//! transcribing the user's meeting. As a dedicated task the detection
//! latency is bounded by the poll interval (~3s).
//!
//! Pauses via [`pause::set_mic_conflict_paused`] — its own source, never
//! MASTER — so a pause the user set manually survives the call ending.

use std::time::Duration;

use tokio::runtime::Handle;

use crate::always::log::{Event, Logger};
use crate::always::mic_monitor::MicrophoneMonitor;
use crate::always::{config as always_config, event, pause};

const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Consecutive polls the microphone must be free before listening
/// resumes.
///
/// Dictation apps do not hold the mic for a whole session — they grab it
/// per phrase and release it in between. Resuming on the first free poll
/// meant Always woke up during those gaps and started listening to
/// speech that was still aimed at the other app: observed live, the mic
/// was released and Always had verified the user's voice and lit the
/// overlay **82 ms later**, while they were still mid-dictation
/// elsewhere. Requiring the mic to stay free across several polls rides
/// out the between-phrase gaps; the cost is a couple of seconds before
/// Always comes back after a genuine call, which is imperceptible.
const RESUME_FREE_POLLS: u32 = 3;

/// What one poll decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MicAction {
    None,
    Pause,
    Resume,
}

/// Pause/resume state machine, separated from the Core Audio probe so the
/// "every auto-pause auto-resumes" guarantee is testable.
///
/// The probe result per poll is `Some(true)` (mic taken), `Some(false)`
/// (free) or `None` (probe failed). A failed probe while paused counts as a
/// FREE poll: a probe that keeps erroring must never strand Always paused
/// with no way back except a restart. A failed probe while listening does
/// nothing (no evidence of a conflict).
#[derive(Debug, Default)]
struct MicConflictState {
    paused: bool,
    free_polls: u32,
}

impl MicConflictState {
    fn on_poll(&mut self, in_use: Option<bool>) -> MicAction {
        match (self.paused, in_use) {
            (false, Some(true)) => {
                self.paused = true;
                self.free_polls = 0;
                MicAction::Pause
            }
            (false, _) => MicAction::None,
            // Mic taken again — cancel any resume that was building.
            (true, Some(true)) => {
                self.free_polls = 0;
                MicAction::None
            }
            (true, Some(false) | None) => {
                self.free_polls += 1;
                if self.free_polls < RESUME_FREE_POLLS {
                    return MicAction::None;
                }
                self.free_polls = 0;
                self.paused = false;
                MicAction::Resume
            }
        }
    }
}

/// Spawn the watchdog task. Lives for the daemon lifetime.
pub fn spawn(rt: &Handle, coexisting_bundles: Vec<String>) {
    rt.spawn(async move {
        let mut monitor = MicrophoneMonitor::new(coexisting_bundles);
        let mut state = MicConflictState::default();
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            // `is_microphone_in_use` is an in-process CoreAudio query
            // (microseconds; lsof subprocess only as a pre-14.4
            // fallback) — negligible for the runtime workers at 1s.
            let in_use = match monitor.is_microphone_in_use() {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(error = %e, "microphone_check_failed");
                    None
                }
            };
            // Self-conflict watchdog: if every process we would blame is
            // Always itself, it is never a conflict. Refuse to pause, and
            // if already paused, resume at once (logged as an error — the
            // probe's self-exclusion missed something).
            let mut in_use = in_use;
            if in_use == Some(true) || state.paused {
                let users = monitor.get_microphone_users().unwrap_or_default();
                if !users.is_empty() && users.iter().all(|u| crate::always::mic_monitor::label_is_self(u)) {
                    tracing::error!(apps = ?users, paused = state.paused, "mic_conflict_self_only_ignored");
                    in_use = Some(false);
                    if state.paused {
                        state.free_polls = RESUME_FREE_POLLS - 1;
                    }
                }
            }
            match state.on_poll(in_use) {
                MicAction::None => {
                    if state.paused && state.free_polls > 0 {
                        tracing::debug!(
                            free_polls = state.free_polls,
                            "microphone_free_waiting_to_resume"
                        );
                    }
                }
                MicAction::Pause => {
                    let users = monitor.get_microphone_users().unwrap_or_default();
                    let detail = if users.is_empty() {
                        None
                    } else {
                        Some(users.join(", "))
                    };
                    tracing::info!(apps = ?detail, "microphone_auto_paused");
                    if let Ok(log_path) = always_config::configured_log_path()
                        && let Ok(mut logger) = Logger::open(&log_path)
                    {
                        let apps = detail
                            .clone()
                            .unwrap_or_else(|| "another application".into());
                        logger.write(Event::MicrophoneAutoPaused { apps: &apps });
                    }
                    let (_effective, changed) = pause::set_mic_conflict_paused(true);
                    event::global_broadcaster().pause_source_changed("mic_conflict", true, detail);
                    if changed {
                        pause::dictation_buffer_clear();
                        event::global_broadcaster().paused();
                    }
                }
                MicAction::Resume => {
                    tracing::info!("microphone_auto_resumed");
                    if let Ok(log_path) = always_config::configured_log_path()
                        && let Ok(mut logger) = Logger::open(&log_path)
                    {
                        logger.write(Event::MicrophoneAutoResumed);
                    }
                    // Clear the idle flag too — the idle gap accumulated
                    // during the call must not instantly re-pause us.
                    pause::set_idle_auto_paused(false);
                    let (effective, changed) = pause::set_mic_conflict_paused(false);
                    pause::mark_voice_seen();
                    event::global_broadcaster().pause_source_changed("mic_conflict", false, None);
                    if changed {
                        if effective {
                            event::global_broadcaster().paused();
                        } else {
                            event::global_broadcaster().resumed();
                        }
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mic_self_only_conflict_resumes_on_next_poll() {
        // Watchdog path: paused, and the next poll is rewritten to "free"
        // with the resume counter primed — one poll must resume.
        let mut st = MicConflictState::default();
        assert_eq!(st.on_poll(Some(true)), MicAction::Pause);
        st.free_polls = RESUME_FREE_POLLS - 1;
        assert_eq!(st.on_poll(Some(false)), MicAction::Resume);
        assert!(!st.paused);
    }

    #[test]
    fn mic_pause_then_release_resumes() {
        let mut s = MicConflictState::default();
        assert_eq!(s.on_poll(Some(false)), MicAction::None);
        assert_eq!(s.on_poll(Some(true)), MicAction::Pause);
        assert_eq!(s.on_poll(Some(true)), MicAction::None, "held: stays paused");
        for _ in 1..RESUME_FREE_POLLS {
            assert_eq!(s.on_poll(Some(false)), MicAction::None);
        }
        assert_eq!(s.on_poll(Some(false)), MicAction::Resume);
        assert!(!s.paused);
        // A later conflict pauses again and again resumes.
        assert_eq!(s.on_poll(Some(true)), MicAction::Pause);
        let resumed = (0..RESUME_FREE_POLLS).any(|_| s.on_poll(Some(false)) == MicAction::Resume);
        assert!(resumed);
    }

    #[test]
    fn mic_flapping_dictation_app_does_not_resume_early() {
        let mut s = MicConflictState::default();
        s.on_poll(Some(true));
        for _ in 0..10 {
            for _ in 1..RESUME_FREE_POLLS {
                assert_eq!(s.on_poll(Some(false)), MicAction::None);
            }
            assert_eq!(s.on_poll(Some(true)), MicAction::None);
        }
        assert!(s.paused);
    }

    #[test]
    fn mic_failing_probe_never_strands_a_pause() {
        let mut s = MicConflictState::default();
        assert_eq!(s.on_poll(None), MicAction::None, "error while listening: no pause");
        assert_eq!(s.on_poll(Some(true)), MicAction::Pause);
        let resumed = (0..RESUME_FREE_POLLS).any(|_| s.on_poll(None) == MicAction::Resume);
        assert!(resumed, "a probe that keeps failing must still resume");
    }

    #[test]
    fn mic_tap_only_helper_never_pauses() {
        // End to end through the real classifier: an Iris process tap's
        // aggregate is not a microphone, so the poll reports "free".
        use crate::always::mic_monitor::{InputDeviceFacts, TRANSPORT_AGGREGATE, process_counts_as_mic};
        let tap = InputDeviceFacts {
            transport: Some(TRANSPORT_AGGREGATE),
            input_streams: Some(1),
            taps: 1,
            sub_input_streams: vec![],
        };
        let in_use = process_counts_as_mic(Some(&[tap]))
            && !crate::always::mic_monitor::is_ignored_captor(Some("music-tap"), None, None, &[]);
        let mut s = MicConflictState::default();
        for _ in 0..20 {
            assert_eq!(s.on_poll(Some(in_use)), MicAction::None);
        }
        assert!(!s.paused);
    }
}
