//! Auto-pause after a configurable stretch with no voice activity.
//!
//! The VAD blocks while recording an utterance and the main event loop
//! blocks on the VAD, so a periodic tick has to live outside the main
//! loop. We spawn a tokio task that wakes every `CHECK_INTERVAL` and:
//!
//! 1. If the daemon is currently paused for an *idle* reason and voice
//!    came back somehow (e.g. user toggled resume manually), clear the
//!    idle-auto-paused flag without touching the pause state.
//! 2. If the daemon is *not* already paused for any reason and
//!    the gap since the last voice event exceeds `idle_pause_secs`, set
//!    the idle flag (not MASTER), recompute effective pause, broadcast
//!    `IdleAutoPaused`, and write a log line.
//!
//! Disabled when `idle_pause_secs == 0`.

use std::time::Duration;

use tokio::runtime::Handle;

use super::{event::global_broadcaster, pause};

const CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// What one idle tick decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdleTick {
    Nothing,
    /// Already paused for another reason (master, per-app, mic conflict,
    /// audio output, no GUI): no voice CAN be seen, so the gap is not
    /// idleness. Restart the clock instead of stacking an idle pause on
    /// top — that stacked idle flag outlived the real pause, showed the
    /// corner pill over an app that was never listening, and kept a
    /// just-resumed app paused (v0.1.0 field report).
    ResetClock,
    AutoPause,
}

fn idle_tick(
    effectively_paused: bool,
    idle_paused: bool,
    elapsed: Duration,
    threshold: Duration,
) -> IdleTick {
    if idle_paused {
        IdleTick::Nothing
    } else if effectively_paused {
        IdleTick::ResetClock
    } else if elapsed >= threshold {
        IdleTick::AutoPause
    } else {
        IdleTick::Nothing
    }
}

pub fn spawn(rt: &Handle, idle_pause_secs: u32) {
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
            // Idle pause is separate from MASTER so switching to an
            // allowlisted app or speaking again can resume without the
            // user hunting for a global "lift pause" toggle.
            match idle_tick(
                pause::is_paused(),
                pause::is_idle_auto_paused(),
                elapsed,
                threshold,
            ) {
                IdleTick::Nothing => {}
                IdleTick::ResetClock => pause::mark_voice_seen(),
                IdleTick::AutoPause => {
                    pause::set_idle_auto_paused(true);
                    let (effective, changed) = pause::recompute_effective();

                    let secs = elapsed.as_secs() as u32;
                    global_broadcaster().idle_auto_paused(secs);
                    if changed {
                        pause::dictation_buffer_clear();
                        global_broadcaster().paused();
                    }
                    tracing::info!(idle_secs = secs, effective, changed, "idle_auto_paused");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_secs(300);

    #[test]
    fn idle_fires_only_while_actually_listening() {
        assert_eq!(idle_tick(false, false, T, T), IdleTick::AutoPause);
        assert_eq!(idle_tick(false, false, T / 2, T), IdleTick::Nothing);
    }

    #[test]
    fn idle_never_stacks_on_another_pause() {
        // In a non-allowlisted app, during a call, or muted: no voice can
        // be seen, so the clock restarts instead of idle-pausing.
        assert_eq!(idle_tick(true, false, T * 10, T), IdleTick::ResetClock);
    }

    #[test]
    fn idle_already_set_is_left_alone() {
        assert_eq!(idle_tick(true, true, T * 10, T), IdleTick::Nothing);
    }
}
