//! User pause/resume commands — the ONE place every user-facing control
//! (master chord, per-app chord, UDS `TogglePause` / `Resume` /
//! `SetAppPaused`, the idle pill's Play button, the CLI, the Linux tray)
//! turns a press into pause-state changes plus the events the GUIs follow.
//!
//! Field bug (v0.1.0, 2026-10-16): an auto-pause (idle watchdog, or mic
//! conflict while ChatGPT voice held the mic) showed the corner pill with a
//! Play button. Play ran `toggle-pause`, which only flipped MASTER: with
//! master off it turned master ON — the press paused *everywhere* instead
//! of resuming. The per-app chord then "did nothing" (master wins) and only
//! the global chord — the one path that cleared every source — recovered.
//!
//! The model now: a user's explicit resume clears every source a user may
//! override (master, idle, audio-output, mic-conflict — the watchdogs come
//! back on their next transition), and every press ends with the
//! authoritative effective state broadcast, so a GUI that predicted wrongly
//! is corrected instead of drifting.

use crate::always::event::global_broadcaster;
use crate::always::{pause, per_app};

/// What a user command did, for logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub effective: bool,
    pub changed: bool,
    pub master: bool,
}

/// Snapshot of the user-overridable sources, taken before a resume so the
/// GUIs can be told exactly which ones were cleared.
#[derive(Debug, Clone, Copy, Default)]
struct Cleared {
    idle: bool,
    audio_output: bool,
    mic_conflict: bool,
}

fn clear_overridable_sources(keep_master: bool) -> (bool, bool, Cleared) {
    let cleared = Cleared {
        idle: pause::is_idle_auto_paused(),
        audio_output: pause::is_audio_output_paused(),
        mic_conflict: pause::is_mic_conflict_paused(),
    };
    // Idle first: `set_idle_auto_paused` does not recompute, the mutators
    // below do — clearing it afterwards left EFFECTIVE stale at true.
    pause::set_idle_auto_paused(false);
    pause::mark_voice_seen();
    let (effective, changed) = if keep_master {
        let _ = pause::set_audio_output_paused(false);
        pause::set_mic_conflict_paused(false)
    } else {
        pause::clear_global_pauses()
    };
    (effective, changed, cleared)
}

fn announce_cleared(c: Cleared) {
    let b = global_broadcaster();
    if c.idle {
        b.idle_auto_resumed();
    }
    if c.audio_output {
        b.pause_source_changed("audio_output", false, None);
    }
    if c.mic_conflict {
        b.pause_source_changed("mic_conflict", false, None);
    }
}

/// Broadcast the effective state unconditionally. A user press is rare, and
/// the GUIs predict optimistically: re-stating the truth after every press
/// is what keeps the menu-bar icon from drifting when a prediction missed a
/// source (their `Paused`/`Resumed` handlers are idempotent).
fn announce_effective(effective: bool, changed: bool) {
    if effective {
        if changed {
            pause::dictation_buffer_clear();
        }
        global_broadcaster().paused();
    } else {
        global_broadcaster().resumed();
    }
}

fn should_pause_peer_before_resume(instance: &str, peer_live: bool) -> bool {
    instance == "dev" && peer_live
}

fn dev_can_take_over_microphone() -> bool {
    !should_pause_peer_before_resume(
        crate::config::instance(),
        crate::always::daemon::peer_is_live(),
    ) || crate::always::daemon::set_peer_paused(true)
}

/// Explicit "resume listening" (idle pill Play, `Resume` UDS command).
/// Idempotent: never pauses, whatever the current state.
pub fn resume() -> Outcome {
    // An explicit Dev resume is a deliberate takeover. Pause production
    // first, and keep Dev passive if the live peer cannot be reached.
    if !dev_can_take_over_microphone() {
        tracing::warn!("dev_takeover_blocked_peer_pause_failed");
        let effective = pause::is_paused();
        announce_effective(effective, false);
        return Outcome {
            effective,
            changed: false,
            master: pause::is_master_paused(),
        };
    }
    // An explicit user action takes ownership from startup handoff policy.
    pause::set_paused_by_peer(false);
    let (effective, changed, cleared) = clear_overridable_sources(false);
    let master = pause::is_master_paused();
    global_broadcaster().master_pause_changed(master);
    announce_cleared(cleared);
    announce_effective(effective, changed);
    Outcome {
        effective,
        changed,
        master,
    }
}

