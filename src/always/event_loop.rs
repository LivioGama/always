use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use parking_lot::RwLock;
use tokio::runtime::{Handle, Runtime};

use crate::always::log::{Event, Logger};
use crate::always::speech_action::{
    SpeechAction, classify_transcription, merge_dictation_with, normalize_for_paste_dedupe,
    paste_dedupe_window,
};
use crate::always::{
    AlwaysConfig, auto_enter_countdown, clipboard_watcher, daemon, event, filter, idle_watcher,
    keyboard, mic_monitor, paste, pause, per_app, uds_server, vad,
};
use crate::managers::model_registry::ModelRegistry;
use crate::stt::Transcriber;
use crate::stt_dispatch::{PendingTranscriber, build_transcriber};

/// Active [`Transcriber`] shared between the main loop, the UDS server
/// (which swaps it when the user picks a different model in Settings),
/// and any future hot-reload sources. `RwLock<Arc<dyn Transcriber>>`
/// gives us cheap cloned reads (one Arc bump per utterance) and a
/// trivial swap on user model change — no need to restart the daemon.
pub type ActiveTranscriber = Arc<RwLock<Arc<dyn Transcriber>>>;

/// Active [`AlwaysConfig`] shared between the main loop and the UDS
/// server so Settings changes apply without a daemon restart.
pub type ActiveConfig = Arc<RwLock<AlwaysConfig>>;

