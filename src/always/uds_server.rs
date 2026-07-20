use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use super::event::{DaemonCommand, DaemonEvent, global_broadcaster};
use super::event_loop::{ActiveConfig, ActiveTranscriber};
use super::pause;
use crate::managers::model_registry::{ModelEvent, ModelRegistry};
use crate::stt_dispatch::{TranscriberBackendChoice, build_transcriber};

/// Everything `execute_command` needs to handle the new
/// `models.*` commands. Bundled into a single struct so we don't grow
/// a 5-argument function signature every time we add a stateful
/// command. Cloned per call — all fields are cheap to clone.
#[derive(Clone)]
struct ModelCommandCtx {
    registry: ModelRegistry,
    active: ActiveTranscriber,
    /// Live config shared with the main loop. Mutated by
    /// `ApplyRuntimePreferences` and when swapping transcriber backend.
    cfg: ActiveConfig,
}

// DoS protection constants
const MAX_COMMAND_LINE_LENGTH: usize = 1024; // 1KB max command size
const COMMANDS_PER_SECOND_LIMIT: u32 = 10; // Max 10 commands per second per client

/// Upper bound on a single write to a client. A client that connects but
/// never reads (full socket buffer, crashed mid-handshake, or a
/// connect-and-drop liveness probe) would otherwise block the handler
/// task forever, holding a `ClientGuard` that keeps `CONNECTED_CLIENTS > 0`
/// and defeats the orphan watchdog.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the daemon stays alive after the last UDS client disconnects.
/// Prevents stale daemons surviving indefinitely when the Mac app quits.
/// Short because dictation is already muted after NO_GUI_MUTE_GRACE_SECS —
/// this is just process cleanup.
const ORPHAN_TIMEOUT_SECS: u64 = 30;

/// How long a GUI-spawned daemon keeps dictating after its last UDS client
/// disconnects. Long enough to survive a GUI restart / reconnect blip,
/// short enough that speech never pastes into windows with the app gone.
const NO_GUI_MUTE_GRACE_SECS: u64 = 5;

static CONNECTED_CLIENTS: AtomicUsize = AtomicUsize::new(0);

/// Rate limiter for UDS commands
struct RateLimiter {
    last_reset: Instant,
    command_count: u32,
}

impl RateLimiter {
    fn new() -> Self {
        Self {
            last_reset: Instant::now(),
            command_count: 0,
        }
    }

    fn check(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_reset);

        // Reset counter if more than 1 second has passed
        if elapsed >= Duration::from_secs(1) {
            self.last_reset = now;
            self.command_count = 0;
        }

        // Check if we've exceeded the rate limit
        if self.command_count >= COMMANDS_PER_SECOND_LIMIT {
            tracing::warn!(commands = self.command_count, "uds_rate_limit_exceeded");
            return false;
        }

        self.command_count += 1;
        true
    }
}

/// Validate a command line before processing
fn validate_command(line: &str) -> Result<()> {
    // Check line length
    if line.len() > MAX_COMMAND_LINE_LENGTH {
        anyhow::bail!(
            "Command line exceeds maximum length of {} bytes",
            MAX_COMMAND_LINE_LENGTH
        );
    }

    // Check for basic JSON structure - must start with { and end with }
    let trimmed = line.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        anyhow::bail!("Command must be valid JSON object");
    }

    // Check for suspicious patterns (e.g., nested objects that could cause deep recursion)
    let brace_depth = trimmed.chars().filter(|&c| c == '{').count();
    if brace_depth > 10 {
        anyhow::bail!("Command has excessive nesting depth");
    }

    Ok(())
}

/// RAII guard that decrements connected client count on drop.
struct ClientGuard;

impl Drop for ClientGuard {
    fn drop(&mut self) {
        let prev = CONNECTED_CLIENTS.fetch_sub(1, Ordering::Relaxed);
        let remaining = prev - 1;
        tracing::info!(clients = remaining, "uds_client_disconnected");
        // Safety net: if the last client vanished (e.g. a controller crashed
        // without sending SetConsumeMode{false}), drop consume mode so normal
        // dictation + pasting resume for whoever connects next.
        if remaining == 0 && crate::always::pause::is_consume_mode() {
            crate::always::pause::set_consume_mode(false);
            tracing::info!("consume_mode_cleared_on_last_disconnect");
        }
    }
}

/// Unix Domain Socket path for daemon-to-GUI communication
#[cfg(unix)]
pub fn socket_path() -> Result<PathBuf> {
    let path = if cfg!(target_os = "macos") {
        // macOS: Use ~/Library/Caches/Always/always.sock
        let home = std::env::var("HOME").context("HOME environment variable not set")?;
        PathBuf::from(home)
            .join("Library")
            .join("Caches")
            .join("Always")
            .join("always.sock")
    } else {
        // Linux: Use XDG_RUNTIME_DIR or fallback to /tmp
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(runtime_dir).join("always.sock")
        } else {
            PathBuf::from("/tmp/always.sock")
        }
    };

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create socket directory")?;
    }

    Ok(path)
}

#[cfg(not(unix))]
pub fn socket_path() -> Result<PathBuf> {
    anyhow::bail!("Unix domain sockets not supported on this platform")
}

