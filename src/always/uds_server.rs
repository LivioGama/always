use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use super::event::{global_broadcaster, DaemonCommand, DaemonEvent};
use super::pause;

/// Unix Domain Socket path for daemon-to-GUI communication
pub fn socket_path() -> Result<PathBuf> {
    let path = std::path::PathBuf::from("/tmp/always.sock");
    Ok(path)
}

/// Start the Unix Domain Socket server
pub async fn start_server() -> Result<()> {
    let socket_path = socket_path()?;

    // Remove existing socket file if it exists
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    let listener = UnixListener::bind(&socket_path)
        .context("Failed to bind Unix Domain Socket")?;

    eprintln!("🔌 UDS server listening on {}", socket_path.display());

    // Accept connections in a loop
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                tokio::spawn(async move {
                    let _ = handle_client(stream).await;
                });
            }
            Err(e) => {
                eprintln!("Failed to accept connection: {}", e);
            }
        }
    }
}

/// Handle a single client connection
async fn handle_client(stream: UnixStream) -> Result<()> {
    let mut rx = global_broadcaster().subscribe();
    let (reader, mut writer) = stream.into_split();
    let reader = BufReader::new(reader);

    // Send current state on connection
    let is_paused = pause::is_paused();
    let is_auto_enter = pause::is_auto_enter_enabled();

    let initial_events = vec![
        if is_paused { DaemonEvent::Paused } else { DaemonEvent::Resumed },
        if is_auto_enter { DaemonEvent::AutoEnterEnabled } else { DaemonEvent::AutoEnterDisabled },
        DaemonEvent::ListeningStarted,
    ];

    for event in initial_events {
        if let Ok(json_line) = event.to_json_line() {
            if let Err(e) = writer.write_all(json_line.as_bytes()).await {
                eprintln!("Failed to send initial state: {}", e);
                return Ok(());
            }
        }
    }

    if let Err(e) = writer.flush().await {
        eprintln!("Failed to flush initial state: {}", e);
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
                    match DaemonCommand::from_json_line(trimmed) {
                        Ok(cmd) => execute_command(cmd),
                        Err(e) => eprintln!("UDS: failed to parse command '{}': {}", trimmed, e),
                    }
                }
                Err(e) => {
                    eprintln!("UDS: read error: {}", e);
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
                eprintln!("Failed to serialize event: {}", e);
                continue;
            }
        };

        if let Err(e) = writer.write_all(json_line.as_bytes()).await {
            eprintln!("Failed to write to client: {}", e);
            break;
        }

        if let Err(e) = writer.flush().await {
            eprintln!("Failed to flush: {}", e);
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
            eprintln!("UDS: TogglePause -> {}", if new_state { "paused" } else { "resumed" });
        }
        DaemonCommand::ToggleAutoEnter => {
            let new_state = pause::toggle_auto_enter();
            if new_state {
                global_broadcaster().auto_enter_enabled();
            } else {
                global_broadcaster().auto_enter_disabled();
            }
            eprintln!("UDS: ToggleAutoEnter -> {}", if new_state { "enabled" } else { "disabled" });
        }
    }
}

/// Send an event to all connected clients
pub fn send_event(event: DaemonEvent) {
    global_broadcaster().send(event);
}