pub fn run(cfg: &AlwaysConfig) -> Result<()> {
    let _pid = daemon::PidGuard::install()?;
    let mut log = Logger::open(&cfg.log_path)?;
    log.write(Event::Start { cfg });
    print_banner(cfg);

    let rt = Runtime::new()?;

    // Install SIGTERM/SIGINT handlers BEFORE any subsystem starts so a
    // signal during init still triggers cleanup. The handler removes
    // the pid + socket files and calls `std::process::exit(0)`.
    daemon::install_signal_handlers(rt.handle());

    // Initialize the auto-enter state from config
    pause::init_auto_enter(cfg.auto_enter);

    // Create shared config early so UDS server can start immediately
    let active_cfg: ActiveConfig = Arc::new(RwLock::new(cfg.clone()));

    // Start UDS server IMMEDIATELY with a placeholder transcriber so the GUI
    // can connect while the real backend loads in the background.
    let cfg_for_uds = Arc::clone(&active_cfg);
    let registry_placeholder = ModelRegistry::new().context("init model registry")?;
    // Pending placeholder blocks transcribe() until the real backend swaps in,
    // so the first utterance after launch waits for init instead of being
    // dropped with a "still initializing" error.
    let (pending, ready_signal) = PendingTranscriber::new();
    let active_placeholder: ActiveTranscriber = Arc::new(RwLock::new(pending));
    let registry_for_uds = registry_placeholder.clone();
    let active_for_uds = Arc::clone(&active_placeholder);
    let _uds_handle = rt.spawn(async move {
        if let Err(e) =
            uds_server::start_server(registry_for_uds, active_for_uds, cfg_for_uds).await
        {
            tracing::error!(error = %e, "UDS server error");
        }
    });

    // Send initial state events immediately - UDS server will broadcast them
    // to clients as they connect via the initial state in handle_client
    event::global_broadcaster().listening_started();
    if cfg.auto_enter {
        event::global_broadcaster().auto_enter_enabled();
    }

    // Restore persisted focus/pause state now that the broadcaster and UDS
    // server are up (so any events it emits reach connected clients) but
    // before audio gating begins. The function is a no-op when there is
    // nothing to restore; owned by `focus_state`, just invoked here.
    crate::always::focus_state::restore_on_startup();

    // Keyboard hooks are not needed for UDS bind — start after the socket is live.
    keyboard::start_keyboard_listener()?;

    // Load the real transcriber off the hot path (local models can take seconds).
    let cfg_for_build = cfg.clone();
    let registry_for_build = registry_placeholder.clone();
    let active_for_build = Arc::clone(&active_placeholder);
    std::thread::Builder::new()
        .name("transcriber-init".into())
        .spawn(move || {
            match build_transcriber(&cfg_for_build, &registry_for_build) {
                Ok(transcriber) => {
                    *active_for_build.write() = Arc::clone(&transcriber);
                    // Wake any utterance threads that are blocked in the
                    // PendingTranscriber waiting for init to finish.
                    ready_signal.set(transcriber);
                    tracing::info!("transcriber_ready");
                }
                Err(e) => tracing::error!(error = %e, "transcriber_init_failed"),
            }
        })
        .expect("spawn transcriber-init thread");

    // Now do the expensive operations after UDS is accepting connections
    let active = active_placeholder;

    // Heartbeat task: emit Heartbeat every 5s so connected GUI clients can
    // detect a dead/stalled daemon via watchdog timeout.
    let _heartbeat_handle = rt.spawn(async {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        // First tick fires immediately; skip it to avoid double-emit at startup.
        interval.tick().await;
        loop {
            interval.tick().await;
            event::global_broadcaster().send(event::DaemonEvent::Heartbeat);
        }
    });

    // Spawn the passive correction-capture watcher when the user has
    // opted in via the `passive_correction_capture` preference. The
    // module is a no-op when `enabled == false`; we still call it so
    // the daemon's wiring is the single source of truth and a future
    // toggle from the UI can simply restart the daemon. Failure to
    // load preferences is non-fatal — we treat it as "feature off".
    let passive_correction_enabled = crate::db::open()
        .and_then(|conn| crate::db::get_preferences(&conn))
        .ok()
        .and_then(|p| p.passive_correction_capture)
        .unwrap_or(false);
    clipboard_watcher::spawn_if_enabled(rt.handle(), passive_correction_enabled);

    // Idle-pause watchdog. Spawns at most one task; no-op when
    // `idle_pause_secs == 0`. Lives for the daemon lifetime.
    idle_watcher::spawn(rt.handle(), cfg.idle_pause_secs, cfg.idle_pause_action);

    let mut last_process = Instant::now() - Duration::from_secs(10);
    let mut last_dup_check = Instant::now();

    // Initialize microphone monitor for auto-pause functionality
    let mut mic_monitor = mic_monitor::MicrophoneMonitor::new();
    let mut auto_paused_for_mic = false;

    loop {
        if last_dup_check.elapsed() >= Duration::from_secs(30) {
            last_dup_check = Instant::now();
            daemon::reconcile_duplicate_processes();
        }

        // Check if other apps are using the microphone
        match mic_monitor.is_microphone_in_use() {
            Ok(true) if !auto_paused_for_mic => {
                // Another app started using the microphone - pause Always
                if let Ok(users) = mic_monitor.get_microphone_users() {
                    let app_list = if users.is_empty() {
                        "another application".to_string()
                    } else {
                        users.join(", ")
                    };
                    tracing::info!(apps = %app_list, "microphone_auto_paused");

                    // Log the auto-pause event
                    log.write(Event::MicrophoneAutoPaused { apps: &app_list });
                }
                let (_effective, changed) = pause::set_paused(true);
                event::global_broadcaster().master_pause_changed(true);
                if changed {
                    pause::dictation_buffer_clear();
                    event::global_broadcaster().paused();
                }
                auto_paused_for_mic = true;
            }
            Ok(false) if auto_paused_for_mic => {
                // Microphone is no longer in use - resume Always
                tracing::info!("microphone_auto_resumed");

                // Log the auto-resume event
                log.write(Event::MicrophoneAutoResumed);

                // Clear the idle-auto-paused flag so subsequent
                // audio-output stop events resume the daemon. Previously
                // we set `paused=false` but left `idle_auto_paused=true`,
                // so `uds_server::NotifySystemAudioState{playing:false}`
                // would refuse to auto-resume (it gates on
                // `!is_idle_auto_paused()`) and the daemon stayed paused
                // even though the user could see "Listening" in the UI.
                pause::set_idle_auto_paused(false);
                let (effective, changed) = pause::set_paused(false);
                event::global_broadcaster().master_pause_changed(false);
                // Reset the voice-seen timestamp so the idle watchdog
                // doesn't immediately re-pause us based on the gap that
                // accumulated while we were mic-paused.
                pause::mark_voice_seen();
                if changed {
                    if effective {
                        event::global_broadcaster().paused();
                    } else {
                        event::global_broadcaster().resumed();
                    }
                }
                auto_paused_for_mic = false;
            }
            Err(e) => {
                tracing::warn!(error = %e, "microphone_check_failed");
            }
            _ => {} // No change in microphone state
        }

        if pause::is_paused() {
            // Wake-on-voice while idle-paused: keep the mic hot enough to
            // detect speech without running the full VAD/transcribe loop.
            if pause::is_idle_auto_paused()
                && !pause::is_master_paused()
                && !auto_paused_for_mic
                && vad::poll_speech_energy(&active_cfg.read()).unwrap_or(false)
            {
                pause::set_idle_auto_paused(false);
                pause::mark_voice_seen();
                let (effective, changed) = pause::recompute_effective();
                event::global_broadcaster().idle_auto_resumed();
                if changed && !effective {
                    event::global_broadcaster().resumed();
                }
                tracing::info!(effective, changed, "idle_wake_on_voice");
                continue;
            }
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        // Note: Removed auto_enter config reload that was causing race conditions
        // Auto-enter state is now managed consistently through keyboard shortcuts and settings UI

        // Take a cheap snapshot of the active transcriber per utterance —
        // if the user swaps models mid-session, the next loop iteration
        // picks up the new backend automatically.
        let transcriber_snapshot = active.read().clone();
        if let Err(e) = process_one(
            &active_cfg,
            &mut log,
            &mut last_process,
            rt.handle(),
            &transcriber_snapshot,
        ) {
            tracing::error!(error = %e, "voice_processing_error");
            log.write(Event::Error {
                message: &format!("Voice processing error: {:#}", e),
            });
            // Don't exit - continue running
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

/// Map a record/transcribe error to a (kind, message) pair for the
/// `TranscriptionFailed` event. `kind` is a stable machine tag the GUI can
/// branch on; `message` is short human-facing text. Best-effort substring
/// matching on the anyhow error chain since the STT layer surfaces opaque strings.
fn classify_transcription_error(err: &anyhow::Error) -> (&'static str, String) {
    let s = format!("{err:#}").to_lowercase();
    if s.contains("401")
        || s.contains("unauthorized")
        || s.contains("invalid api key")
        || s.contains("invalid_api_key")
        || s.contains("api key")
    {
        ("auth", "Invalid or missing Groq API key".to_string())
    } else if s.contains("429")
        || s.contains("rate limit")
        || s.contains("quota")
        || s.contains("too many requests")
    {
        ("quota", "Groq rate limit or quota exceeded".to_string())
    } else if s.contains("timed out")
        || s.contains("timeout")
        || s.contains("dns")
        || s.contains("connect")
        || s.contains("network")
        || s.contains("sending request")
    {
        ("network", "Network error reaching Groq".to_string())
    } else {
        ("error", "Transcription failed".to_string())
    }
}

fn process_one(
    active_cfg: &ActiveConfig,
    log: &mut Logger,
    last_process: &mut Instant,
    rt: &Handle,
    transcriber: &Arc<dyn Transcriber>,
) -> Result<()> {
    let cfg = active_cfg.read();
    let record_result = match vad::record_utterance(&cfg, log, transcriber) {
        Ok(r) => r,
        Err(e) => {
            let (kind, message) = classify_transcription_error(&e);
            event::global_broadcaster().transcription_failed(kind, message);
            return Err(e).context("failed to record/transcribe utterance");
        }
    };
    match record_result {
        vad::RecordResult::Speech {
            text,
            energy,
            transcription,
        } => {
            handle_speech(&cfg, log, &text, energy, &transcription, last_process, rt)?;
        }
        vad::RecordResult::Silence => {
            // Don't log silence events - they're too frequent and not useful
        }
        vad::RecordResult::Timeout => log.write(Event::Timeout),
        vad::RecordResult::DroppedLowEnergy { energy } => {
            log.write(Event::DroppedLowEnergy { energy });
            event::global_broadcaster().low_microphone_volume_maybe(energy);
        }
        vad::RecordResult::DroppedNoise { raw } => {
            tracing::debug!(raw, "dropped_noise");
            log.write(Event::DroppedNoise { raw: &raw });
        }
    }
    Ok(())
}

fn handle_speech(
    cfg: &AlwaysConfig,
    log: &mut Logger,
    text: &str,
    energy: f64,
    transcription: &crate::stt::TranscriptionResult,
    last_process: &mut Instant,
    rt: &Handle,
) -> Result<()> {
    let now = Instant::now();
    let action = classify_transcription(cfg, text, transcription, now, *last_process);

    // Cooldown is observable in the decision but does not advance
    // last_process — let the next utterance re-check.
    if !matches!(action, SpeechAction::InCooldown) {
        *last_process = now;
        log.write(Event::Transcribed { text, energy });
    }

    match action {
        SpeechAction::InCooldown => Ok(()),
        SpeechAction::Rejected { reason } => {
            // Privacy: gate raw transcript text behind `should_log_transcripts`
            // — without the gate, every release-build daemon writes user
            // speech to its info-level logs, which then propagate to any
            // central log collector. The structured `Event::Filtered`
            // record below is still written via the audited log path,
            // which honors the same gate.
            if crate::always::telemetry::should_log_transcripts() {
                tracing::info!(text, filter_reason = %reason, "filtered");
            } else {
                tracing::info!(
                    chars = text.chars().count(),
                    filter_reason = %reason,
                    "filtered"
                );
            }
            // Re-derive the structured filter result for logging fidelity.
            let filter_result = filter::should_accept_with_reason(text, cfg);
            log.write(Event::Filtered {
                text,
                energy,
                reason: filter_result,
            });
            pause::set_last_filtered(text);
            event::global_broadcaster().voice_activity_ended();
            event::global_broadcaster().transcription_filtered(reason);
            Ok(())
        }
        SpeechAction::Hallucinated { reason } => {
            if crate::always::telemetry::should_log_transcripts() {
                tracing::info!(text, reason = %reason, "hallucination filtered");
            } else {
                tracing::info!(
                    chars = text.chars().count(),
                    reason = %reason,
                    "hallucination filtered"
                );
            }
            log.write(Event::Filtered {
                text,
                energy,
                reason: filter::FilterReason::HardPhrase(reason.clone()),
            });
            pause::set_last_filtered(text);
            event::global_broadcaster().voice_activity_ended();
            event::global_broadcaster().transcription_filtered(reason);
            Ok(())
        }
        SpeechAction::Paste { text: transformed } => {
            // Log the raw Whisper transcript for debugging STT quality issues
            // Privacy: gate transcript text behind `should_log_transcripts`
            if crate::always::telemetry::should_log_transcripts() {
                tracing::info!(
                    stage = "whisper_raw",
                    transcript = %text,
                    "speech-to-text output"
                );
            } else {
                tracing::info!(
                    chars = text.chars().count(),
                    stage = "whisper_raw",
                    "speech-to-text output"
                );
            }

            // Resume-merge must use the full blocking pipeline — async
            // grammar patch only replaces the last paste via Cmd+Z, which
            // cannot safely rewrite a merged delta mid-countdown.
            let merge_active = pause::dictation_buffer_text().is_some();

            // Short-utterance bypass: tiny inputs like "yes", "ok", "done"
            // get pasted verbatim. Longer utterances: acoustic fix-up is
            // sync (fast); LLM grammar runs async after paste when enabled
            // so the user sees text immediately (~300-800ms sooner).
            let (final_text, grammar_patch_async) = if is_short_utterance(&transformed) {
                tracing::info!(
                    stage = "short_utterance_bypass",
                    text = %transformed,
                    "skipping correction stack — too short to benefit"
                );
                (transformed.clone(), false)
            } else {
                // Single-paste policy: correct synchronously, then paste the
                // final text exactly ONCE. The old "paste acoustic now, patch
                // via Cmd+Z + repaste when the LLM returns" path surfaced text
                // a few hundred ms sooner but produced a DOUBLE transcript
                // whenever the undo failed to land — user pressed Enter first,
                // the app has non-standard undo (Slack, terminals, web
                // contenteditable), or focus shifted. One clean paste of the
                // corrected text is worth the small extra latency.
                let acoustic = apply_acoustic_corrections(&transformed);
                (apply_grammar_blocking(&acoustic, cfg, rt), false)
            };

            // Privacy: gate transcript text behind `should_log_transcripts`
            if crate::always::telemetry::should_log_transcripts() {
                tracing::info!(
                    stage = "final_result",
                    text = %final_text,
                    energy = energy,
                    "transcript ready for pasting"
                );
            } else {
                tracing::info!(
                    stage = "final_result",
                    chars = final_text.chars().count(),
                    energy = energy,
                    "transcript ready for pasting"
                );
            }
            log.write(Event::Pasting {
                raw: text,
                processed: &final_text,
                energy,
            });

            event::global_broadcaster().transcript_final(final_text.clone());

            // Resume-merge path: if the auto-enter countdown is still
            // active from the previous utterance, the user paused only
            // briefly and is now continuing. Append the new transcript
            // to the existing buffered text (preserving sentence casing
            // — see `merge_dictation`) instead of pasting it as a fresh,
            // re-capitalized sentence. Cancels the in-flight countdown
            // and reschedules so the timer restarts from the resumed
            // speech, giving the user the full delay to keep going.
            // Merge whenever the dictation buffer still holds the
            // previous paste — not just when the countdown is still
            // ticking. The voice-activity hook in `vad.rs` cancels
            // the countdown as soon as the user resumes speaking,
            // which previously broke the merge gate (countdown_active
            // flipped to false before the new utterance finalised) and
            // produced split-sentence pastes. The buffer is still
            // cleared on Return commit / pause / explicit user cancel,
            // so this can't leak across sessions.
            let (paste_clipboard, buffer_text) = if merge_active {
                let previous = pause::dictation_buffer_text().unwrap_or_default();
                let (joined, delta) =
                    merge_dictation_with(&cfg.localization, &previous, &final_text);
                tracing::info!(
                    stage = "dictation_merge",
                    previous = %previous,
                    addition = %final_text,
                    delta = %delta,
                    joined = %joined,
                    "merging resumed utterance into in-flight dictation"
                );
                pause::countdown_request_cancel();
                // Delta carries its own leading space when needed; no
                // trailing space here because we want the merged buffer
                // to end exactly at the last character pasted so a
                // subsequent merge picks up correct sentence-end state.
                (delta, joined)
            } else {
                // Fresh paste — preserve the historical trailing space
                // on the clipboard payload (clients expect it).
                (format!("{} ", final_text), final_text.clone())
            };

            let dup_window = paste_dedupe_window(cfg.cooldown_ms);
            let paste_payload = paste_clipboard.trim();
            if pause::should_suppress_duplicate_paste(paste_payload, dup_window) {
                tracing::info!(
                    text = %paste_payload,
                    dup_window_ms = dup_window.as_millis() as u64,
                    "duplicate_paste_suppressed"
                );
                event::global_broadcaster().voice_activity_ended();
                return Ok(());
            }

            if !pause::try_begin_paste() {
                tracing::info!(
                    text = %paste_payload,
                    "duplicate_paste_suppressed_in_flight"
                );
                event::global_broadcaster().voice_activity_ended();
                return Ok(());
            }

            // Focus moved to a paused app (or master/idle pause kicked in)
            // between when we started recording and now. Drop the paste so
            // the transcript doesn't leak into the wrong window once the
            // user gets there. Surface as `transcription_filtered` so the
            // GUI flashes a "not pasted" overlay — same channel as the
            // cmd-held drop below.
            if pause::is_paused() {
                tracing::info!(
                    text = %paste_payload,
                    "paste_dropped_paused_mid_utterance"
                );
                log.write(Event::Error {
                    message: "Skipped paste: app paused mid-utterance",
                });
                event::global_broadcaster()
                    .transcription_filtered("App paused mid-utterance — not pasted");
                pause::dictation_buffer_clear();
                pause::end_paste();
                return Ok(());
            }

            if daemon::list_daemon_pids().len() > 1 {
                daemon::reconcile_duplicate_processes();
            }

            // Snapshot the user's clipboard BEFORE we overwrite it with the
            // transcript, so the synchronous / no-grammar paste paths can
            // restore it after the Cmd+V is consumed (guarded — see the
            // restore call below). Best-effort: if pbpaste fails we simply
            // skip the restore rather than abort the paste. Plain-text only;
            // non-text flavors aren't preserved (documented in paste.rs).
            // The clipboard payload we are about to write — kept so the
            // guarded restore can confirm nothing else clobbered it since.
            let prev_clipboard = paste::read_clipboard_text().ok();
            let written_clipboard = paste_clipboard.clone();

            // NOTE: the paste-in-flight lock is held from `try_begin_paste()`
            // above. Every early-return below MUST release it via
            // `end_paste()` or the daemon goes permanently mute (future
            // utterances drop as "in_flight"). A bare `?` here previously
            // leaked the lock when pbcopy failed.
            if let Err(err) = paste::copy_to_clipboard(paste_clipboard) {
                tracing::warn!(error = %err, "clipboard_copy_failed");
                log.write(Event::Error {
                    message: "Skipped paste: clipboard copy failed",
                });
                event::global_broadcaster().transcription_filtered("Clipboard error — not pasted");
                event::global_broadcaster().voice_activity_ended();
                pause::dictation_buffer_clear();
                pause::end_paste();
                return Ok(());
            }

            // Skip paste if Command is held — likely a shortcut in flight.
            // Surface the drop on the status overlay (same channel as
            // hallucination / hard-filter rejections) so the user knows
            // their utterance was heard but intentionally not pasted.
            if keyboard::is_cmd_held() {
                tracing::debug!("skipped_paste_cmd_held");
                log.write(Event::Error {
                    message: "Skipped paste: Command key held",
                });
                event::global_broadcaster().transcription_filtered("Held Command key — not pasted");
                pause::dictation_buffer_clear();
                pause::end_paste();
                return Ok(());
            }

            // Always paste WITHOUT enter — the auto-enter Return key
            // is now decoupled, gated behind an optional countdown so
            // the user can intercept (any key cancels). When
            // `auto_enter_delay_ms == 0` the countdown helper presses
            // Return immediately, preserving the legacy behavior.
            if let Err(err) = paste::paste_text(false) {
                pause::end_paste();
                return Err(err);
            }
            let pasted_at = Instant::now();
            // Snapshot the focused app at paste time. If the user switches
            // apps (or submits the message) before the grammar patch fires,
            // Cmd+Z would land in the wrong window and create a double paste.
            let pasted_to_app = pause::current_app();

            // Add a small delay after paste to prevent double-paste when
            // the user makes a break mid-utterance. This gives the system
            // time to settle before releasing the paste-in-flight lock.
            std::thread::sleep(Duration::from_millis(200));

            if grammar_patch_async {
                // TODO(clipboard-restore): the async-grammar path ends with
                // `replace_via_undo` INSIDE `spawn_grammar_patch` (undo +
                // copy(corrected) + Cmd+V), which is the LAST clipboard op
                // for this utterance. The guarded restore must run after
                // that final repaste and re-check against the *corrected*
                // clipboard payload, not `written_clipboard`. Threading the
                // snapshot through and re-guarding correctly there is more
                // intricate than the synchronous case, so the clipboard
                // restore for the async-grammar path is deliberately
                // deferred. The synchronous + no-grammar path below restores.
                spawn_grammar_patch(rt, cfg, final_text.clone(), pasted_at, pasted_to_app);
            } else {
                pause::end_paste();
                // Synchronous / no-grammar path: this was the last clipboard
                // op for the utterance. Give the target app time to consume
                // the Cmd+V, then restore the user's prior clipboard — but
                // only if our transcript is still on the pasteboard (i.e.
                // the user/another app didn't copy something fresh in the
                // meantime). See `restore_clipboard_if_unchanged`.
                if let Some(prev) = prev_clipboard {
                    std::thread::sleep(Duration::from_millis(150));
                    if let Err(err) =
                        paste::restore_clipboard_if_unchanged(&prev, &written_clipboard)
                    {
                        tracing::warn!(error = %err, "clipboard_restore_failed");
                    }
                }
            }

            let auto_enter_effective =
                per_app::effective_auto_enter(pause::is_auto_enter_enabled());
            if auto_enter_effective && should_auto_enter_for_text(&buffer_text) {
                // Hold the merged buffer so the NEXT utterance can
                // append again if it arrives before the countdown
                // fires. Set BEFORE schedule() so even a 0-delay
                // immediate-Return path sees the buffer for clearing.
                pause::dictation_buffer_set(buffer_text.clone());
                let delay = per_app::effective_auto_enter_delay_ms(cfg.auto_enter_delay_ms);
                auto_enter_countdown::schedule(rt, delay);
            } else {
                if auto_enter_effective {
                    tracing::info!(
                        text = %buffer_text,
                        words = word_count(&buffer_text),
                        "auto_enter_suppressed_short_transcript"
                    );
                }
                // No auto-enter, or transcript too short to submit — no
                // merge window. Drop any stale buffer so the next utterance
                // doesn't accidentally try to merge with text the user has
                // long since committed.
                pause::dictation_buffer_clear();
            }

            // Snapshot the freshly-pasted text as the diff baseline for
            // the manual-correction capture pipeline. We store the
            // post-vocabulary, post-postprocess `final_text` (i.e. the
            // exact bytes the user sees in their app) so the ⌃⌥X hotkey
            // and passive clipboard watcher can compare it against the
            // user's selection without having to reconstruct what was
            // pasted. Only runs on the actually-pasted branch — if
            // Cmd was held the early-return above skipped paste and
            // there's nothing for a correction-diff to anchor on.
            // For merge: store the full joined text so correction tools
            // diff against what's actually on screen.
            pause::set_last_pasted(buffer_text);
            // Explicit voice-activity-ended after a successful paste so the
            // Swift overlay clears cleanly. Without this, the next VAD loop
            // iteration can pick up residual mic energy and fire a new
            // VoiceActivityDetected before the overlay from this utterance
            // has hidden — causing the "listening appears again" flash.
            event::global_broadcaster().voice_activity_ended();
            Ok(())
        }
    }
}

/// Maximum word count for the short-utterance bypass. Inputs at or below
/// this go straight to paste without the acoustic+LLM correction stack.
pub const SHORT_UTTERANCE_MAX_WORDS: usize = 2;

/// Maximum character count (after trim) for the short-utterance bypass.
/// Belt-and-braces companion to `SHORT_UTTERANCE_MAX_WORDS` so a single
/// long word ("acknowledged") still flows through correction, but a
/// two-letter "ok" does not.
pub const SHORT_UTTERANCE_MAX_CHARS: usize = 8;

/// True when the transcript is short enough to skip the correction stack
/// (`≤ SHORT_UTTERANCE_MAX_WORDS` words OR `≤ SHORT_UTTERANCE_MAX_CHARS`
/// trimmed chars). Empty input also returns true — nothing to correct.
pub fn is_short_utterance(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return true;
    }
    let words = word_count(trimmed);
    let chars = trimmed.chars().count();
    words <= SHORT_UTTERANCE_MAX_WORDS || chars <= SHORT_UTTERANCE_MAX_CHARS
}

const AUTO_ENTER_MIN_WORDS: usize = 3;

fn should_auto_enter_for_text(text: &str) -> bool {
    word_count(text) >= AUTO_ENTER_MIN_WORDS
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Stage 1 — acoustic glossary fix-up (Soundex + Levenshtein).
fn apply_acoustic_corrections(text: &str) -> String {
    let custom_words = crate::glossary::user_glossary_terms();
    let (acoustic, acoustic_subs) = crate::always::text_match::apply_custom_words(
        text,
        &custom_words,
        crate::always::text_match::DEFAULT_THRESHOLD,
    );
    if !acoustic_subs.is_empty() {
        let pairs: Vec<String> = acoustic_subs
            .iter()
            .map(|(w, r)| format!("{w}->{r}"))
            .collect();
        tracing::info!(
            stage = "acoustic_match",
            before = %text,
            after = %acoustic,
            count = acoustic_subs.len(),
            pairs = %pairs.join(", "),
            "glossary corrections applied"
        );
    } else {
        tracing::debug!(
            stage = "acoustic_match",
            text = %acoustic,
            "no glossary corrections needed"
        );
    }
    acoustic
}

/// Stage 2 — blocking LLM grammar cleanup (merge path or grammar disabled).
fn apply_grammar_blocking(text: &str, cfg: &AlwaysConfig, rt: &Handle) -> String {
    if cfg.postprocess_config.grammar_correction_enabled
        && let Some(ref pp) = cfg.post_processor
    {
        let started = Instant::now();
        // Bound the blocking grammar call: this runs on the main loop
        // thread (merge path / async-grammar disabled), so a hung cloud
        // endpoint would otherwise freeze the whole daemon — no new
        // utterance could be recorded until it returned. On timeout we
        // fall back to the acoustic `text` exactly like the error arm
        // below, so behavior on a stalled endpoint matches a failed one.
        // Returning the fallback string directly (rather than synthesizing
        // an Err) keeps the result type identical to `pp.process(..)` and
        // avoids reaching into its concrete error type. `Duration` is
        // imported at the top of this file.
        const GRAMMAR_BLOCKING_TIMEOUT: Duration = Duration::from_secs(8);
        let timed = rt.block_on(async {
            tokio::time::timeout(GRAMMAR_BLOCKING_TIMEOUT, pp.process(text, None)).await
        });
        let process_result = match timed {
            Ok(inner) => inner,
            Err(_elapsed) => {
                tracing::warn!(
                    timeout_secs = GRAMMAR_BLOCKING_TIMEOUT.as_secs(),
                    fallback_text = %text,
                    "grammar correction timed out, using acoustic match result"
                );
                return text.to_string();
            }
        };
        match process_result {
            Ok(cleaned) => {
                let elapsed_ms = started.elapsed().as_millis() as u64;
                if text != cleaned {
                    tracing::info!(
                        stage = "grammar_correction",
                        before = %text,
                        after = %cleaned,
                        elapsed_ms = elapsed_ms,
                        "grammar correction applied"
                    );
                } else {
                    tracing::debug!(
                        stage = "grammar_correction",
                        text = %cleaned,
                        elapsed_ms = elapsed_ms,
                        "no grammar changes"
                    );
                }
                cleaned
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    fallback_text = %text,
                    "grammar correction failed, using acoustic match result"
                );
                text.to_string()
            }
        }
    } else {
        tracing::debug!(
            stage = "grammar_correction",
            text = %text,
            "grammar correction disabled"
        );
        text.to_string()
    }
}

/// After paste-first, patch grammar via Cmd+Z + repaste when the LLM returns.
///
/// Retired by the single-paste policy (see the `apply_grammar_blocking` branch
/// in the main loop) because the undo step doubled the transcript whenever it
/// failed to land. Kept behind `dead_code` so the paste-first path can be
/// restored if a select-and-replace correction (no Cmd+Z) is built later.
#[allow(dead_code)]
fn spawn_grammar_patch(
    rt: &Handle,
    cfg: &AlwaysConfig,
    acoustic_pasted: String,
    pasted_at: Instant,
    pasted_to_app: Option<String>,
) {
    let Some(pp) = cfg.post_processor.clone() else {
        return;
    };
    if !cfg.postprocess_config.grammar_correction_enabled {
        return;
    }

    rt.spawn(async move {
        // Release the paste-in-flight lock held since the initial Cmd+V.
        struct PasteRelease;
        impl Drop for PasteRelease {
            fn drop(&mut self) {
                pause::end_paste();
            }
        }
        let _paste_release = PasteRelease;

        let started = Instant::now();
        let cleaned = match pp.process(&acoustic_pasted, None).await {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    fallback_text = %acoustic_pasted,
                    "async grammar patch failed"
                );
                return;
            }
        };

        if cleaned == acoustic_pasted
            || normalize_for_paste_dedupe(&cleaned) == normalize_for_paste_dedupe(&acoustic_pasted)
        {
            tracing::debug!(
                stage = "grammar_patch",
                text = %cleaned,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "no grammar changes"
            );
            return;
        }

        if pasted_at.elapsed() > paste::GRAMMAR_PATCH_MAX_AGE {
            tracing::debug!(
                stage = "grammar_patch",
                elapsed_since_paste_ms = pasted_at.elapsed().as_millis() as u64,
                "skipped — paste too old to safely undo"
            );
            return;
        }

        if keyboard::is_cmd_held() {
            tracing::debug!("grammar_patch_skipped_cmd_held");
            return;
        }

        // Abort if the focused app changed since the paste. The most common
        // case: user pressed Enter to submit the message before the LLM
        // corrected it. Cmd+Z would land in the wrong window (e.g., a code
        // editor) and the subsequent Cmd+V would create a duplicate paste.
        let current_app = pause::current_app();
        if current_app != pasted_to_app {
            tracing::info!(
                pasted_to = ?pasted_to_app,
                current = ?current_app,
                "grammar_patch_aborted_app_switched"
            );
            return;
        }

        // Abort when auto-enter is enabled and the dictation buffer was
        // cleared — auto_enter_countdown clears it the moment Return fires.
        // If the buffer is gone, the message was already submitted: Cmd+Z
        // would do nothing (empty input) and Cmd+V would ghost-paste the
        // corrected text as a new message — the "double transcript" bug.
        if pause::is_auto_enter_enabled() && pause::dictation_buffer_text().is_none() {
            tracing::info!("grammar_patch_aborted_message_submitted");
            return;
        }

        let clipboard = format!("{cleaned} ");
        if let Err(err) = paste::replace_via_undo(&clipboard) {
            tracing::warn!(
                error = %err,
                "grammar patch replace failed; acoustic paste kept"
            );
            return;
        }

        tracing::info!(
            stage = "grammar_patch",
            before = %acoustic_pasted,
            after = %cleaned,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "async grammar correction applied"
        );
        pause::set_last_pasted(cleaned.clone());
        // Notify the UI that grammar correction silently mutated the text.
        event::global_broadcaster().grammar_corrected(acoustic_pasted.clone(), cleaned.clone());
        event::global_broadcaster().transcript_final(cleaned);
    });
}

fn print_banner(cfg: &AlwaysConfig) {
    tracing::info!(
        energy_threshold = cfg.energy_threshold,
        silence_secs = cfg.silence_secs,
        auto_enter = cfg.auto_enter,
        filter_enabled = cfg.filter_enabled,
        log_path = %cfg.log_path.display(),
        "daemon_banner"
    );
}

#[cfg(test)]
mod tests {
    // Pure-logic tests for the speech pipeline (classifier, dictation
    // merge, cooldown window, localization seam) live in
    // `super::speech_action::tests` — they were extracted alongside
    // the functions they cover so daemon orchestration and pure
    // decision logic can be exercised in isolation.

    use super::{
        AUTO_ENTER_MIN_WORDS, SHORT_UTTERANCE_MAX_CHARS, SHORT_UTTERANCE_MAX_WORDS,
        is_short_utterance, should_auto_enter_for_text,
    };

    #[test]
    fn short_utterance_bypass_matches_thresholds() {
        // Empty / whitespace — nothing to correct.
        assert!(is_short_utterance(""));
        assert!(is_short_utterance("   "));

        // Single short word — bypass.
        assert!(is_short_utterance("yes"));
        assert!(is_short_utterance("ok"));
        assert!(is_short_utterance("done"));
        assert!(is_short_utterance("  done  "), "trim before checking");

        // Two short words — bypass (word count threshold).
        assert!(is_short_utterance("yes please"));
        assert!(is_short_utterance("all good"));

        // Spec is `≤2 words OR ≤8 chars` (intentionally permissive) —
        // a single long word still hits the bypass via word count.
        assert!(
            is_short_utterance("acknowledged"),
            "single word always bypasses regardless of length"
        );

        // Three short words but ≤8 chars — char count gates it.
        assert!(
            is_short_utterance("a b c d"),
            "7-char 4-word input still bypasses via char threshold"
        );

        // Three words AND >8 chars — runs full pipeline.
        assert!(
            !is_short_utterance("yes no maybe"),
            "3 words above char threshold must run through correction"
        );
        assert!(
            !is_short_utterance("this is a sentence that needs cleanup"),
            "long multi-word input must run through correction"
        );

        // Sanity: thresholds are at the documented values.
        assert_eq!(SHORT_UTTERANCE_MAX_WORDS, 2);
        assert_eq!(SHORT_UTTERANCE_MAX_CHARS, 8);
    }

    #[test]
    fn auto_enter_requires_three_or_more_words() {
        assert!(!should_auto_enter_for_text(""));
        assert!(!should_auto_enter_for_text("   "));
        assert!(!should_auto_enter_for_text("hello"));
        assert!(!should_auto_enter_for_text("hello there"));

        assert!(should_auto_enter_for_text("hello over there"));
        assert!(should_auto_enter_for_text("  hello over there  "));
        assert!(should_auto_enter_for_text("one two three four"));

        assert_eq!(AUTO_ENTER_MIN_WORDS, 3);
    }
}
