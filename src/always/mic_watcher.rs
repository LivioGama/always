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

const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Spawn the watchdog task. Lives for the daemon lifetime.
pub fn spawn(rt: &Handle) {
    rt.spawn(async move {
        let mut monitor = MicrophoneMonitor::new();
        let mut paused_for_mic = false;
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            // `is_microphone_in_use` shells out to lsof (~10-30ms) — at a
            // 3s cadence this is negligible for the runtime workers.
            match monitor.is_microphone_in_use() {
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
                        let apps = detail.clone().unwrap_or_else(|| "another application".into());
                        logger.write(Event::MicrophoneAutoPaused { apps: &apps });
                    }
                    let (_effective, changed) = pause::set_mic_conflict_paused(true);
                    event::global_broadcaster().pause_source_changed(
                        "mic_conflict",
                        true,
                        detail,
                    );
                    if changed {
                        pause::dictation_buffer_clear();
                        event::global_broadcaster().paused();
                    }
                    paused_for_mic = true;
                }
                Ok(false) if paused_for_mic => {
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
                    event::global_broadcaster().pause_source_changed(
                        "mic_conflict",
                        false,
                        None,
                    );
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