/// Start the Unix Domain Socket server.
///
/// Takes the model registry and the active-transcriber lock so the
/// `models.*` commands can mutate them and the `ModelRegistry::subscribe`
/// channel can be bridged into the global event broadcaster.
#[cfg(unix)]
pub async fn start_server(
    registry: ModelRegistry,
    active: ActiveTranscriber,
    cfg: ActiveConfig,
) -> Result<()> {
    // Bridge registry events → global daemon broadcaster so every
    // connected UDS client sees download/extract/verify progress on
    // the same channel as the rest of the daemon's events.
    spawn_registry_event_bridge(registry.clone());

    let ctx = ModelCommandCtx {
        registry,
        active,
        cfg,
    };

    let socket_path = socket_path()?;

    // Do not unlink a live socket — a second daemon would otherwise bind a
    // new path while the first keeps the mic (duplicate transcripts).
    if socket_path.exists() {
        if crate::always::daemon::socket_is_live(&socket_path) {
            anyhow::bail!(
                "UDS socket {} is already in use — another always daemon is running",
                socket_path.display()
            );
        }
        std::fs::remove_file(&socket_path)?;
    }

    let listener = UnixListener::bind(&socket_path).context("Failed to bind Unix Domain Socket")?;

    // Restrict socket to owner only — defense against local privilege escalation.
    // Without this, a multi-user macOS could let any local user inject UDS commands.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
            .context("Failed to chmod 0600 on UDS socket")?;
    }

    tracing::info!(socket_path = %socket_path.display(), "uds_server_listening");

    // Orphan watchdog: if no UDS client is connected for ORPHAN_TIMEOUT_SECS,
    // the Mac app is gone (quit/crashed/force-killed). Exit so we don't leave
    // a stale daemon that the next app launch can't communicate with.
    //
    // Before exiting we remove the pid + socket files explicitly because
    // `std::process::exit` does NOT run Drop, so `PidGuard::Drop` would
    // be skipped and the next launch would have to wait for
    // `remove_stale_pid` / `remove_stale_socket` to detect a dead PID.
    // Only a GUI-spawned daemon mutes itself when clients vanish — a
    // daemon started manually from a terminal (`always run`) has no GUI
    // to wait for and keeps today's behavior.
    let gui_spawned = std::env::var("ALWAYS_SPAWNED_BY_GUI").ok().as_deref() == Some("1");
    tokio::spawn(async move {
        let mut last_had_clients = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let clients = CONNECTED_CLIENTS.load(Ordering::Relaxed);
            if clients > 0 {
                last_had_clients = Instant::now();
                continue;
            }
            // Mute first (fast, reversible), exit later (cleanup). The
            // mute is what guarantees speech never pastes into windows
            // while the app is closed.
            if gui_spawned
                && !pause::is_no_gui_paused()
                && last_had_clients.elapsed() > Duration::from_secs(NO_GUI_MUTE_GRACE_SECS)
            {
                let (effective, changed) = pause::set_no_gui_paused(true);
                tracing::warn!(effective, changed, "no_gui_pause_engaged");
            }
            if last_had_clients.elapsed() > Duration::from_secs(ORPHAN_TIMEOUT_SECS) {
                tracing::warn!(
                    timeout_secs = ORPHAN_TIMEOUT_SECS,
                    "orphan_daemon_exit: no UDS clients for too long"
                );
                // Mirror PidGuard::Drop's cleanup so the next daemon
                // launch sees a clean slate without needing to detect
                // a dead pid.
                if let Ok(log_path) = crate::always::config::configured_log_path()
                    && let Ok(mut log) = crate::always::log::Logger::open(&log_path)
                {
                    log.write(crate::always::log::Event::Stop);
                }
                let _ = std::fs::remove_file(crate::always::daemon::pid_path());
                if let Some(sock) = crate::always::daemon::socket_path() {
                    let _ = std::fs::remove_file(sock);
                }
                std::process::exit(0);
            }
        }
    });

    // Accept connections in a loop
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    let _ = handle_client(stream, ctx).await;
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "uds_accept_failed");
            }
        }
    }
}

