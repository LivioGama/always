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

/// Spawn the watchdog task. Lives for the daemon lifetime.
pub fn spawn(rt: &Handle) {
    rt.spawn(async move {
        let mut monitor = MicrophoneMonitor::new();
        let mut paused_for_mic = false;
        // Consecutive polls seeing a free mic while paused. Reset by any
        // poll that finds the mic taken again, so a dictation app
        // toggling between phrases never accumulates a resume.
        let mut free_polls: u32 = 0;
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            // `is_microphone_in_use` is an in-process CoreAudio query
            // (microseconds; lsof subprocess only as a pre-14.4
            // fallback) — negligible for the runtime workers at 1s.
            match monitor.is_microphone_in_use() {
                // Mic taken again — cancel any resume that was building.
                Ok(true) if paused_for_mic => {
                    free_polls = 0;
                }
                Ok(true) if !paused_for_mic => {
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
                    paused_for_mic = true;
                }
                Ok(false) if paused_for_mic => {
                    free_polls += 1;
                    if free_polls < RESUME_FREE_POLLS {
                        tracing::debug!(free_polls, "microphone_free_waiting_to_resume");
                        continue;
                    }
                    free_polls = 0;
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
                    paused_for_mic = false;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "microphone_check_failed");
                }
                _ => {}
            }
        }
    });
}
