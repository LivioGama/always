//! UDS client for connecting to the daemon and receiving events
//!
//! This module handles the Unix domain socket connection to the daemon,
//! decodes newline-delimited JSON events, and provides a stream of events.

use super::state::{DaemonEvent, parse_daemon_event};
use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tracing::{error, info, warn};

pub struct UDSClient {
    reader: BufReader<UnixStream>,
}

impl UDSClient {
    /// Create a new UDS client and connect to the daemon socket
    pub async fn new(socket_path: PathBuf) -> Result<Self> {
        info!("Connecting to daemon socket: {}", socket_path.display());

        let stream = UnixStream::connect(&socket_path).await.with_context(|| {
            format!(
                "Failed to connect to daemon socket at {}",
                socket_path.display()
            )
        })?;

        info!("Connected to daemon");

        Ok(Self {
            reader: BufReader::new(stream),
        })
    }

    /// Read the next event from the daemon
    /// Returns Ok(Some(event)) on success, Ok(None) on connection closed, Err on error
    pub async fn next_event(&mut self) -> Result<Option<DaemonEvent>> {
        let mut line = String::new();

        match self.reader.read_line(&mut line).await {
            Ok(0) => {
                // Connection closed
                warn!("Daemon connection closed");
                Ok(None)
            }
            Ok(_) => {
                // Parse JSON event
                let event = parse_daemon_event(&line)
                    .with_context(|| format!("Failed to parse daemon event: {}", line.trim()))?;

                Ok(Some(event))
            }
            Err(e) => {
                error!("Failed to read from daemon socket: {}", e);
                Err(e.into())
            }
        }
    }
}