/// Subscribe to the registry's `ModelEvent` channel and re-emit each
/// event on the global daemon broadcaster. Spawned once per server
/// start; lives for the daemon's lifetime.
fn spawn_registry_event_bridge(registry: ModelRegistry) {
    tokio::spawn(async move {
        let mut rx = registry.subscribe();
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let daemon_ev = match ev {
                        ModelEvent::DownloadProgress(p) => DaemonEvent::ModelDownloadProgress {
                            model_id: p.model_id,
                            downloaded: p.downloaded,
                            total: p.total,
                            percentage: p.percentage,
                        },
                        ModelEvent::DownloadComplete { model_id } => {
                            DaemonEvent::ModelDownloadComplete { model_id }
                        }
                        ModelEvent::DownloadCancelled { model_id } => {
                            DaemonEvent::ModelDownloadCancelled { model_id }
                        }
                        ModelEvent::DownloadFailed { model_id, error } => {
                            DaemonEvent::ModelDownloadFailed { model_id, error }
                        }
                        ModelEvent::VerificationStarted { model_id } => {
                            DaemonEvent::ModelVerificationStarted { model_id }
                        }
                        ModelEvent::VerificationCompleted { model_id } => {
                            DaemonEvent::ModelVerificationCompleted { model_id }
                        }
                        ModelEvent::ExtractionStarted { model_id } => {
                            DaemonEvent::ModelExtractionStarted { model_id }
                        }
                        ModelEvent::ExtractionCompleted { model_id } => {
                            DaemonEvent::ModelExtractionCompleted { model_id }
                        }
                        ModelEvent::ExtractionFailed { model_id, error } => {
                            DaemonEvent::ModelExtractionFailed { model_id, error }
                        }
                        ModelEvent::ActiveChanged { model_id } => {
                            let backend = match model_id {
                                Some(id) => format!("local:{id}"),
                                None => "groq".to_string(),
                            };
                            DaemonEvent::ActiveTranscriberChanged { backend }
                        }
                        ModelEvent::DiskStatusRefreshed => {
                            // Background verification settled the
                            // provisional flags — push the authoritative
                            // catalog so the Models tab drops its spinners.
                            DaemonEvent::ModelsList {
                                models: registry.list(),
                            }
                        }
                    };
                    global_broadcaster().send(daemon_ev);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "registry_event_bridge_lagged");
                    // Re-sync clients by pushing a fresh catalog snapshot —
                    // they may have missed a `ModelsList` mutation event.
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Handle a single client connection
#[cfg(unix)]
async fn handle_client(stream: UnixStream, ctx: ModelCommandCtx) -> Result<()> {
    CONNECTED_CLIENTS.fetch_add(1, Ordering::Relaxed);
    let _guard = ClientGuard;
    tracing::info!(
        clients = CONNECTED_CLIENTS.load(Ordering::Relaxed),
        "uds_client_connected"
    );

    // Lift the no-GUI lifecycle mute the moment a client attaches — must
    // happen BEFORE the state snapshot below so this client sees the
    // post-clear pause state.
    if pause::is_no_gui_paused() {
        let (effective, changed) = pause::set_no_gui_paused(false);
        tracing::info!(effective, "no_gui_pause_cleared");
        if changed {
            if effective {
                global_broadcaster().paused();
            } else {
                global_broadcaster().resumed();
            }
        }
    }

    let mut rx = global_broadcaster().subscribe();
    let (reader, mut writer) = stream.into_split();
    let reader = BufReader::new(reader);
    let rate_limiter = Mutex::new(RateLimiter::new());

    // Send current state on connection
    let is_paused = pause::is_paused();
    let is_master_paused = pause::is_master_paused();
    let is_auto_enter = pause::is_auto_enter_enabled();
    let resumed_bundles = crate::always::per_app::resumed_apps();
    let active_backend = ctx.cfg.read().transcriber_backend.to_string();

    // The Hello frame MUST be first so version-mismatched clients can
    // disconnect before reading state they may not understand.
    let initial_events = vec![
        DaemonEvent::Hello {
            version: crate::always::event::PROTOCOL_VERSION,
        },
        DaemonEvent::ListeningStarted, // Send immediately after Hello
        if is_paused {
            DaemonEvent::Paused
        } else {
            DaemonEvent::Resumed
        },
        DaemonEvent::MasterPauseChanged {
            master_paused: is_master_paused,
        },
        DaemonEvent::ResumedAppsChanged {
            bundles: resumed_bundles,
        },
        if is_auto_enter {
            DaemonEvent::AutoEnterEnabled
        } else {
            DaemonEvent::AutoEnterDisabled
        },
        // Tell the client which transcriber backend is active so the UI
        // shows the correct model selection after reconnect.
        DaemonEvent::ActiveTranscriberChanged {
            backend: active_backend,
        },
    ];

    let mut initial_events = initial_events;
    // Daemon-side tap health (Input Monitoring): a startup broadcast would
    // be lost (no clients yet), so each client gets the current status in
    // its initial burst.
    if let Some(granted) = crate::always::keyboard::input_monitoring_status() {
        initial_events.push(DaemonEvent::ShortcutListenerStatus {
            input_monitoring_granted: granted,
        });
    }
    // "My Voice" profile snapshot so the Settings tab renders correct
    // state on open without a request round-trip.
    {
        let (enrolled, enabled, steps) =
            crate::always::enrollment::profile_status(ctx.cfg.read().speaker_gate_enabled);
        initial_events.push(DaemonEvent::VoiceProfileStatus {
            enrolled,
            enabled,
            steps,
        });
    }

    let mut initial_payload = String::new();
    for event in initial_events {
        if let Ok(json_line) = event.to_json_line() {
            initial_payload.push_str(&json_line);
        }
    }

    if !initial_payload.is_empty() {
        // Bound the initial burst by a timeout: a client that connects but
        // never reads would otherwise wedge this task forever and keep
        // CONNECTED_CLIENTS > 0, defeating the orphan watchdog.
        match tokio::time::timeout(WRITE_TIMEOUT, async {
            writer.write_all(initial_payload.as_bytes()).await?;
            writer.flush().await
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::error!(error = %e, "uds_send_initial_state_failed");
                return Ok(());
            }
            Err(_elapsed) => {
                tracing::warn!("uds_send_initial_state_timeout: dropping unresponsive client");
                return Ok(());
            }
        }
    }

    // Read commands from the client in a separate task. Each line is a JSON
    // DaemonCommand. Executing them here (inside the daemon process) is what
    // makes the resulting events reach all UDS subscribers.
    let ctx_for_reader = ctx.clone();
    tokio::spawn(async move {
        let mut reader = reader;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break, // EOF
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    // Validate command before parsing
                    if let Err(e) = validate_command(trimmed) {
                        tracing::warn!(error = %e, command = %trimmed, "uds_command_validation_failed");
                        continue;
                    }

                    // Check rate limit
                    {
                        let mut limiter: tokio::sync::MutexGuard<RateLimiter> =
                            rate_limiter.lock().await;
                        if !limiter.check() {
                            tracing::warn!("uds_rate_limit_rejected_command");
                            continue;
                        }
                    }

                    match DaemonCommand::from_json_line(trimmed) {
                        Ok(cmd) => execute_command(cmd, &ctx_for_reader),
                        Err(e) => {
                            tracing::error!(error = %e, "uds_parse_command_failed: {command}", command = trimmed)
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "uds_read_error");
                    break;
                }
            }
        }
    });

    // Send events to client
    while let Ok(event) = rx.recv().await {
        let json_line = match event.to_json_line() {
            Ok(line) => line,
            Err(e) => {
                tracing::error!(error = %e, "uds_serialize_event_failed");
                continue;
            }
        };

        if let Err(e) = writer.write_all(json_line.as_bytes()).await {
            tracing::error!(error = %e, "uds_write_failed");
            break;
        }

        if let Err(e) = writer.flush().await {
            tracing::error!(error = %e, "uds_flush_failed");
            break;
        }
    }

    Ok(())
}

