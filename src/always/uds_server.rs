use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::UnixListener;
#[cfg(windows)]
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
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

/// RAII guard for a UDS connection. It releases a consume-mode lease owned by
/// this connection before decrementing the connection count.
struct ClientGuard {
    consume_lease: Arc<AtomicBool>,
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        pause::release_consume_mode(&self.consume_lease);
        let prev = CONNECTED_CLIENTS.fetch_sub(1, Ordering::Relaxed);
        let remaining = prev - 1;
        tracing::info!(clients = remaining, "uds_client_disconnected");
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
            .join(format!("always{}.sock", crate::config::instance_suffix()))
    } else {
        // Linux: Use XDG_RUNTIME_DIR or fallback to /tmp
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(runtime_dir)
                .join(format!("always{}.sock", crate::config::instance_suffix()))
        } else {
            PathBuf::from(format!(
                "/tmp/always{}.sock",
                crate::config::instance_suffix()
            ))
        }
    };

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create socket directory")?;
    }

    Ok(path)
}

/// Windows equivalent of the UDS socket: a named pipe per instance.
/// The `\\.\pipe\` prefix selects the local-machine pipe namespace —
/// the pipe is reachable only from this machine, same trust boundary
/// as the per-user socket file.
#[cfg(windows)]
pub fn socket_path() -> Result<PathBuf> {
    Ok(PathBuf::from(format!(
        r"\\.\pipe\always{}",
        crate::config::instance_suffix()
    )))
}

#[cfg(not(any(unix, windows)))]
pub fn socket_path() -> Result<PathBuf> {
    anyhow::bail!("daemon IPC not supported on this platform")
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

    spawn_orphan_watchdog();

    // Accept connections in a loop
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                #[cfg(unix)]
                tune_send_buf(&stream);
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

/// Orphan watchdog: if no IPC client is connected for ORPHAN_TIMEOUT_SECS,
/// the GUI is gone (quit/crashed/force-killed). Exit so we don't leave
/// a stale daemon that the next app launch can't communicate with.
///
/// Before exiting we remove the pid + socket files explicitly because
/// `std::process::exit` does NOT run Drop, so `PidGuard::Drop` would
/// be skipped and the next launch would have to wait for
/// `remove_stale_pid` / `remove_stale_socket` to detect a dead PID.
/// Only a GUI-spawned daemon mutes itself when clients vanish — a
/// daemon started manually from a terminal (`always run`) has no GUI
/// to wait for and keeps today's behavior.
///
/// The watchdog only runs for GUI-spawned daemons — they are the only
/// ones a vanished client can orphan. A daemon started from a terminal
/// (`always run` / `always start`) has no client to wait for and must
/// keep running; on Windows, where IPC clients are transient CLI
/// invocations, an unconditional exit would kill every `always start`
/// daemon 30 seconds in.
fn spawn_orphan_watchdog() {
    if std::env::var("ALWAYS_SPAWNED_BY_GUI").ok().as_deref() != Some("1") {
        return;
    }
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
            if !pause::is_no_gui_paused()
                && last_had_clients.elapsed() > Duration::from_secs(NO_GUI_MUTE_GRACE_SECS)
            {
                let (effective, changed) = pause::set_no_gui_paused(true);
                tracing::warn!(effective, changed, "no_gui_pause_engaged");
            }
            if last_had_clients.elapsed() > Duration::from_secs(ORPHAN_TIMEOUT_SECS) {
                tracing::warn!(
                    timeout_secs = ORPHAN_TIMEOUT_SECS,
                    "orphan_daemon_exit: no IPC clients for too long"
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
                // On Windows the pipe vanishes with the process — there is
                // no stale endpoint file to remove.
                #[cfg(unix)]
                if let Some(sock) = crate::always::daemon::socket_path() {
                    let _ = std::fs::remove_file(sock);
                }
                std::process::exit(0);
            }
        }
    });
}

/// Named-pipe server — the Windows equivalent of the UDS listener. Same
/// JSON-lines protocol, same per-connection handler. `first_pipe_instance`
/// makes bind a single-winner operation, mirroring the socket-file check:
/// a second daemon cannot steal the endpoint out from under a live one.
#[cfg(windows)]
const PRIVATE_PIPE_SDDL: &str = "D:P(A;;GA;;;OW)(A;;GA;;;SY)";

#[cfg(windows)]
fn create_private_pipe_server(
    path: &std::path::Path,
    first_instance: bool,
) -> Result<NamedPipeServer> {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null_mut;

    #[repr(C)]
    struct SecurityAttributes {
        length: u32,
        descriptor: *mut c_void,
        inherit_handle: i32,
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_string: *const u16,
            revision: u32,
            descriptor: *mut *mut c_void,
            descriptor_size: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    // Protected DACL: only this process token's owner and SYSTEM may access
    // the pipe. In particular, do not inherit the default Everyone/Anonymous
    // read ACEs: the pipe carries transcript events.
    let sddl_wide: Vec<u16> = PRIVATE_PIPE_SDDL
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor = null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_wide.as_ptr(),
            1,
            &mut descriptor,
            null_mut(),
        )
    };
    if converted == 0 {
        return Err(std::io::Error::last_os_error()).context("build private named-pipe ACL");
    }

    let mut attributes = SecurityAttributes {
        length: size_of::<SecurityAttributes>() as u32,
        descriptor,
        inherit_handle: 0,
    };
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first_instance)
        .reject_remote_clients(true);
    let result = unsafe {
        options.create_with_security_attributes_raw(
            path.as_os_str(),
            (&mut attributes as *mut SecurityAttributes).cast(),
        )
    };
    unsafe {
        LocalFree(descriptor);
    }
    result.context("create per-user named pipe")
}

