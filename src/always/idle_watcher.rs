//! Auto-pause after a configurable stretch with no voice activity.
//!
//! The VAD blocks while recording an utterance and the main event loop
//! blocks on the VAD, so a periodic tick has to live outside the main
//! loop. We spawn a tokio task that wakes every `CHECK_INTERVAL` and:
//!
//! 1. If the daemon is currently paused for an *idle* reason and voice
//!    came back somehow (e.g. user toggled resume manually), clear the
//!    idle-auto-paused flag without touching the pause state.
//! 2. If the daemon is *not* paused and the gap since the last voice
//!    event exceeds `idle_pause_secs`, set `paused = true`, broadcast
//!    `IdleAutoPaused`, and write a log line. If idle_pause_action is
//!    `PauseAndMute`, also set `muted = true`.
//!
//! Disabled when `idle_pause_secs == 0`.

use std::time::Duration;

use tokio::runtime::Handle;

use super::{config::IdlePauseAction, event::global_broadcaster, pause};

const CHECK_INTERVAL: Duration = Duration::from_secs(5);

pub fn spawn(rt: &Handle, idle_pause_secs: u32, idle_pause_action: IdlePauseAction) {
    if idle_pause_secs == 0 {
        tracing::info!("idle_watcher_disabled");
        return;
    }
    let threshold = Duration::from_secs(idle_pause_secs as u64);
    tracing::info!(threshold_secs = idle_pause_secs, "idle_watcher_spawn");
    rt.spawn(async move {
        loop {
            tokio::time::sleep(CHECK_INTERVAL).await;

            let elapsed = pause::since_last_voice();
            let already_paused = pause::is_paused();

            if !already_paused && elapsed >= threshold {
                pause::set_paused(true);
                pause::set_idle_auto_paused(true);

                // Apply idle_pause_action: if pause_and_mute, also set muted
                if idle_pause_action == IdlePauseAction::PauseAndMute {
                    pause::set_muted(true);
                }

                let secs = elapsed.as_secs() as u32;
                global_broadcaster().idle_auto_paused(secs);
                global_broadcaster().paused();
                tracing::info!(idle_secs = secs, "idle_auto_paused");
            }
        }
    });
}
