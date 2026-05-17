use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use super::event::{DaemonCommand, DaemonEvent, global_broadcaster};
use super::pause;

// DoS protection constants
const MAX_COMMAND_LINE_LENGTH: usize = 1024; // 1KB max command size
const COMMANDS_PER_SECOND_LIMIT: u32 = 10; // Max 10 commands per second per client

/// How long the daemon stays alive after the last UDS client disconnects.
/// Prevents stale daemons surviving indefinitely when the Mac app quits.
const ORPHAN_TIMEOUT_SECS: u64 = 30;

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
        tracing::info!(clients = prev - 1, "uds_client_disconnected");
    }
}

/// Unix Domain Socket path for daemon-to-GUI communication
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

/// Start the Unix Domain Socket server
pub async fn start_server() -> Result<()> {
    let socket_path = socket_path()?;

    // Remove existing socket file if it exists
    if socket_path.exists() {
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
    tokio::spawn(async {
        let mut last_had_clients = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let clients = CONNECTED_CLIENTS.load(Ordering::Relaxed);
            if clients > 0 {
                last_had_clients = Instant::now();
            } else if last_had_clients.elapsed() > Duration::from_secs(ORPHAN_TIMEOUT_SECS) {
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
                tokio::spawn(async move {
                    let _ = handle_client(stream).await;
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "uds_accept_failed");
            }
        }
    }
}

/// Handle a single client connection
async fn handle_client(stream: UnixStream) -> Result<()> {
    CONNECTED_CLIENTS.fetch_add(1, Ordering::Relaxed);
    let _guard = ClientGuard;
    tracing::info!(
        clients = CONNECTED_CLIENTS.load(Ordering::Relaxed),
        "uds_client_connected"
    );

    let mut rx = global_broadcaster().subscribe();
    let (reader, mut writer) = stream.into_split();
    let reader = BufReader::new(reader);
    let rate_limiter = Mutex::new(RateLimiter::new());

    // Send current state on connection
    let is_paused = pause::is_paused();
    let is_auto_enter = pause::is_auto_enter_enabled();

    // The Hello frame MUST be first so version-mismatched clients can
    // disconnect before reading state they may not understand.
    let initial_events = vec![
        DaemonEvent::Hello {
            version: crate::always::event::PROTOCOL_VERSION,
        },
        if is_paused {
            DaemonEvent::Paused
        } else {
            DaemonEvent::Resumed
        },
        if is_auto_enter {
            DaemonEvent::AutoEnterEnabled
        } else {
            DaemonEvent::AutoEnterDisabled
        },
        DaemonEvent::ListeningStarted,
    ];

    for event in initial_events {
        if let Ok(json_line) = event.to_json_line()
            && let Err(e) = writer.write_all(json_line.as_bytes()).await
        {
            tracing::error!(error = %e, "uds_send_initial_state_failed");
            return Ok(());
        }
    }

    if let Err(e) = writer.flush().await {
        tracing::error!(error = %e, "uds_flush_initial_state_failed");
        return Ok(());
    }

    // Read commands from the client in a separate task. Each line is a JSON
    // DaemonCommand. Executing them here (inside the daemon process) is what
    // makes the resulting events reach all UDS subscribers.
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
                        Ok(cmd) => execute_command(cmd),
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

fn execute_command(cmd: DaemonCommand) {
    match cmd {
        DaemonCommand::TogglePause => {
            let new_state = pause::toggle_pause();
            if new_state {
                global_broadcaster().paused();
            } else {
                // Clear the idle-auto-paused flag and reset the voice
                // timestamp so the watchdog doesn't immediately re-pause us
                // — same invariants the explicit `SetPaused{false}` path
                // enforces. Without this, an idle-auto-paused daemon that
                // the user unpaused via TogglePause would re-pause within
                // CHECK_INTERVAL seconds.
                pause::set_idle_auto_paused(false);
                pause::mark_voice_seen();
                global_broadcaster().resumed();
            }
            tracing::info!(new_state, "uds_toggle_pause");
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
        DaemonCommand::ApproveCorrection { id } => handle_approve_correction(&id),
        DaemonCommand::RejectCorrection { id } => handle_reject_correction(&id),
        DaemonCommand::CaptureCorrection => handle_capture_correction(),
        DaemonCommand::SetPaused { paused, reason } => {
            let was_paused = pause::is_paused();
            if was_paused != paused {
                pause::set_paused(paused);
                if !paused {
                    pause::set_idle_auto_paused(false);
                    pause::mark_voice_seen();
                }
                if paused {
                    // Pause invalidates the dictation merge buffer — when
                    // the user resumes, they're starting a fresh utterance,
                    // not continuing a sentence from before the pause.
                    pause::dictation_buffer_clear();
                    global_broadcaster().paused();
                } else {
                    global_broadcaster().resumed();
                }
            }
            tracing::info!(paused, reason = reason.as_deref().unwrap_or(""), "uds_set_paused");
        }
        DaemonCommand::CancelAutoEnterCountdown => {
            if pause::countdown_active() {
                pause::countdown_request_cancel();
                pause::dictation_buffer_clear();
                tracing::info!("uds_cancel_auto_enter_countdown");
            }
        }
        DaemonCommand::NotifyFocusedAppChanged { bundle_id } => {
            pause::set_current_app(bundle_id.clone());
            global_broadcaster().focused_app_changed(bundle_id.clone());
            // Apply per-app paused override on focus change. We compute
            // the effective paused state from the override table (if
            // any) and the *current* global flag, then sync the global
            // flag to it. Without this push, an app that wants
            // "always paused" would still record while focused unless
            // the user manually toggles.
            let global = pause::is_paused();
            let effective = crate::always::per_app::effective_paused(global);
            if effective != global {
                pause::set_paused(effective);
                if effective {
                    global_broadcaster().paused();
                } else {
                    pause::mark_voice_seen();
                    global_broadcaster().resumed();
                }
                tracing::info!(effective, "per_app_paused_applied");
            }
            tracing::info!(bundle = ?bundle_id, "uds_focused_app_changed");
        }
        DaemonCommand::NotifySystemAudioState { playing } => {
            // Audio output started → pause. Audio output stopped → if
            // we were paused for audio, resume. Distinguish from
            // idle-auto-pause via the dedicated flag.
            if playing {
                if !pause::is_paused() {
                    pause::set_paused(true);
                    global_broadcaster().paused();
                    tracing::info!("audio_output_auto_paused");
                }
            } else {
                // Only auto-resume if we weren't already in idle-pause.
                if pause::is_paused() && !pause::is_idle_auto_paused() {
                    pause::set_paused(false);
                    pause::mark_voice_seen();
                    global_broadcaster().resumed();
                    tracing::info!("audio_output_auto_resumed");
                }
            }
        }
        DaemonCommand::LogCorrection { intended } => {
            handle_log_correction(&intended);
        }
    }
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