#[cfg(windows)]
pub async fn start_server(
    registry: ModelRegistry,
    active: ActiveTranscriber,
    cfg: ActiveConfig,
) -> Result<()> {
    spawn_registry_event_bridge(registry.clone());
    let ctx = ModelCommandCtx {
        registry,
        active,
        cfg,
    };

    let pipe_path = socket_path()?;
    let mut server = create_private_pipe_server(&pipe_path, true)
        .context("Failed to create named pipe — another always daemon may hold it")?;

    spawn_orphan_watchdog();
    tracing::info!(pipe = %pipe_path.display(), "pipe_server_listening");

    loop {
        match server.connect().await {
            Ok(()) => {
                // The connected instance is handed to a task; a fresh
                // instance takes its place as the listener.
                match create_private_pipe_server(&pipe_path, false) {
                    Ok(next) => {
                        let conn = std::mem::replace(&mut server, next);
                        let ctx = ctx.clone();
                        tokio::spawn(async move {
                            let _ = handle_client(conn, ctx).await;
                        });
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "pipe_recreate_failed");
                        anyhow::bail!("named pipe listener lost: {e}");
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "pipe_accept_failed");
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

/// Bump the UDS send buffer for the large initial-state payload.
/// `tokio::net::UnixStream` exposes no setter (unlike TcpStream), so go
/// through the raw fd. A failure is non-fatal: the payload is also
/// chunked, the bigger buffer only avoids the extra round-trips.
#[cfg(unix)]
fn tune_send_buf<S>(stream: &S)
where
    S: std::os::fd::AsRawFd,
{
    // macOS caps SO_SNDBUF on Unix domain streams well below 1 MB —
    // asking for it fails with EINVAL on every boot (observed live).
    // Step down to a size the kernel accepts and log at debug: this
    // is a throughput nicety, not a correctness requirement.
    for size in [1024 * 1024, 256 * 1024, 64 * 1024] {
        let size: libc::c_int = size;
        let rc = unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                std::ptr::from_ref(&size).cast::<libc::c_void>(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if rc == 0 {
            break;
        }
        if size == 64 * 1024 {
            tracing::debug!(
                error = %std::io::Error::last_os_error(),
                "uds_send_buffer_increase_failed (kernel default in use)"
            );
        }
    }
}

#[cfg(not(unix))]
fn tune_send_buf<S>(_stream: &S) {
    // No-op on non-Unix platforms (Windows uses named pipes)
}

/// Handle a single client connection
async fn handle_client<S>(stream: S, ctx: ModelCommandCtx) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    CONNECTED_CLIENTS.fetch_add(1, Ordering::Relaxed);
    let consume_lease = Arc::new(AtomicBool::new(false));
    let _guard = ClientGuard {
        consume_lease: Arc::clone(&consume_lease),
    };

    // Skip tune_send_buf in generic handler - called in platform-specific code

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
    let (reader, mut writer) = tokio::io::split(stream);
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
    let mut initial_events = vec![
        DaemonEvent::Hello {
            version: crate::always::event::PROTOCOL_VERSION,
            build_id: Some(crate::always::event::BUILD_ID.to_string()),
        },
        DaemonEvent::ListeningStarted, // Send immediately after Hello
        DaemonEvent::MasterPauseChanged {
            master_paused: is_master_paused,
        },
        DaemonEvent::ResumedAppsChanged {
            bundles: resumed_bundles,
        },
    ];
    // Active watchdog sources, so a (re)connecting GUI predicts with them
    // instead of guessing "listening" over a live mic conflict. Only ACTIVE
    // ones: `paused:false` for an inactive source would make the GUI drop a
    // live one (`pauseSource = paused ? source : null`).
    for (source, active) in [
        ("mic_conflict", pause::is_mic_conflict_paused()),
        ("audio_output", pause::is_audio_output_paused()),
    ] {
        if active {
            initial_events.push(DaemonEvent::PauseSourceChanged {
                source: source.into(),
                paused: true,
                detail: None,
            });
        }
    }
    initial_events.extend([
        // Effective state AFTER the above: the GUI recomputes a local
        // prediction on MasterPauseChanged / ResumedAppsChanged, so sending
        // the truth first let that prediction overwrite it (icon showed
        // "listening" while the daemon was idle- or mic-paused).
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
        // Tell the client which transcriber backend is active so the UI
        // shows the correct model selection after reconnect.
        DaemonEvent::ActiveTranscriberChanged {
            backend: active_backend,
        },
    ]);
    // Daemon-side tap health (Input Monitoring): a startup broadcast would
    // be lost (no clients yet), so each client gets the current status in
    // its initial burst.
    if let Some(granted) = crate::always::keyboard::input_monitoring_status() {
        initial_events.push(DaemonEvent::ShortcutListenerStatus {
            input_monitoring_granted: granted,
        });
    }
    // A stuck microphone recorder: a client connecting after the
    // supervisor's one-shot broadcast must still learn about it.
    if crate::always::audio::mic_unavailable() {
        initial_events.push(DaemonEvent::MicUnavailable { unavailable: true });
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

    // A known newer release (SPEC §12a): the client may connect long after
    // the check that found it.
    if let Some(update) = crate::always::updater::initial_burst_event() {
        initial_events.push(update);
    }

    let mut initial_payload = String::new();
    for event in initial_events {
        if let Ok(json_line) = event.to_json_line() {
            initial_payload.push_str(&json_line);
        }
    }

    // Send initial state in chunks to avoid socket buffer overflow
    if !initial_payload.is_empty() {
        const CHUNK_SIZE: usize = 64 * 1024; // 64KB chunks
        let mut pos = 0;

        while pos < initial_payload.len() {
            let end = std::cmp::min(pos + CHUNK_SIZE, initial_payload.len());
            let chunk = &initial_payload[pos..end];

            // Find the last complete line in this chunk
            let write_end = if let Some(last_newline) = chunk.rfind('\n') {
                pos + last_newline + 1
            } else {
                // No newline found, write the whole chunk
                end
            };

            if write_end <= pos {
                // Couldn't find a complete line, write what we have
                if let Err(e) = tokio::time::timeout(WRITE_TIMEOUT, async {
                    writer.write_all(&initial_payload.as_bytes()[pos..]).await?;
                    writer.flush().await
                })
                .await
                {
                    tracing::error!(error = %e, "uds_send_initial_state_failed");
                    return Ok(());
                }
                break;
            }

            let write_chunk = &initial_payload[pos..write_end];
            if let Err(e) = tokio::time::timeout(WRITE_TIMEOUT, async {
                writer.write_all(write_chunk.as_bytes()).await?;
                writer.flush().await
            })
            .await
            {
                tracing::error!(error = %e, "uds_send_initial_state_failed");
                return Ok(());
            }

            pos = write_end;
        }
    }

    // Send ModelsList separately after initial state
    let models = ctx.registry.list();
    let models_event = DaemonEvent::ModelsList { models };
    if let Ok(models_json) = models_event.to_json_line() {
        let _ = writer.write_all(models_json.as_bytes()).await;
        let _ = writer.flush().await;
    }

    // Read commands from the client in a separate task. Each line is a JSON
    // DaemonCommand. Executing them here (inside the daemon process) is what
    // makes the resulting events reach all UDS subscribers.
    let ctx_for_reader = ctx.clone();
    let consume_lease_for_reader = Arc::clone(&consume_lease);
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
                        Ok(cmd) => execute_command(cmd, &ctx_for_reader, &consume_lease_for_reader),
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
        pause::release_consume_mode(&consume_lease_for_reader);
    });

    // Send events to client.
    //
    // A broadcast receiver returns `Lagged` when the client fell further
    // behind than the channel's capacity. That is NOT a fatal condition —
    // it means some events were missed — but this loop used to be
    // `while let Ok(event) = rx.recv().await`, which treated it as the
    // end of the stream: the loop exited, `handle_client` returned, and
    // the socket closed. From the app's side the daemon had simply
    // vanished mid-session, so it reconnected, re-handshook and re-read
    // the whole initial state before events resumed — seconds during
    // which nothing reached the overlay, which is exactly the "the badge
    // takes about two seconds" report. Skip the gap and keep serving.
    loop {
        let event = match rx.recv().await {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "uds_event_stream_lagged");
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        };
        let json_line = match event.to_json_line() {
            Ok(line) => line,
            Err(e) => {
                tracing::error!(error = %e, "uds_serialize_event_failed");
                continue;
            }
        };

        if let Err(e) = writer.write_all(json_line.as_bytes()).await {
            // A client that vanishes during the initial-state burst (the
            // GUI opens a second connection and drops the first) surfaces
            // here as EPIPE on every boot. That is normal connection
            // lifecycle, not an error — log it at debug and stop writing.
            if matches!(
                e.kind(),
                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
            ) {
                tracing::debug!(error = %e, "uds_client_left_during_write");
            } else {
                tracing::error!(error = %e, "uds_write_failed");
            }
            break;
        }

        if let Err(e) = writer.flush().await {
            tracing::error!(error = %e, "uds_flush_failed");
            break;
        }
    }

    Ok(())
}

fn execute_command(cmd: DaemonCommand, ctx: &ModelCommandCtx, consume_lease: &AtomicBool) {
    match cmd {
        DaemonCommand::TogglePause => {
            // Same semantics as the master chord: while anything the user
            // may override is pausing (master, idle, mic conflict, audio
            // output), a toggle RESUMES. It used to flip master only, so
            // the idle pill's Play (and the tray / CLI) turned an idle or
            // mic-conflict pause into "paused everywhere".
            let out = crate::always::pause_commands::toggle_global();
            tracing::info!(
                master = out.master,
                effective = out.effective,
                changed = out.changed,
                "uds_toggle_pause"
            );
        }
        DaemonCommand::Resume => {
            let out = crate::always::pause_commands::resume();
            tracing::info!(
                master = out.master,
                effective = out.effective,
                changed = out.changed,
                "uds_resume"
            );
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
            if enabled {
                pause::acquire_consume_mode(consume_lease);
            } else {
                pause::release_consume_mode(consume_lease);
            }
            tracing::info!(enabled, "uds_set_consume_mode");
        }
        DaemonCommand::ReleaseHeld { id, text } => {
            let id_num = id.parse::<u64>().unwrap_or(0);
            let ok = crate::always::held::release(id_num, text.as_deref());
            tracing::info!(id = %id, ok, "uds_release_held");
        }
        DaemonCommand::DiscardHeld { id, text } => {
            let id_num = id.parse::<u64>().unwrap_or(0);
            let ok = crate::always::held::discard(id_num, text.as_deref());
            tracing::info!(id = %id, ok, "uds_discard_held");
        }
        DaemonCommand::PlayStatusCue { cue } => {
            match crate::always::status_sound::StatusSound::from_name(&cue) {
                Some(sound) => crate::always::status_sound::cue(sound),
                None => tracing::warn!(cue = %cue, "uds_play_status_cue_unknown"),
            }
        }
        DaemonCommand::SetActStatus {
            task,
            goal,
            text,
            action,
        } => {
            tracing::debug!(task = %task, goal_chars = goal.len(), text_chars = text.len(), action = %action, "uds_act_status_ignored");
        }
        DaemonCommand::PasteRejected { id } => {
            let id_num = id.parse::<u64>().unwrap_or(0);
            let ok = crate::always::event_loop::paste_rejected(id_num);
            tracing::info!(id = %id, ok, "uds_paste_rejected");
        }
        DaemonCommand::ApplyRuntimePreferences {
            auto_enter_delay_ms,
            energy_threshold,
            silence_secs,
            cooldown_ms,
            silero_threshold,
            adaptive_silence,
            audible_status_sound,
            stt_live_preview,
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
                stt_live_preview,
            );
        }
        DaemonCommand::ApproveCorrection { id } => handle_approve_correction(&id),
        DaemonCommand::RejectCorrection { id } => handle_reject_correction(&id),
        DaemonCommand::CaptureCorrection => handle_capture_correction(),
        DaemonCommand::SetPaused { paused, reason } => {
            // Clear idle BEFORE set_paused so the recompute inside sees
            // idle=false. The old order (set_paused → set_idle) left
            // EFFECTIVE_PAUSED stale when resuming while idle was set.
            if !paused {
                pause::set_idle_auto_paused(false);
                pause::mark_voice_seen();
            }
            // Peer-instance handoff: a dev/prod daemon that just started
            // pauses us with reason "peer-instance". Track the provenance
            // so the watchdog knows whether this pause is ours to lift —
            // any non-peer pause command clears it and wins.
            let by_peer = reason.as_deref() == Some("peer-instance");
            pause::set_paused_by_peer(paused && by_peer);
            let (effective, changed) = pause::set_paused(paused);
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
            if by_peer && paused {
                crate::always::daemon::spawn_peer_resume_watchdog();
            }
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
            // Media playing → the My Voice gate is more patient before an
            // early reject (own voice over a video scores lower).
            crate::always::vad::set_media_playing(playing);
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
                // Idle-paused: drop the audio source flag (no Resumed —
                // idle still pauses) so it can't strand a pause after idle
                // resume. The GUI must still hear the source cleared, or
                // its prediction keeps "audio playing" forever (#124).
                let _ = pause::set_audio_output_paused(false);
                global_broadcaster().pause_source_changed("audio_output", false, None);
            }
        }
        DaemonCommand::SetAppPaused { bundle_id, paused } => {
            // Write the override for `bundle_id` and recompute the
            // effective state. Broadcasting is gated on `changed` so
            // setting an override for a non-focused app doesn't spam
            // the UDS bus.
            // Plain allowlist editing: does not lift idle / watchdog pauses
            // (see `pause_commands::set_app_paused`).
            match crate::always::pause_commands::set_app_paused(&bundle_id, paused) {
                Ok(out) => {
                    tracing::info!(
                        bundle = %bundle_id,
                        paused = ?paused,
                        effective = out.effective,
                        changed = out.changed,
                        "uds_set_app_paused"
                    );
                }
                Err(e) => {
                    tracing::error!(error = %e, bundle = %bundle_id, "uds_set_app_paused_failed");
                }
            }
        }
        DaemonCommand::VoiceEdit {
            req,
            command,
            force,
        } => {
            std::thread::spawn(move || {
                let o = crate::always::voice_edit::run(
                    &command,
                    force,
                    "iris",
                    crate::always::ax_edit::KeyTarget::Hid,
                    None,
                );
                // Claimed edits already broadcast; answer declines too so
                // the requester never waits on silence.
                if !o.claimed || !req.is_empty() {
                    global_broadcaster().send(DaemonEvent::VoiceEdit {
                        req,
                        op: o.op.to_string(),
                        ok: o.claimed.to_string(),
                        method: o.method.to_string(),
                        ms: format!("{:.1}", o.ms),
                        detail: o.reason.to_string(),
                        text: String::new(),
                    });
                }
            });
        }
        DaemonCommand::VoiceEditRewriterAlive => {
            crate::always::voice_edit::note_rewriter_alive();
        }
        DaemonCommand::VoiceEditGetTarget { req } => {
            std::thread::spawn(move || {
                let t0 = std::time::Instant::now();
                let t = crate::always::voice_edit::current_target(None);
                let detail = if t.secure { "secure" } else { t.source };
                global_broadcaster().send(DaemonEvent::VoiceEdit {
                    req,
                    op: "get_target".into(),
                    ok: (t.source != "none").to_string(),
                    method: "ax".into(),
                    ms: format!("{:.1}", t0.elapsed().as_secs_f64() * 1000.0),
                    detail: detail.into(),
                    text: t.text,
                });
            });
        }
        DaemonCommand::VoiceEditReplaceTarget { req, expect, text } => {
            std::thread::spawn(move || {
                let r = crate::always::voice_edit::replace_target(
                    &expect,
                    &text,
                    None,
                    crate::always::ax_edit::KeyTarget::Hid,
                );
                tracing::info!(ok = r.is_ok(), result = ?r, "voice_edit_replace_target");
                let (ok, method, ms, detail) = match r {
                    Ok((m, ms)) => (true, m, ms, "ok"),
                    Err(e) => (false, "", 0.0, e),
                };
                global_broadcaster().send(DaemonEvent::VoiceEdit {
                    req,
                    op: "replace_target".into(),
                    ok: ok.to_string(),
                    method: method.into(),
                    ms: format!("{ms:.1}"),
                    detail: detail.into(),
                    text: String::new(),
                });
            });
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
        DaemonCommand::GetShortcutListenerStatus => {
            // Re-send only a known status; "unknown" (listener not started
            // yet) is announced by the listener itself when it reports.
            if let Some(granted) = crate::always::keyboard::input_monitoring_status() {
                global_broadcaster().shortcut_listener_status(granted);
            }
        }
        DaemonCommand::RespawnRecorder => {
            // macOS reports the default input device changed. The running
            // `rec` opened the old device at spawn time and won't follow
            // the switch — force a respawn so the new mic is picked up
            // without an app relaunch. On non-macOS builds there is no
            // recorder to respawn (the CLI path uses a different source),
            // so the command is a no-op there.
            #[cfg(target_os = "macos")]
            {
                if let Err(e) = crate::always::audio::RecChild::force_respawn() {
                    tracing::error!(error = %e, "uds_respawn_recorder_failed");
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                tracing::debug!("RespawnRecorder ignored — no recorder on this build");
            }
        }
        DaemonCommand::ReloadShortcuts => {
            // Settings → Shortcuts saved a new combo. Re-read the prefs
            // DB and swap the live keyboard listener's shortcut set.
            crate::always::keyboard::reload_shortcuts();
        }
        DaemonCommand::SetupMode { enabled } => {
            // Toggle setup mode: when enabled, bypasses per-app pause
            // rules so recording and pasting work during onboarding even
            // when the focused app is Always itself.
            pause::set_setup_mode(enabled);
            pause::recompute_effective();
            tracing::info!(enabled, "setup_mode_toggled");
        }
        // Both run on their own thread (blocking network work) and answer
        // through the global broadcaster. See SPEC §12a.
        DaemonCommand::CheckForUpdates => crate::always::updater::request_check(),
        DaemonCommand::InstallUpdate => crate::always::updater::request_install(),
    }
}

/// Held for the whole of `switch_active_backend` / `set_language`. The engine
/// build runs outside the config lock (it would freeze dictation), so without
/// this two UDS clients could interleave — `set_language` snapshotting the
/// config mid-switch — leaving the config naming backend A while the active
/// slot holds backend B. The main loop never takes this lock.
static TRANSCRIBER_REBUILD: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Update the transcription language live: write `cfg.lang`, persist to the
/// prefs DB, and rebuild the active transcriber so engines that bake the
/// language into their decode (Canary, Cohere, SenseVoice) pick it up
/// without a daemon restart.
fn set_language(ctx: &ModelCommandCtx, lang: &str) {
    let _rebuild = TRANSCRIBER_REBUILD.lock();
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
    let _rebuild = TRANSCRIBER_REBUILD.lock();
    let outcome = switch_backend_with(&ctx.cfg, &choice, |cfg| {
        build_transcriber(cfg, &ctx.registry)
    });
    let active_now = ctx.cfg.read().transcriber_backend.to_string();
    match &outcome {
        SwitchOutcome::Noop => {
            tracing::debug!(backend = %choice, "uds_set_active_transcriber_noop");
        }
        SwitchOutcome::Switched(transcriber) => {
            *ctx.active.write() = Arc::clone(transcriber);
            // Persist so the next daemon start uses the same choice.
            if let Ok(conn) = crate::db::open() {
                let value = choice.to_string();
                if let Err(e) = crate::db::set_preference(&conn, "transcriber_backend", &value) {
                    tracing::warn!(error = %e, "persist_transcriber_backend_failed");
                }
            }
            tracing::info!(backend = %choice, "uds_set_active_transcriber");
        }
        SwitchOutcome::Failed(error) => {
            tracing::error!(backend = %choice, error = %error, "build_new_transcriber_failed");
        }
    }
    for event in switch_events(&choice, &active_now, &outcome) {
        global_broadcaster().send(event);
    }
}

/// Result of a backend switch request.
enum SwitchOutcome<T> {
    /// Already the active backend — nothing rebuilt.
    Noop,
    /// The new engine built and is now the configured backend.
    Switched(T),
    /// The new engine failed to build; the config still names the backend
    /// that was active before, which keeps running.
    Failed(String),
}

/// Switch `cfg` to `choice`, building the engine with `build`.
///
/// The build (for a local model: loading ONNX sessions from disk and a
/// warm-up decode, seconds on a cold disk) runs on a CLONE of the config,
/// never under the config write lock — the main loop reads that config
/// every utterance, so holding it froze dictation for the whole load.
/// The config only names `choice` once the engine exists: a failed build
/// leaves it on the backend that is still actually running, so a retry is
/// a real retry instead of a silent "already active" no-op.
///
/// Selecting the backend that is already active is free (no rebuild):
/// rebuilding on every echo was a measured ten-rebuilds-in-one-second
/// stall when the GUI's picker re-emitted its selection.
fn switch_backend_with<T>(
    cfg: &ActiveConfig,
    choice: &TranscriberBackendChoice,
    build: impl FnOnce(&crate::always::AlwaysConfig) -> Result<T>,
) -> SwitchOutcome<T> {
    let mut candidate = {
        let current = cfg.read();
        if current.transcriber_backend == *choice {
            return SwitchOutcome::Noop;
        }
        current.clone()
    };
    candidate.transcriber_backend = choice.clone();
    match build(&candidate) {
        Ok(t) => {
            cfg.write().transcriber_backend = choice.clone();
            SwitchOutcome::Switched(t)
        }
        Err(e) => SwitchOutcome::Failed(format!("{e:#}")),
    }
}

/// What the GUI is told about a switch. EVERY outcome ends with the
/// backend that is active now: the Models tab shows "Loading…" from the
/// click until an `ActiveTranscriberChanged` arrives, so an outcome that
/// sent nothing (a no-op, a failed load) left that spinner on forever.
/// A failure first says why, so the row shows the error instead.
fn switch_events<T>(
    choice: &TranscriberBackendChoice,
    active_now: &str,
    outcome: &SwitchOutcome<T>,
) -> Vec<DaemonEvent> {
    let mut events = Vec::with_capacity(2);
    if let SwitchOutcome::Failed(error) = outcome {
        events.push(DaemonEvent::ActiveTranscriberFailed {
            backend: choice.to_string(),
            error: error.clone(),
        });
    }
    events.push(DaemonEvent::ActiveTranscriberChanged {
        backend: active_now.to_string(),
    });
    events
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
    stt_live_preview: Option<bool>,
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
    if let Some(live_preview) = stt_live_preview {
        cfg.stt_live_preview = live_preview;
    }
    tracing::info!(
        auto_enter_delay_ms = cfg.auto_enter_delay_ms,
        energy_threshold = cfg.energy_threshold,
        silence_secs = cfg.silence_secs,
        cooldown_ms = cfg.cooldown_ms,
        silero_threshold = cfg.silero_threshold,
        adaptive_silence = cfg.adaptive_silence_enabled,
        audible_status_sound = cfg.audible_status_sound.as_str(),
        stt_live_preview = cfg.stt_live_preview,
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
    let auto_learn = auto_learn_corrections_enabled();
    let outcome = match correction::capture_via_hotkey(
        crate::always::clipboard_watcher::PASTE_WINDOW,
        auto_learn,
    ) {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(error = %e, "uds_capture_correction_failed");
            return;
        }
    };
    match outcome {
        correction::CaptureOutcome::Applied {
            pairs,
            applied: _,
            original,
            corrected,
        } => {
            for p in &pairs {
                global_broadcaster().correction_logged(&p.wrong, &p.right);
            }
            spawn_llm_extraction(original, corrected, pairs, auto_learn);
        }
        correction::CaptureOutcome::Extracted {
            pairs,
            original,
            corrected,
        } => {
            for p in &pairs {
                global_broadcaster().correction_logged(&p.wrong, &p.right);
            }
            spawn_llm_extraction(original, corrected, pairs, auto_learn);
        }
        _ => {}
    }
}

/// Spawn a background LLM second-opinion extraction (UDS path).
fn spawn_llm_extraction(
    original: String,
    corrected: String,
    already_found: Vec<crate::always::correction::CorrectionPair>,
    auto_apply: bool,
) {
    use crate::always::correction_extract;

    let Some(api_key) = correction_extract::get_extraction_api_key() else {
        return;
    };

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                tracing::error!(error = %e, "llm_extraction_runtime_failed");
                return;
            }
        };

        let model = correction_extract::get_extraction_model();
        let result = rt.block_on(correction_extract::extract_corrections_via_llm(
            &original,
            &corrected,
            &api_key,
            &model,
            &already_found,
        ));

        match result {
            Ok(new_pairs) if !new_pairs.is_empty() => {
                tracing::info!(
                    count = new_pairs.len(),
                    "llm_extraction_found_additional_pairs"
                );
                crate::always::correction_metrics::record(
                    crate::always::correction_metrics::MetricEvent::LlmExtractionFound,
                );
                if auto_apply {
                    for p in &new_pairs {
                        global_broadcaster().correction_logged(&p.wrong, &p.right);
                    }
                    if let Err(e) =
                        crate::always::correction::apply_pairs_to_glossary_with_provenance(
                            &new_pairs,
                            crate::always::correction::Provenance::Llm,
                        )
                    {
                        tracing::error!(error = %e, "llm_extraction_apply_failed");
                    }
                }
            }
            Ok(_) => {
                crate::always::correction_metrics::record(
                    crate::always::correction_metrics::MetricEvent::LlmExtractionEmpty,
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "llm_extraction_failed");
                crate::always::correction_metrics::record(
                    crate::always::correction_metrics::MetricEvent::LlmExtractionFailed,
                );
            }
        }
    });
}

