//! Linux overlay companion process for Always
//!
//! This is a separate process that connects to the daemon via UDS socket,
//! receives daemon events, and renders overlay UI. It mirrors the macOS
//! overlay behavior using the same daemon event stream.

mod renderer;
mod state;
mod uds_client;

use renderer::OverlayRenderer;
use state::{DaemonEvent, OverlayStateReducer};
use uds_client::UDSClient;

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::Duration;
use tracing::{error, info, warn};

fn get_socket_path() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/run/user/{}/always.sock", uid))
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter("always_overlay=debug,info")
        .init();

    info!("Starting Always overlay companion process");

    let socket_path = get_socket_path();
    info!("Connecting to daemon at: {}", socket_path.display());

    // Create state reducer
    let mut reducer = OverlayStateReducer::new();

    // Create overlay renderer
    let mut renderer = OverlayRenderer::new().context("Failed to initialize overlay renderer")?;

    // Main loop with reconnection logic
    loop {
        // Try to connect to daemon
        match UDSClient::new(socket_path.clone()).await {
            Ok(mut uds_client) => {
                info!("Connected to daemon, starting event loop");

                // Run event loop
                let mut timeout_interval = tokio::time::interval(Duration::from_millis(100));

                loop {
                    tokio::select! {
                        // Handle daemon events
                        event_result = uds_client.next_event() => {
                            match event_result {
                                Ok(Some(event)) => {
                                    info!("Received daemon event: {:?}", std::mem::discriminant(&event));

                                    // Validate protocol version
                                    if let DaemonEvent::Hello { version } = &event {
                                        if *version != 7 {
                                            warn!("Protocol version mismatch: expected 7, got {}", version);
                                        }
                                    }

                                    // Process event through state reducer
                                    if let Some(new_state) = reducer.process_event(&event) {
                                        info!("Overlay state changed: {:?}", new_state);

                                        // Update renderer with new state
                                        if let Err(e) = renderer.update_state(new_state).await {
                                            error!("Failed to update renderer: {}", e);
                                        }
                                    }
                                }
                                Ok(None) => {
                                    warn!("UDS connection closed");
                                    break;
                                }
                                Err(e) => {
                                    error!("UDS error: {}", e);
                                    break;
                                }
                            }
                        }

                        // Check timeouts (flash expiry, initial sync)
                        _ = timeout_interval.tick() => {
                            if let Some(new_state) = reducer.check_timeouts() {
                                info!("Timeout triggered state change: {:?}", new_state);
                                if let Err(e) = renderer.update_state(new_state).await {
                                    error!("Failed to update renderer: {}", e);
                                }
                            }
                        }

                        // Handle renderer events (e.g., window close)
                        _ = renderer.wait_for_event() => {}
                    }
                }
            }
            Err(e) => {
                error!("Failed to connect to daemon: {}", e);
                info!("Retrying in 5 seconds...");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}