fn execute_command(cmd: DaemonCommand, ctx: &ModelCommandCtx) {
    match cmd {
        DaemonCommand::TogglePause => {
            // toggle_pause flips MASTER and returns (effective, changed).
            // We always reset idle/voice bookkeeping on a master flip,
            // even when effective didn't change — the user clearly wants
            // a "fresh start" timing-wise. Broadcasting is gated on
            // `changed` so we don't spam UDS subscribers when the
            // per-app rule kept effective the same as before the flip.
            let (effective, changed) = pause::toggle_pause();
            let master = pause::is_master_paused();
            if !master {
                pause::set_idle_auto_paused(false);
                pause::mark_voice_seen();
            }
            // Master always changed (we just toggled it) — broadcast so
            // the UI can label the global toggle correctly.
            global_broadcaster().master_pause_changed(master);
            if changed {
                if effective {
                    pause::dictation_buffer_clear();
                    global_broadcaster().paused();
                } else {
                    global_broadcaster().resumed();
                }
            }
            tracing::info!(master, effective, changed, "uds_toggle_pause");
        }
        DaemonCommand::ToggleAutoEnter => {
            let new_state = pause::toggle_auto_enter();
            if new_state {
                global_broadcaster().auto_enter_enabled();
            } else {
                global_broadcaster().auto_enter_disabled();
            }
            tracing::info!(new_state, "uds_toggle_auto_enter");
        }
        DaemonCommand::SetAutoEnter { enabled } => {
            pause::set_auto_enter_enabled(enabled);
            if enabled {
                global_broadcaster().auto_enter_enabled();
            } else {
                global_broadcaster().auto_enter_disabled();
            }
            tracing::info!(enabled, "uds_set_auto_enter");
        }
        DaemonCommand::SetConsumeMode { enabled } => {
            // Route to stream consumers (Iris) instead of pasting. No pause
            // recompute needed — the capture loop and paste path read
            // `is_consume_mode()` directly.
            pause::set_consume_mode(enabled);
            tracing::info!(enabled, "uds_set_consume_mode");
        }
        DaemonCommand::ApplyRuntimePreferences {
            auto_enter_delay_ms,
            energy_threshold,
            silence_secs,
            cooldown_ms,
            silero_threshold,
            adaptive_silence,
            audible_status_sound,
        } => {
            apply_runtime_preferences(
                ctx,
                auto_enter_delay_ms,
                energy_threshold,
                silence_secs,
                cooldown_ms,
                silero_threshold,
                adaptive_silence,
                audible_status_sound,
            );
        }
        DaemonCommand::ApproveCorrection { id } => handle_approve_correction(&id),
        DaemonCommand::RejectCorrection { id } => handle_reject_correction(&id),
        DaemonCommand::CaptureCorrection => handle_capture_correction(),
        DaemonCommand::SetPaused { paused, reason } => {
            let (effective, changed) = pause::set_paused(paused);
            if !paused {
                pause::set_idle_auto_paused(false);
                pause::mark_voice_seen();
            }
            global_broadcaster().master_pause_changed(paused);
            if changed {
                if effective {
                    pause::dictation_buffer_clear();
                    global_broadcaster().paused();
                } else {
                    global_broadcaster().resumed();
                }
            }
            tracing::info!(
                master = paused,
                effective,
                changed,
                reason = reason.as_deref().unwrap_or(""),
                "uds_set_paused"
            );
        }
        DaemonCommand::CancelAutoEnterCountdown => {
            if pause::countdown_active() {
                pause::countdown_request_cancel();
                pause::dictation_buffer_clear();
                tracing::info!("uds_cancel_auto_enter_countdown");
            }
        }
        DaemonCommand::NotifyFocusedAppChanged { bundle_id } => {
            // Returning to an allowlisted app after idle auto-pause should
            // resume listening without requiring a global master unpause.
            if pause::is_idle_auto_paused()
                && bundle_id
                    .as_deref()
                    .is_some_and(crate::always::per_app::is_app_resumed)
            {
                pause::set_idle_auto_paused(false);
                pause::mark_voice_seen();
                global_broadcaster().idle_auto_resumed();
            }
            // Update focused app + recompute effective in one step.
            // Effective may flip because per-app rules differ between
            // the previous and new bundle; broadcast on change so the
            // status bar icon and overlay follow the user's focus.
            let (effective, changed) = pause::set_current_app_and_recompute(bundle_id.clone());
            // Persist the now-current bundle so a daemon restart can
            // restore per-app pause state via
            // `focus_state::restore_on_startup` without waiting for the
            // next focus-change event from the Mac app.
            crate::always::focus_state::save(bundle_id.as_deref());
            global_broadcaster().focused_app_changed(bundle_id.clone());
            if changed {
                if effective {
                    pause::dictation_buffer_clear();
                    // Quiet variant: focus changes come from manual
                    // window switches (mouse, Mission Control). The
                    // user already knows they switched app — flashing
                    // pause/play overlays is visual noise.
                    global_broadcaster().paused_quietly();
                } else {
                    pause::mark_voice_seen();
                    global_broadcaster().resumed_quietly();
                }
                tracing::info!(effective, "per_app_paused_applied");
            }
            tracing::info!(bundle = ?bundle_id, effective, "uds_focused_app_changed");
        }
        DaemonCommand::NotifySystemAudioState { playing } => {
            // "My Voice" gate active → the daemon already ignores every
            // voice but the enrolled user's, so media playback is no
            // reason to stop listening. Skip the auto-pause entirely
            // (and make sure no stale one lingers) — this is what lets
            // dictation keep working while movies/music play.
            let speaker_gate_active = {
                let cfg = ctx.cfg.read();
                crate::always::vad::speaker_gate_ready(&cfg)
            };
            if speaker_gate_active {
                let (effective, changed) = pause::set_audio_output_paused(false);
                if changed {
                    global_broadcaster().pause_source_changed("audio_output", false, None);
                    if !effective {
                        global_broadcaster().resumed();
                    }
                }
                tracing::debug!(playing, "audio_output_ignored_speaker_gate");
                return;
            }
            // Audio output started/stopped → flip the audio-output pause
            // SOURCE only. This used to stomp MASTER pause, which meant
            // a pause the user set manually was silently cleared the
            // moment their music stopped — MASTER is user intent now.
            if playing {
                let (effective, changed) = pause::set_audio_output_paused(true);
                global_broadcaster().pause_source_changed("audio_output", true, None);
                if changed {
                    pause::dictation_buffer_clear();
                    global_broadcaster().paused();
                    tracing::info!(effective, "audio_output_auto_paused");
                }
            } else if !pause::is_idle_auto_paused() {
                let (effective, changed) = pause::set_audio_output_paused(false);
                pause::mark_voice_seen();
                global_broadcaster().pause_source_changed("audio_output", false, None);
                if changed {
                    if effective {
                        global_broadcaster().paused();
                    } else {
                        global_broadcaster().resumed();
                    }
                    tracing::info!(effective, "audio_output_auto_resumed");
                }
            } else {
                // Idle-paused: drop the audio source flag quietly so it
                // can't strand a pause after idle resume.
                let _ = pause::set_audio_output_paused(false);
            }
        }
        DaemonCommand::SetAppPaused { bundle_id, paused } => {
            // Write the override for `bundle_id` and recompute the
            // effective state. Broadcasting is gated on `changed` so
            // setting an override for a non-focused app doesn't spam
            // the UDS bus.
            match crate::always::per_app::set_app_paused_override(&bundle_id, paused) {
                Ok(()) => {
                    let (effective, changed) = pause::recompute_effective();
                    if changed {
                        if effective {
                            pause::dictation_buffer_clear();
                            global_broadcaster().paused();
                        } else {
                            pause::mark_voice_seen();
                            global_broadcaster().resumed();
                        }
                    }
                    // Always broadcast the updated allowlist so every
                    // connected client (Settings UI, menu bar) sees
                    // the same snapshot without round-tripping
                    // through the CLI `config show` path.
                    global_broadcaster()
                        .resumed_apps_changed(crate::always::per_app::resumed_apps());
                    tracing::info!(
                        bundle = %bundle_id,
                        paused = ?paused,
                        effective,
                        changed,
                        "uds_set_app_paused"
                    );
                }
                Err(e) => {
                    tracing::error!(error = %e, bundle = %bundle_id, "uds_set_app_paused_failed");
                }
            }
        }
        DaemonCommand::LogCorrection { intended } => {
            handle_log_correction(&intended);
        }
        DaemonCommand::ListModels => {
            let models = ctx.registry.list();
            global_broadcaster().send(DaemonEvent::ModelsList { models });
            // Always re-announce the active backend alongside the catalog.
            // The Models panel's client is a lazy singleton that may not have
            // existed when the connection-time ActiveTranscriberChanged was
            // broadcast — so without this, the panel never learns which model
            // is active after relaunch and shows the wrong selection.
            let active_backend = ctx.cfg.read().transcriber_backend.to_string();
            global_broadcaster().send(DaemonEvent::ActiveTranscriberChanged {
                backend: active_backend,
            });
        }
        DaemonCommand::DownloadModel { model_id } => {
            let registry = ctx.registry.clone();
            let id_for_log = model_id.clone();
            tokio::spawn(async move {
                if let Err(e) = registry.download(&model_id).await {
                    tracing::error!(model = %model_id, error = %e, "model_download_failed");
                }
                // Push a fresh catalog snapshot so clients see the
                // updated `is_downloaded` flag without polling.
                let models = registry.list();
                global_broadcaster().send(DaemonEvent::ModelsList { models });
            });
            tracing::info!(model = %id_for_log, "uds_download_model");
        }
        DaemonCommand::CancelModelDownload { model_id } => {
            ctx.registry.cancel_download(&model_id);
            tracing::info!(model = %model_id, "uds_cancel_model_download");
        }
        DaemonCommand::DeleteModel { model_id } => {
            match ctx.registry.delete(&model_id) {
                Ok(()) => {
                    // If the deleted model was active, fall back to Groq.
                    let was_active = matches!(
                        &ctx.cfg.read().transcriber_backend,
                        TranscriberBackendChoice::Local { model_id: active_id }
                            if active_id == &model_id
                    );
                    if was_active {
                        switch_active_backend(ctx, TranscriberBackendChoice::Groq);
                    }
                    let models = ctx.registry.list();
                    global_broadcaster().send(DaemonEvent::ModelsList { models });
                    tracing::info!(model = %model_id, was_active, "uds_delete_model");
                }
                Err(e) => tracing::error!(model = %model_id, error = %e, "uds_delete_model_failed"),
            }
        }
        DaemonCommand::SetActiveTranscriber { backend } => match backend.parse() {
            Ok(choice) => {
                switch_active_backend(ctx, choice);
            }
            Err(e) => tracing::error!(error = %e, backend, "uds_set_active_invalid"),
        },
        DaemonCommand::SetLanguage { lang } => {
            set_language(ctx, &lang);
        }
        DaemonCommand::StartVoiceEnrollment { step } => {
            match crate::always::voiceprint::EnrollStep::parse(&step) {
                Some(parsed) => {
                    if crate::always::enrollment::request_step(parsed) {
                        tracing::info!(step = %step, "uds_voice_enrollment_queued");
                    } else {
                        global_broadcaster().voice_enrollment_failed(
                            &step,
                            "another enrollment recording is already in progress",
                        );
                    }
                }
                None => {
                    global_broadcaster().voice_enrollment_failed(
                        &step,
                        "unknown enrollment step (expected normal, lower, or louder)",
                    );
                }
            }
        }
        DaemonCommand::CancelVoiceEnrollment => {
            crate::always::enrollment::request_cancel();
            tracing::info!("uds_voice_enrollment_cancel_requested");
        }
        DaemonCommand::DeleteVoiceProfile => match crate::always::voiceprint::clear() {
            Ok(()) => {
                let (enrolled, enabled, steps) =
                    crate::always::enrollment::profile_status(ctx.cfg.read().speaker_gate_enabled);
                global_broadcaster().voice_profile_status(enrolled, enabled, steps);
                tracing::info!("uds_voice_profile_deleted");
            }
            Err(e) => tracing::error!(error = %e, "uds_voice_profile_delete_failed"),
        },
        DaemonCommand::SetVoiceProfileEnabled { enabled } => {
            ctx.cfg.write().speaker_gate_enabled = enabled;
            // Persist for the next daemon start.
            if let Ok(conn) = crate::db::open()
                && let Err(e) = crate::db::set_preference(
                    &conn,
                    "speaker_gate_enabled",
                    if enabled { "true" } else { "false" },
                )
            {
                tracing::warn!(error = %e, enabled, "persist_speaker_gate_failed");
            }
            // The gate supersedes the media auto-pause: clear any stale
            // audio-output pause on enable so listening resumes right
            // away even if music is currently playing.
            let speaker_gate_active = {
                let cfg = ctx.cfg.read();
                crate::always::vad::speaker_gate_ready(&cfg)
            };
            if speaker_gate_active {
                let (effective, changed) = pause::set_audio_output_paused(false);
                if changed {
                    global_broadcaster().pause_source_changed("audio_output", false, None);
                    if !effective {
                        global_broadcaster().resumed();
                    }
                }
            }
            let (enrolled, _, steps) = crate::always::enrollment::profile_status(enabled);
            global_broadcaster().voice_profile_status(enrolled, enabled, steps);
            tracing::info!(enabled, "uds_set_voice_profile_enabled");
        }
        DaemonCommand::GetVoiceProfileStatus => {
            let (enrolled, enabled, steps) =
                crate::always::enrollment::profile_status(ctx.cfg.read().speaker_gate_enabled);
            global_broadcaster().voice_profile_status(enrolled, enabled, steps);
        }
    }
}

