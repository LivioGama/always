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
    }
}

/// Send an event to all connected clients
pub fn send_event(event: DaemonEvent) {
    global_broadcaster().send(event);
}
