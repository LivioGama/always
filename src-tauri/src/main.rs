use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, mpsc};
use tauri::{Emitter, Manager};

// ─── UDS path resolution ─────────────────────────────────────────────────────

fn socket_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME environment variable not set");
    if cfg!(target_os = "macos") {
        PathBuf::from(home)
            .join("Library")
            .join("Caches")
            .join("Always")
            .join("always.sock")
    } else if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir).join("always.sock")
    } else {
        PathBuf::from("/tmp/always.sock")
    }
}

// ─── Shared connection state ─────────────────────────────────────────────────

/// State shared between the Tauri commands and the background UDS connection.
struct UdsContext {
    connected: Arc<Mutex<bool>>,
    tx: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
    _reader_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl Clone for UdsContext {
    fn clone(&self) -> Self {
        Self {
            connected: Arc::clone(&self.connected),
            tx: Arc::clone(&self.tx),
            _reader_handle: Arc::clone(&self._reader_handle),
        }
    }
}

/// Get the app state (or create it on first access)
fn get_uds_ctx(app: &tauri::AppHandle) -> Result<Arc<UdsContext>> {
    if let Some(ctx) = app.state::<Arc<UdsContext>>().into_inner() {
        return Ok(ctx);
    }
    let ctx = Arc::new(UdsContext {
        connected: Arc::new(Mutex::new(false)),
        tx: Arc::new(Mutex::new(None)),
        _reader_handle: Arc::new(Mutex::new(None)),
    });
    app.manage(ctx.clone());
    Ok(ctx)
}

// ─── Tauri v2 commands ──────────────────────────────────────────────────────

/// Send a DaemonCommand to the daemon via UDS.
#[tauri::command]
async fn uds_send(
    app: tauri::AppHandle,
    data: String,
) -> Result<(), String> {
    let ctx = get_uds_ctx(&app)
        .map_err(|e| e.to_string())?;

    let locked_tx = ctx.tx.lock().await;
    let tx = locked_tx.as_ref()
        .ok_or("Not connected to daemon")?;

    tx.send(data)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Connect to the daemon's UDS socket.
#[tauri::command]
async fn uds_connect(
    app: tauri::AppHandle,
) -> Result<bool, String> {
    let ctx = get_uds_ctx(&app)
        .map_err(|e| e.to_string())?;

    // If already connected, just confirm
    if *ctx.connected.lock().await {
        return Ok(true);
    }

    let path = socket_path();

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Try to connect
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        UnixStream::connect(&path),
    )
    .await
    .map_err(|_| "Connection to daemon timed out".to_string())?
    .map_err(|e| format!("Failed to connect to daemon: {e}"))?;

    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // Keep handles alive
    let ctx_reader = ctx.clone();
    let ctx_writer = ctx.clone();

    // Spawn the read loop — reads daemon events and emits them to the frontend
    let read_handle = tokio::spawn(async move {
        let app = app.clone();
        let mut line = String::new();
        loop {
            line.clear();
            match tokio::time::timeout(
                std::time::Duration::from_secs(30),
                reader.read_line(&mut line),
            )
            .await
            {
                Ok(Ok(0)) => {
                    tracing::info!("uds_reader_eof: daemon disconnected");
                    break;
                }
                Ok(Ok(_)) => {
                    let trimmed = line.trim().to_string();
                    if trimmed.is_empty() {
                        continue;
                    }

                    // Emit the raw JSON event to the frontend
                    if let Err(e) = app.emit("uds_event", &trimmed) {
                        tracing::warn!(error = %e, "failed to emit uds_event");
                    }

                    // Detect Hello as connection confirmation
                    if trimmed.contains("\"type\":\"Hello\"")
                        || trimmed.contains("\"type\": \"Hello\"")
                    {
                        *ctx_reader.connected.lock().await = true;
                        if let Err(e) = app.emit("uds_connected", ()) {
                            tracing::warn!(error = %e, "failed to emit uds_connected");
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!(error = %e, "uds_read_error");
                    break;
                }
                Err(_) => {
                    tracing::warn!("uds_read_timeout");
                    break;
                }
            }
        }

        // Connection lost
        if let Err(e) = app.emit("uds_disconnected", ()) {
            tracing::warn!(error = %e, "failed to emit uds_disconnected");
        }
        *ctx_reader.connected.lock().await = false;
    });

    // Spawn the write loop — forwards commands from frontend to daemon
    let write_handle = tokio::spawn(async move {
        let mut line = String::new();
        while let Some(cmd) = rx.recv().await {
            line.clear();
            line.push_str(&cmd);
            if !line.ends_with('\n') {
                line.push('\n');
            }
            if writer.write_all(line.as_bytes()).await.is_err() {
                tracing::error!("uds_write_error");
                break;
            }
            if writer.flush().await.is_err() {
                tracing::error!("uds_flush_error");
                break;
            }
        }
    });

    // Store connection state
    *ctx.connected.lock().await = true;
    *ctx.tx.lock().await = Some(tx);
    *ctx._reader_handle.lock().await = Some(read_handle);

    // Keep the writer handle alive until the connection dies
    tokio::spawn(async move {
        tokio::join!(read_handle, write_handle);
        *ctx.connected.lock().await = false;
    });

    Ok(true)
}

/// Check if we're connected to the daemon
#[tauri::command]
async fn uds_is_connected(app: tauri::AppHandle) -> bool {
    if let Some(ctx) = app.state::<Arc<UdsContext>>().into_inner() {
        *ctx.connected.lock().await
    } else {
        false
    }
}

/// Disconnect from the daemon
#[tauri::command]
async fn uds_disconnect(app: tauri::AppHandle) {
    if let Some(ctx) = app.state::<Arc<UdsContext>>().into_inner() {
        *ctx.tx.lock().await = None;
        *ctx.connected.lock().await = false;
    }
}

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "always_tauri=info,uds=debug".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::builder().build())
        .setup(|app| {
            // Auto-connect to daemon on startup
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Give the frontend time to initialize
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                match uds_connect(app_handle.clone()).await {
                    Ok(_) => {
                        tracing::info!("tauri_auto_connected_to_daemon");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "tauri_failed_to_connect_to_daemon");
                        if let Err(e) = app_handle.emit("uds_connection_error", serde_json::json!({ "message": e })) {
                            tracing::warn!(error = %e, "failed_to_emit_connection_error");
                        }
                    }
                }
            });

            // Check for updates in the background (non-blocking)
            let update_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Some(update) = match update_handle.updater().check().await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(error = %e, "update_check_failed");
                        return;
                    }
                } {
                    if update.is_update() {
                        tracing::info!(
                            update_version = update.version,
                            update_target = update.target.as_deref().unwrap_or("all"),
                            "update_available"
                        );
                        // Notify frontend that an update is available
                        if let Err(e) = update_handle.emit("update_available", serde_json::json!({
                            "version": update.version,
                            "current_version": update.current_version,
                            "body": update.body.as_deref().unwrap_or("Update available"),
                        })) {
                            tracing::warn!(error = %e, "failed_to_emit_update_available");
                        }
                    }
                }
            });

            Ok(())
        })
        .manage(Arc::new(UdsContext {
            connected: Arc::new(Mutex::new(false)),
            tx: Arc::new(Mutex::new(None)),
            _reader_handle: Arc::new(Mutex::new(None)),
        }))
        .invoke_handler(tauri::generate_handler![
            uds_send,
            uds_connect,
            uds_is_connected,
            uds_disconnect,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}