/// Update the transcription language live: write `cfg.lang`, persist to the
/// prefs DB, and rebuild the active transcriber so engines that bake the
/// language into their decode (Canary, Cohere, SenseVoice) pick it up
/// without a daemon restart.
fn set_language(ctx: &ModelCommandCtx, lang: &str) {
    {
        let mut cfg = ctx.cfg.write();
        cfg.lang = lang.to_string();
    }
    // Persist for the next daemon start.
    if let Ok(conn) = crate::db::open()
        && let Err(e) = crate::db::set_preference(&conn, "lang", lang)
    {
        tracing::warn!(error = %e, lang, "persist_lang_failed");
    }
    // Rebuild the active transcriber with the new language hint.
    let cfg_snapshot = ctx.cfg.read().clone();
    match build_transcriber(&cfg_snapshot, &ctx.registry) {
        Ok(transcriber) => {
            *ctx.active.write() = transcriber;
            tracing::info!(lang, "uds_set_language_rebuilt");
        }
        Err(e) => tracing::error!(error = %e, lang, "uds_set_language_rebuild_failed"),
    }
}

/// Rebuild the [`crate::stt::Transcriber`] for `choice`, swap it into
/// the [`ActiveTranscriber`] lock, persist the selection to the prefs
/// DB, and emit [`DaemonEvent::ActiveTranscriberChanged`] so connected
/// UDS clients refresh their active-model badge.
fn switch_active_backend(ctx: &ModelCommandCtx, choice: TranscriberBackendChoice) {
    let mut new_cfg = ctx.cfg.write();
    new_cfg.transcriber_backend = choice.clone();
    match build_transcriber(&new_cfg, &ctx.registry) {
        Ok(transcriber) => {
            *ctx.active.write() = transcriber;
            // Persist so the next daemon start uses the same choice.
            if let Ok(conn) = crate::db::open() {
                let value = choice.to_string();
                if let Err(e) = crate::db::set_preference(&conn, "transcriber_backend", &value) {
                    tracing::warn!(error = %e, "persist_transcriber_backend_failed");
                }
            }
            global_broadcaster().send(DaemonEvent::ActiveTranscriberChanged {
                backend: choice.to_string(),
            });
            tracing::info!(backend = %choice, "uds_set_active_transcriber");
        }
        Err(e) => {
            tracing::error!(backend = %choice, error = %e, "build_new_transcriber_failed");
        }
    }
}