/// Read the `auto_learn_corrections` preference from the DB.
/// Defaults to `true` when unavailable.
fn auto_learn_corrections_enabled() -> bool {
    let Ok(conn) = crate::db::open() else {
        return true;
    };
    let Ok(prefs) = crate::db::get_preferences(&conn) else {
        return true;
    };
    prefs.auto_learn_corrections.unwrap_or(true)
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

#[cfg(all(test, windows))]
mod private_pipe_security_tests {
    use std::ffi::c_void;
    use std::ptr::null_mut;

    use super::PRIVATE_PIPE_SDDL;

    #[repr(C)]
    struct AclSizeInformation {
        ace_count: u32,
        acl_bytes_in_use: u32,
        acl_bytes_free: u32,
    }

    #[repr(C)]
    struct AceHeader {
        ace_type: u8,
        ace_flags: u8,
        ace_size: u16,
    }

    #[repr(C)]
    struct AccessAllowedAce {
        header: AceHeader,
        mask: u32,
        sid_start: u32,
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_string: *const u16,
            revision: u32,
            descriptor: *mut *mut c_void,
            descriptor_size: *mut u32,
        ) -> i32;
        fn ConvertStringSidToSidW(string_sid: *const u16, sid: *mut *mut c_void) -> i32;
        fn GetSecurityDescriptorDacl(
            descriptor: *mut c_void,
            dacl_present: *mut i32,
            dacl: *mut *mut c_void,
            dacl_defaulted: *mut i32,
        ) -> i32;
        fn GetSecurityDescriptorControl(
            descriptor: *mut c_void,
            control: *mut u16,
            revision: *mut u32,
        ) -> i32;
        fn GetAclInformation(
            acl: *mut c_void,
            information: *mut c_void,
            information_length: u32,
            information_class: u32,
        ) -> i32;
        fn GetAce(acl: *mut c_void, ace_index: u32, ace: *mut *mut c_void) -> i32;
        fn EqualSid(sid1: *mut c_void, sid2: *mut c_void) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    #[test]
    fn generated_pipe_descriptor_allows_only_owner_and_system() {
        let mut descriptor = null_mut();
        let sddl = wide(PRIVATE_PIPE_SDDL);
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    1,
                    &mut descriptor,
                    null_mut(),
                )
            },
            0
        );

        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = null_mut();
        assert_ne!(
            unsafe {
                GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
            },
            0
        );
        assert_eq!(present, 1);
        assert_eq!(defaulted, 0);
        assert!(!dacl.is_null());

        let mut control = 0;
        let mut revision = 0;
        assert_ne!(
            unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) },
            0
        );
        assert_ne!(
            control & 0x1000,
            0,
            "DACL must be protected from inheritance"
        );

        let mut acl_info = AclSizeInformation {
            ace_count: 0,
            acl_bytes_in_use: 0,
            acl_bytes_free: 0,
        };
        assert_ne!(
            unsafe {
                GetAclInformation(
                    dacl,
                    (&mut acl_info as *mut AclSizeInformation).cast(),
                    std::mem::size_of::<AclSizeInformation>() as u32,
                    2,
                )
            },
            0
        );
        assert_eq!(
            acl_info.ace_count, 2,
            "unexpected additional principals in pipe DACL"
        );

        let owner_sid_string = wide("S-1-3-4"); // OWNER RIGHTS
        let system_sid_string = wide("S-1-5-18"); // LOCAL SYSTEM
        let mut owner_sid = null_mut();
        let mut system_sid = null_mut();
        assert_ne!(
            unsafe { ConvertStringSidToSidW(owner_sid_string.as_ptr(), &mut owner_sid) },
            0
        );
        assert_ne!(
            unsafe { ConvertStringSidToSidW(system_sid_string.as_ptr(), &mut system_sid) },
            0
        );

        let mut saw_owner = false;
        let mut saw_system = false;
        for index in 0..acl_info.ace_count {
            let mut ace_ptr = null_mut();
            assert_ne!(unsafe { GetAce(dacl, index, &mut ace_ptr) }, 0);
            let ace = unsafe { &*(ace_ptr as *const AccessAllowedAce) };
            assert_eq!(
                ace.header.ace_type, 0,
                "pipe ACL must only contain allow ACEs"
            );
            assert_eq!(
                ace.mask, 0x1000_0000,
                "ACE must grant generic all only to private principals"
            );
            let sid = (&ace.sid_start as *const u32).cast_mut().cast::<c_void>();
            if unsafe { EqualSid(sid, owner_sid) } != 0 {
                saw_owner = true;
            } else if unsafe { EqualSid(sid, system_sid) } != 0 {
                saw_system = true;
            } else {
                panic!("pipe DACL contains an unexpected principal");
            }
        }
        assert!(saw_owner && saw_system);

        unsafe {
            LocalFree(owner_sid);
            LocalFree(system_sid);
            LocalFree(descriptor);
        }
    }
}

