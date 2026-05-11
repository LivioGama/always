//! Auto-enter countdown — fires a synthetic Return key `delay_ms` after a
//! paste, broadcasting `AutoEnterCountdownTick` events so the GUI overlay
//! can show a visible countdown. Any key press while the countdown is
//! active sets the global cancel flag (see `pause::countdown_request_cancel`)
//! and the synthesizer aborts before posting Return.

use std::time::Duration;

use tokio::runtime::Handle;

use super::event::global_broadcaster;
use super::pause;

const TICK_MS: u32 = 100;

/// Schedule the auto-enter Return key for `delay_ms` from now. Spawns a
/// tokio task on `rt` so the daemon main loop is free to record the next
/// utterance. If a countdown is already active, the new one supersedes
/// it (we cancel the previous so it doesn't fire after the new paste).
pub fn schedule(rt: &Handle, delay_ms: u32) {
    if delay_ms == 0 {
        // Fast path: bypass the timer entirely.
        let _ = press_return_now();
        return;
    }

    // Supersede any in-flight countdown.
    if pause::countdown_active() {
        pause::countdown_request_cancel();
    }
    pause::countdown_set_active(true);
    pause::countdown_set_remaining_ms(delay_ms);
    // Clear any stale cancel from a previous run we just aborted.
    let _ = pause::countdown_take_cancel();

    let total = delay_ms;
    global_broadcaster().auto_enter_countdown_started(delay_ms, total);

    rt.spawn(async move {
        let mut remaining = delay_ms;
        loop {
            tokio::time::sleep(Duration::from_millis(TICK_MS as u64)).await;

            if pause::countdown_take_cancel() {
                pause::countdown_set_active(false);
                global_broadcaster().auto_enter_countdown_cancelled();
                tracing::info!("auto_enter_countdown_cancelled");
                return;
            }

            remaining = remaining.saturating_sub(TICK_MS);
            pause::countdown_set_remaining_ms(remaining);
            global_broadcaster().auto_enter_countdown_tick(remaining);

            if remaining == 0 {
                break;
            }
        }

        if let Err(error) = press_return_now() {
            tracing::error!(?error, "auto_enter_countdown_press_return_failed");
        }
        pause::countdown_set_active(false);
        global_broadcaster().auto_enter_countdown_finished();
    });
}

fn press_return_now() -> anyhow::Result<()> {
    super::paste::press_return()
}