/// Global pause toggle (master chord, UDS `TogglePause`, CLI, tray). When
/// anything a user may override is pausing, the press resumes — the
/// user sees "paused" and means "resume". Otherwise it sets master.
pub fn toggle_global() -> Outcome {
    toggle_global_with_resume(resume)
}

fn toggle_global_with_resume(resume_fn: impl FnOnce() -> Outcome) -> Outcome {
    // A manual toggle is an explicit choice, even when a peer instance put
    // this daemon into passive standby.
    if pause::is_user_resumable_pause() {
        return resume_fn();
    }
    pause::set_paused_by_peer(false);
    let (effective, changed) = pause::set_paused(true);
    let master = pause::is_master_paused();
    global_broadcaster().master_pause_changed(master);
    announce_effective(effective, changed);
    Outcome {
        effective,
        changed,
        master,
    }
}

/// What the per-app chord should do for the focused app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerAppPlan {
    /// Allowlist the app (if not listed) and clear idle / watchdog pauses —
    /// the user sees "paused here" and asks to listen here.
    ResumeHere { add_to_allowlist: bool },
    /// Remove the app from the allowlist (it pauses).
    PauseHere,
    /// Master is on: the chord stays strictly per-app (master has its own
    /// chord), toggling allowlist membership only.
    ToggleAllowlistOnly { now_listed: bool },
}

/// Decide by what the user perceives, not only by allowlist membership.
/// The old rule (listed → remove) turned "resume" into "pause" whenever an
/// allowlisted app was auto-paused (idle, mic conflict): the press took it
/// OFF the allowlist and it stayed paused.
pub fn per_app_plan(listed: bool, effectively_paused: bool, master: bool) -> PerAppPlan {
    if master {
        return PerAppPlan::ToggleAllowlistOnly {
            now_listed: !listed,
        };
    }
    if effectively_paused {
        PerAppPlan::ResumeHere {
            add_to_allowlist: !listed,
        }
    } else {
        PerAppPlan::PauseHere
    }
}

/// Per-app chord. Returns the outcome and the app's resulting allowlist
/// membership (`true` = listening there), or the override write error.
pub fn per_app_chord(bundle: &str) -> anyhow::Result<(Outcome, bool)> {
    let listed = per_app::is_app_resumed(bundle);
    let plan = per_app_plan(listed, pause::is_paused(), pause::is_master_paused());
    let (effective, changed, now_listed, cleared) = match plan {
        PerAppPlan::ResumeHere { add_to_allowlist } => {
            if add_to_allowlist {
                per_app::set_app_paused_override(bundle, Some(false))?;
            }
            let (e, c, cleared) = clear_overridable_sources(true);
            (e, c, true, cleared)
        }
        PerAppPlan::PauseHere => {
            per_app::set_app_paused_override(bundle, None)?;
            let (e, c) = pause::recompute_effective();
            (e, c, false, Cleared::default())
        }
        PerAppPlan::ToggleAllowlistOnly { now_listed } => {
            per_app::set_app_paused_override(bundle, if now_listed { Some(false) } else { None })?;
            let (e, c) = pause::recompute_effective();
            (e, c, now_listed, Cleared::default())
        }
    };
    if !effective {
        pause::mark_voice_seen();
    }
    // Allowlist snapshot BEFORE the effective state: GUIs re-predict on
    // `ResumedAppsChanged`, so the daemon's truth must arrive last.
    let b = global_broadcaster();
    b.resumed_apps_changed(per_app::resumed_apps());
    announce_cleared(cleared);
    announce_effective(effective, changed);
    // Scope feedback: `paused` from the app's perspective.
    b.pause_scope_toggled("app", Some(bundle.to_string()), !now_listed);
    Ok((
        Outcome {
            effective,
            changed,
            master: pause::is_master_paused(),
        },
        now_listed,
    ))
}