#[cfg(test)]
mod switch_backend_tests {
    use super::*;
    use parking_lot::RwLock;

    fn cfg_on(choice: TranscriberBackendChoice) -> ActiveConfig {
        Arc::new(RwLock::new(crate::always::AlwaysConfig {
            transcriber_backend: choice,
            ..Default::default()
        }))
    }

    fn parakeet() -> TranscriberBackendChoice {
        "local:parakeet-tdt-0.6b-v3".parse().unwrap()
    }

    fn backends(events: &[DaemonEvent]) -> Vec<String> {
        events
            .iter()
            .map(|e| match e {
                DaemonEvent::ActiveTranscriberChanged { backend } => format!("changed:{backend}"),
                DaemonEvent::ActiveTranscriberFailed { backend, error } => {
                    format!("failed:{backend}:{error}")
                }
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// Ludwig (16 Oct): Parakeet v3 "Use" stuck on "Loading…". A failed
    /// load must say why AND re-announce the backend still running, and
    /// must not leave the config claiming the model that failed.
    #[test]
    fn failed_load_reports_error_and_keeps_previous_backend() {
        let cfg = cfg_on(TranscriberBackendChoice::Groq);
        let outcome: SwitchOutcome<()> =
            switch_backend_with(&cfg, &parakeet(), |_| anyhow::bail!("onnx: file missing"));
        assert!(matches!(outcome, SwitchOutcome::Failed(_)));
        assert_eq!(
            cfg.read().transcriber_backend,
            TranscriberBackendChoice::Groq
        );
        let active = cfg.read().transcriber_backend.to_string();
        assert_eq!(
            backends(&switch_events(&parakeet(), &active, &outcome)),
            vec![
                "failed:local:parakeet-tdt-0.6b-v3:onnx: file missing".to_string(),
                "changed:groq".to_string()
            ]
        );
        // A retry is a real retry, not an "already active" no-op.
        let retry: SwitchOutcome<()> = switch_backend_with(&cfg, &parakeet(), |_| Ok(()));
        assert!(matches!(retry, SwitchOutcome::Switched(())));
        assert_eq!(cfg.read().transcriber_backend, parakeet());
    }

    #[test]
    fn failed_event_wire_shape_matches_gui() {
        let e = DaemonEvent::ActiveTranscriberFailed {
            backend: "local:parakeet-tdt-0.6b-v3".into(),
            error: "file missing".into(),
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"ActiveTranscriberFailed","data":{"backend":"local:parakeet-tdt-0.6b-v3","error":"file missing"}}"#
        );
    }

    #[test]
    fn noop_still_confirms_so_the_spinner_clears() {
        let cfg = cfg_on(parakeet());
        let mut built = false;
        let outcome: SwitchOutcome<()> = switch_backend_with(&cfg, &parakeet(), |_| {
            built = true;
            Ok(())
        });
        assert!(!built, "already-active backend must not rebuild");
        assert_eq!(
            backends(&switch_events(
                &parakeet(),
                &parakeet().to_string(),
                &outcome
            )),
            vec!["changed:local:parakeet-tdt-0.6b-v3".to_string()]
        );
    }

    #[test]
    fn build_runs_without_holding_the_config_lock() {
        let cfg = cfg_on(TranscriberBackendChoice::Groq);
        let probe = Arc::clone(&cfg);
        let outcome: SwitchOutcome<()> = switch_backend_with(&cfg, &parakeet(), |candidate| {
            assert_eq!(candidate.transcriber_backend, parakeet());
            // The main loop must still be able to read AND write the live
            // config while the engine loads.
            assert!(probe.try_write().is_some(), "config locked during load");
            Ok(())
        });
        assert!(matches!(outcome, SwitchOutcome::Switched(())));
        assert_eq!(
            backends(&switch_events(
                &parakeet(),
                &parakeet().to_string(),
                &outcome
            )),
            vec!["changed:local:parakeet-tdt-0.6b-v3".to_string()]
        );
    }
}