/// Hot-reload fields the main loop reads from [`AlwaysConfig`] each
/// utterance. DB persistence is handled by the Mac app before this
/// command is sent.
#[allow(clippy::too_many_arguments)]
fn apply_runtime_preferences(
    ctx: &ModelCommandCtx,
    auto_enter_delay_ms: u32,
    energy_threshold: f64,
    silence_secs: f64,
    cooldown_ms: u32,
    silero_threshold: f32,
    adaptive_silence: Option<bool>,
    audible_status_sound: Option<String>,
) {
    let mut cfg = ctx.cfg.write();
    cfg.auto_enter_delay_ms = auto_enter_delay_ms.min(60_000);
    cfg.energy_threshold = energy_threshold.clamp(0.0001, 0.5);
    cfg.silence_secs = silence_secs.clamp(
        crate::always::config::SILENCE_SECS_MIN,
        crate::always::config::SILENCE_SECS_MAX,
    );
    cfg.cooldown_ms = cooldown_ms;
    cfg.silero_threshold = silero_threshold.clamp(0.0, 1.0);
    if let Some(adaptive) = adaptive_silence {
        cfg.adaptive_silence_enabled = adaptive;
    }
    if let Some(setting) = audible_status_sound
        .as_deref()
        .and_then(|value| value.parse().ok())
    {
        cfg.audible_status_sound = setting;
        crate::always::status_sound::set_setting(setting);
    }
    tracing::info!(
        auto_enter_delay_ms = cfg.auto_enter_delay_ms,
        energy_threshold = cfg.energy_threshold,
        silence_secs = cfg.silence_secs,
        cooldown_ms = cfg.cooldown_ms,
        silero_threshold = cfg.silero_threshold,
        adaptive_silence = cfg.adaptive_silence_enabled,
        audible_status_sound = cfg.audible_status_sound.as_str(),
        "uds_apply_runtime_preferences"
    );
}