/// `SetAppPaused` from a GUI (Settings / Onboarding allowlist checkboxes).
/// Plain allowlist editing: it recomputes the effective state and nothing
/// else. It must NOT lift idle / audio-output / mic-conflict pauses — only
/// the explicit resume controls (chords, idle pill, `Resume` /
/// `TogglePause`, CLI, tray) do that.
pub fn set_app_paused(bundle: &str, paused: Option<bool>) -> anyhow::Result<Outcome> {
    per_app::set_app_paused_override(bundle, paused)?;
    let (effective, changed) = pause::recompute_effective();
    // Same gating as before the shared module: only a real transition is
    // broadcast, so editing a non-focused app does not spam the bus.
    if changed {
        if effective {
            pause::dictation_buffer_clear();
            global_broadcaster().paused();
        } else {
            pause::mark_voice_seen();
            global_broadcaster().resumed();
        }
    }
    // Always re-send the allowlist so every client sees the same snapshot.
    global_broadcaster().resumed_apps_changed(per_app::resumed_apps());
    Ok(Outcome {
        effective,
        changed,
        master: pause::is_master_paused(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::always::event::DaemonEvent;

    const APP: &str = "com.openai.chat";

    fn setup(listed: bool) -> std::sync::MutexGuard<'static, ()> {
        let g = pause::PAUSE_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        pause::reset_pause_state_for_test();
        if listed {
            pause::allowlist_for_test(APP);
        }
        pause::set_current_app_and_recompute(Some(APP.to_string()));
        g
    }

    #[test]
    fn only_explicit_dev_resume_requires_a_live_peer_pause() {
        assert!(should_pause_peer_before_resume("dev", true));
        assert!(!should_pause_peer_before_resume("dev", false));
        assert!(!should_pause_peer_before_resume("prod", true));
        assert!(!should_pause_peer_before_resume("smoke", true));
    }

    fn drain(rx: &mut tokio::sync::broadcast::Receiver<DaemonEvent>) -> Vec<DaemonEvent> {
        let mut v = Vec::new();
        while let Ok(e) = rx.try_recv() {
            v.push(e);
        }
        v
    }

    fn last_effective(evs: &[DaemonEvent]) -> Option<bool> {
        evs.iter().rev().find_map(|e| match e {
            DaemonEvent::Paused | DaemonEvent::PausedQuietly => Some(true),
            DaemonEvent::Resumed | DaemonEvent::ResumedQuietly => Some(false),
            _ => None,
        })
    }

    /// Ludwig's repro, step 1: idle auto-pause → pill Play. The pill used to
    /// send TogglePause, which turned MASTER on (paused everywhere).
    #[test]
    fn pill_play_resumes_from_idle_pause() {
        let _g = setup(true);
        pause::set_idle_auto_paused(true);
        pause::recompute_effective();
        assert!(pause::is_paused());
        let mut rx = global_broadcaster().subscribe();

        let out = resume();
        assert!(!out.effective, "Play must resume");
        assert!(!out.master, "Play must never turn the master pause on");
        assert!(!pause::is_idle_auto_paused());
        let evs = drain(&mut rx);
        assert_eq!(last_effective(&evs), Some(false));
        assert!(
            evs.iter()
                .any(|e| matches!(e, DaemonEvent::IdleAutoResumed))
        );
    }

    #[test]
    fn explicit_resume_takes_over_peer_standby() {
        let _g = setup(true);
        pause::set_paused(true);
        pause::set_paused_by_peer(true);
        assert!(pause::is_paused_by_peer());

        let out = resume();

        assert!(!out.effective);
        assert!(!pause::is_paused_by_peer());
    }

    #[test]
    fn failed_dev_takeover_keeps_toggle_peer_standby_owned() {
        let _g = setup(true);
        pause::set_paused(true);
        pause::set_paused_by_peer(true);

        let out = toggle_global_with_resume(|| {
            // Model the resume path when the live production pause request
            // fails: it reports the still-paused state without clearing the
            // peer-standby marker.
            assert!(pause::is_paused_by_peer());
            Outcome {
                effective: pause::is_paused(),
                changed: false,
                master: pause::is_master_paused(),
            }
        });

        assert!(out.effective);
        assert!(pause::is_paused_by_peer());
    }

    /// The same press through the legacy TogglePause path (the released
    /// pill, the CLI, the Linux tray) must resume too, not mute.
    #[test]
    fn toggle_global_resumes_from_idle_only_pause() {
        let _g = setup(true);
        pause::set_idle_auto_paused(true);
        pause::recompute_effective();

        let out = toggle_global();
        assert!(!out.effective, "toggle from an idle pause must resume");
        assert!(!out.master, "must not set master");
    }

    #[test]
    fn toggle_global_resumes_from_mic_conflict_pause() {
        let _g = setup(true);
        pause::set_mic_conflict_paused(true);
        let mut rx = global_broadcaster().subscribe();

        let out = toggle_global();
        assert!(!out.effective);
        assert!(!out.master);
        assert!(!pause::is_mic_conflict_paused());
        let evs = drain(&mut rx);
        assert!(evs.iter().any(|e| matches!(e,
            DaemonEvent::PauseSourceChanged { source, paused: false, .. } if source == "mic_conflict")));
        assert_eq!(last_effective(&evs), Some(false));
    }

    #[test]
    fn toggle_global_resumes_master_plus_watchdogs_in_one_press() {
        let _g = setup(true);
        pause::set_paused(true);
        pause::set_audio_output_paused(true);
        pause::set_mic_conflict_paused(true);
        pause::set_idle_auto_paused(true);
        let out = toggle_global();
        assert!(
            !out.effective,
            "one press lifts every user-overridable source"
        );
    }

    #[test]
    fn toggle_global_pauses_when_listening() {
        let _g = setup(true);
        assert!(!pause::is_paused());
        let mut rx = global_broadcaster().subscribe();
        let out = toggle_global();
        assert!(out.effective && out.master);
        assert_eq!(last_effective(&drain(&mut rx)), Some(true));
        // ...and the next press resumes.
        assert!(!toggle_global().effective);
    }

    /// A press that changes nothing visible still re-states the truth, so a
    /// GUI that mispredicted is corrected (menu-bar icon out of sync).
    #[test]
    fn every_press_broadcasts_authoritative_state() {
        let _g = setup(false); // unlisted app: per-app paused
        let mut rx = global_broadcaster().subscribe();
        let out = resume();
        assert!(
            out.effective,
            "per-app pause is not lifted by a global resume"
        );
        assert!(!out.changed);
        assert_eq!(last_effective(&drain(&mut rx)), Some(true));
    }

    /// Ludwig's repro, step 2: allowlisted ChatGPT auto-paused, per-app
    /// chord pressed. It used to REMOVE the app (listed → remove).
    #[test]
    fn per_app_chord_resumes_auto_paused_allowlisted_app() {
        let _g = setup(true);
        pause::set_mic_conflict_paused(true);
        pause::set_idle_auto_paused(true);
        pause::recompute_effective();
        assert_eq!(
            per_app_plan(true, true, false),
            PerAppPlan::ResumeHere {
                add_to_allowlist: false
            }
        );
        let (out, listed) = per_app_chord(APP).expect("override write");
        assert!(listed, "the app must stay on the allowlist");
        assert!(!out.effective, "chord must resume here");
        assert!(per_app::is_app_resumed(APP));
    }

    /// #124: in Slack with Apple Music playing (audio-output pause), the
    /// per-app chord, the pill / toggle and the end of playback all resume.
    #[test]
    fn audio_output_pause_resumes_from_every_control() {
        let _g = setup(true);
        pause::set_audio_output_paused(true);
        let (out, listed) = per_app_chord(APP).expect("override write");
        assert!(listed, "chord must not take Slack off the allowlist");
        assert!(!out.effective, "per-app chord resumes over music");

        pause::set_audio_output_paused(true);
        assert!(
            !toggle_global().effective,
            "global toggle resumes over music"
        );

        pause::set_audio_output_paused(true);
        assert!(!resume().effective, "pill Play resumes over music");

        pause::set_audio_output_paused(true);
        let (effective, changed) = pause::set_audio_output_paused(false);
        assert!(!effective && changed, "playback stopping resumes");
    }

    #[test]
    fn per_app_chord_adds_and_resumes_unlisted_idle_app() {
        let _g = setup(false);
        pause::set_idle_auto_paused(true);
        pause::recompute_effective();
        let (out, listed) = per_app_chord(APP).expect("override write");
        assert!(listed);
        assert!(
            !out.effective,
            "idle must not keep a just-resumed app paused"
        );
    }

    #[test]
    fn per_app_chord_pauses_a_listening_app_and_toggles_back() {
        let _g = setup(true);
        let (out, listed) = per_app_chord(APP).expect("override write");
        assert!(!listed && out.effective);
        let (out, listed) = per_app_chord(APP).expect("override write");
        assert!(listed && !out.effective);
    }

    #[test]
    fn per_app_chord_under_master_only_edits_allowlist() {
        let _g = setup(false);
        pause::set_paused(true);
        let (out, listed) = per_app_chord(APP).expect("override write");
        assert!(listed);
        assert!(out.effective && out.master, "master keeps its own chord");
    }

    /// Ludwig's repro, step 3: the conflict ends → listening resumes. This
    /// is the mic watcher's resume path, which must not be blocked by a
    /// stale idle flag accumulated during the call.
    #[test]
    fn end_of_mic_conflict_resumes_even_after_idle_fired() {
        let _g = setup(true);
        pause::set_mic_conflict_paused(true);
        pause::set_idle_auto_paused(true);
        pause::recompute_effective();
        // mic_watcher MicAction::Resume
        pause::set_idle_auto_paused(false);
        let (effective, changed) = pause::set_mic_conflict_paused(false);
        assert!(!effective && changed);
    }

    /// Allowlist edits from Settings / Onboarding are not resume presses:
    /// they must leave idle / watchdog pauses alone, focused app or not.
    #[test]
    fn set_app_paused_leaves_idle_and_watchdog_pauses() {
        let _g = setup(false);
        pause::set_idle_auto_paused(true);
        pause::set_audio_output_paused(true);
        pause::set_mic_conflict_paused(true);
        pause::recompute_effective();
        let out = set_app_paused(APP, Some(false)).expect("override write");
        assert!(out.effective, "an allowlist edit must not resume");
        assert!(pause::is_idle_auto_paused());
        assert!(pause::is_audio_output_paused());
        assert!(pause::is_mic_conflict_paused());
        assert!(per_app::is_app_resumed(APP), "the edit itself still lands");

        let out = set_app_paused("com.other", Some(false)).expect("override write");
        assert!(out.effective);
        assert!(pause::is_idle_auto_paused());
    }

    /// An allowlist edit that is the only thing pausing the focused app
    /// still resumes it (the effective state follows the allowlist).
    #[test]
    fn set_app_paused_allowlisting_focused_app_resumes_when_nothing_else_pauses() {
        let _g = setup(false);
        assert!(pause::is_paused(), "unlisted focused app is paused");
        let out = set_app_paused(APP, Some(false)).expect("override write");
        assert!(!out.effective && out.changed);
    }
}