/// Handler for `ApproveCorrection`. Looks the entry up by UUID, applies
/// it to the glossary, and broadcasts `CorrectionLogged` so connected
/// clients can flash a confirmation toast and refresh their pending
/// counter.
fn handle_approve_correction(id_str: &str) {
    let queue = match crate::always::correction_queue::global_queue() {
        Ok(q) => q,
        Err(e) => {
            tracing::error!(error = %e, "uds_approve_correction_queue_unavailable");
            return;
        }
    };
    let id = match uuid::Uuid::parse_str(id_str) {
        Ok(u) => u,
        Err(_) => {
            tracing::warn!(id = %id_str, "uds_approve_correction_invalid_id");
            return;
        }
    };
    let Some(entry) = queue.take(id) else {
        tracing::warn!(id = %id_str, "uds_approve_correction_unknown_id");
        return;
    };
    match crate::always::correction::apply_pairs_to_glossary(std::slice::from_ref(&entry.pair)) {
        Ok(_) => {
            global_broadcaster().correction_logged(&entry.pair.wrong, &entry.pair.right);
            tracing::info!(
                wrong = %entry.pair.wrong,
                right = %entry.pair.right,
                "uds_approve_correction_applied"
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "uds_approve_correction_apply_failed");
        }
    }
}

fn handle_reject_correction(id_str: &str) {
    let queue = match crate::always::correction_queue::global_queue() {
        Ok(q) => q,
        Err(e) => {
            tracing::error!(error = %e, "uds_reject_correction_queue_unavailable");
            return;
        }
    };
    let id = match uuid::Uuid::parse_str(id_str) {
        Ok(u) => u,
        Err(_) => {
            tracing::warn!(id = %id_str, "uds_reject_correction_invalid_id");
            return;
        }
    };
    if queue.take(id).is_some() {
        tracing::info!(id = %id_str, "uds_reject_correction_dropped");
    }
}

fn handle_capture_correction() {
    use crate::always::correction;
    let outcome =
        match correction::capture_via_hotkey(crate::always::clipboard_watcher::PASTE_WINDOW) {
            Ok(o) => o,
            Err(e) => {
                tracing::error!(error = %e, "uds_capture_correction_failed");
                return;
            }
        };
    if let correction::CaptureOutcome::Applied { pairs, applied: _ } = outcome {
        for p in pairs {
            global_broadcaster().correction_logged(&p.wrong, &p.right);
        }
    }
}

/// Handle the dialog-driven correction: the user typed the intended
/// spelling (`intended`); we diff it against the most recently-pasted
/// transcript to extract the wrong word and apply a correction pair.
///
/// Matching: tokenize the last transcript, pick the token with the
/// lowest case-insensitive Levenshtein distance to `intended`. If the
/// best distance is too large (>= half the intended length), the user
/// probably entered a *new* term — we add a glossary entry with that
/// term and no mistranscriptions but bumped weight.
fn handle_log_correction(intended: &str) {
    let intended = intended.trim();
    if intended.is_empty() {
        tracing::warn!("uds_log_correction_empty_input");
        global_broadcaster().correction_capture_result("no_correction_pairs");
        return;
    }

    let Some(last) = pause::last_transcript_for_correction() else {
        tracing::info!("uds_log_correction_no_recent_paste");
        global_broadcaster().correction_capture_result("no_recent_paste");
        return;
    };

    let best = best_token_match(&last, intended);
    match best {
        Some((wrong, distance)) if distance <= intended.chars().count() / 2 => {
            let pair = crate::always::correction::CorrectionPair {
                wrong: wrong.clone(),
                right: intended.to_string(),
            };
            match crate::always::correction::apply_pairs_to_glossary(std::slice::from_ref(&pair)) {
                Ok(_) => {
                    global_broadcaster().correction_logged(&pair.wrong, &pair.right);
                    global_broadcaster().correction_capture_result("applied");
                    tracing::info!(wrong = %pair.wrong, right = %pair.right, "uds_log_correction_applied");
                }
                Err(e) => {
                    tracing::error!(error = %e, "uds_log_correction_apply_failed");
                    global_broadcaster().correction_capture_result("error");
                }
            }
        }
        _ => {
            // No close match — treat as a new vocabulary term with bumped weight.
            match crate::always::correction::add_or_bump_term(intended) {
                Ok(_) => {
                    global_broadcaster().correction_logged("", intended);
                    global_broadcaster().correction_capture_result("applied");
                    tracing::info!(term = %intended, "uds_log_correction_added_term");
                }
                Err(e) => {
                    tracing::error!(error = %e, "uds_log_correction_term_apply_failed");
                    global_broadcaster().correction_capture_result("error");
                }
            }
        }
    }
}

/// Return the token in `haystack` with the smallest case-insensitive
/// Levenshtein distance to `needle`, paired with that distance.
///
/// Uses `strsim::levenshtein` (same dep `text_match.rs` already pulls in)
/// — earlier this module had its own hand-rolled implementation, which
/// was a maintenance hazard and drifted from the canonical one used in
/// acoustic matching.
fn best_token_match(haystack: &str, needle: &str) -> Option<(String, usize)> {
    let needle_lc = needle.to_lowercase();
    let mut best: Option<(String, usize)> = None;
    for tok in haystack.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_') {
        if tok.is_empty() {
            continue;
        }
        let d = strsim::levenshtein(&tok.to_lowercase(), &needle_lc);
        match &best {
            Some((_, prev)) if *prev <= d => {}
            _ => best = Some((tok.to_string(), d)),
        }
    }
    best
}

/// Send an event to all connected clients
pub fn send_event(event: DaemonEvent) {
    global_broadcaster().send(event);
}
