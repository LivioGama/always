//! Thin client for the Always daemon over Unix Domain Socket.
//!
//! Connects to the platform-specific socket path via
//! [`dirs::cache_dir()`]. Uses the JSON-over-lines protocol v12 that the
//! daemon (`always` crate) speaks — each line is a standalone JSON object
//! with `{"type":"…","data":…}` structure.
//!
//! Reconnects with exponential backoff (1 s .. 30 s max) so transient
//! daemon restarts or cold-start pauses do not tear down the GUI.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
#[cfg(windows)]
use tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::sync::{mpsc, Mutex};

// ── Protocol version ──────────────────────────────────────────────────────

/// Protocol version negotiated on connect. Must match the daemon's
/// [`PROTOCOL_VERSION`](always::always::event::PROTOCOL_VERSION).
pub const PROTOCOL_VERSION: u32 = 12;

// ── Re-exported event / command types ──────────────────────────────────────
///
/// These enums mirror the daemon's [`DaemonEvent`](always::always::event::DaemonEvent)
/// and [`DaemonCommand`](always::all::event::DaemonCommand) exactly.
/// They are kept here so the Tauri crate is self-contained — the `always`
/// binary crate is not required at link time.
///
/// Tagged serde format: `{"type":"…","data":…}` per line.

/// Events sent **from the daemon** to connected GUI clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DaemonEvent {
    /// First frame on every connection — carries the daemon's protocol version.
    Hello {
        version: u32,
    },

    /// Ground-truth about the daemon's keyboard event tap authorisation.
    ShortcutListenerStatus {
        input_monitoring_granted: bool,
    },
    ListeningStarted,
    ListeningStopped,
    ProcessingStarted,
    ProcessingStopped,
    TranscribingStarted,
    TranscribingStopped,
    TranscriptChunk {
        text: String,
    },
    TranscriptFinal {
        text: String,
    },
    TranscriptionInterim {
        text: String,
    },
    GrammarCorrected {
        before: String,
        after: String,
    },

    Paused,
    Resumed,
    PausedQuietly,
    ResumedQuietly,

    AutoEnterEnabled,
    AutoEnterDisabled,

    VoiceActivityDetected,
    VoiceActivityEnded,

    TranscriptionFiltered {
        reason: String,
    },
    CorrectionLogged {
        wrong: String,
        right: String,
    },
    CorrectionPending {
        id: String,
        wrong: String,
        right: String,
    },
    CorrectionCaptureResult {
        outcome: String,
    },
    Heartbeat,

    AutoEnterCountdownStarted {
        remaining_ms: u32,
        total_ms: u32,
    },
    AutoEnterCountdownTick {
        remaining_ms: u32,
    },
    AutoEnterCountdownCancelled,
    AutoEnterCountdownFinished,

    IdleAutoPaused {
        seconds: u32,
    },
    IdleAutoResumed,

    FocusedAppChanged {
        bundle_id: Option<String>,
    },
    MasterPauseChanged {
        master_paused: bool,
    },
    PauseScopeToggled {
        scope: String,
        bundle_id: Option<String>,
        paused: bool,
    },
    LongRecordingWarning {
        elapsed_secs: u32,
        cap_secs: u32,
    },
    PauseSourceChanged {
        source: String,
        paused: bool,
        detail: Option<String>,
    },
    ResumedAppsChanged {
        bundles: Vec<String>,
    },
    CorrectionDialogRequested {
        last_transcript: String,
    },

    ModelsList {
        models: Vec<ModelInfo>,
    },
    ModelDownloadProgress {
        model_id: String,
        downloaded: u64,
        total: u64,
        percentage: f64,
    },
    ModelDownloadComplete {
        model_id: String,
    },
    ModelDownloadCancelled {
        model_id: String,
    },
    ModelDownloadFailed {
        model_id: String,
        error: String,
    },
    ModelVerificationStarted {
        model_id: String,
    },
    ModelVerificationCompleted {
        model_id: String,
    },
    ModelExtractionStarted {
        model_id: String,
    },
    ModelExtractionCompleted {
        model_id: String,
    },
    ModelExtractionFailed {
        model_id: String,
        error: String,
    },

    LowMicrophoneVolume {
        energy: f64,
    },
    ActiveTranscriberChanged {
        backend: String,
    },
    TranscriptionFailed {
        kind: String,
        message: String,
    },
    SttFallbackEngaged {
        model: String,
    },

    VoiceEnrollmentPreparing {
        step: String,
    },
    VoiceEnrollmentStarted {
        step: String,
    },
    VoiceEnrollmentLevel {
        energy: f64,
        voiced_ms: u32,
        target_ms: u32,
    },
    VoiceEnrollmentSampleCaptured {
        step: String,
    },
    VoiceEnrollmentFailed {
        step: String,
        message: String,
    },
    VoiceProfileStatus {
        enrolled: bool,
        enabled: bool,
        steps: Vec<String>,
    },
}

impl DaemonEvent {
    pub fn from_json_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

/// Model information returned in the `ModelsList` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub is_downloaded: bool,
    pub is_downloading: bool,
    pub partial_size: u64,
    pub backend: String,
}

/// Commands the GUI can send **to the daemon**.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DaemonCommand {
    TogglePause,
    ToggleAutoEnter,
    SetAutoEnter {
        enabled: bool,
    },
    SetConsumeMode {
        enabled: bool,
    },
    ApplyRuntimePreferences {
        auto_enter_delay_ms: u32,
        energy_threshold: f64,
        silence_secs: f64,
        cooldown_ms: u32,
        silero_threshold: f32,
        #[serde(default)]
        adaptive_silence: Option<bool>,
        #[serde(default)]
        audible_status_sound: Option<String>,
        #[serde(default)]
        stt_live_preview: Option<bool>,
    },
    ApproveCorrection {
        id: String,
    },
    RejectCorrection {
        id: String,
    },
    CaptureCorrection,
    SetPaused {
        paused: bool,
        reason: Option<String>,
    },
    CancelAutoEnterCountdown,
    NotifyFocusedAppChanged {
        bundle_id: Option<String>,
    },
    NotifySystemAudioState {
        playing: bool,
    },
    LogCorrection {
        intended: String,
    },
    SetAppPaused {
        bundle_id: String,
        paused: Option<bool>,
    },
    ListModels,
    DownloadModel {
        model_id: String,
    },
    CancelModelDownload {
        model_id: String,
    },
    DeleteModel {
        model_id: String,
    },
    SetActiveTranscriber {
        backend: String,
    },
    SetLanguage {
        lang: String,
    },
    StartVoiceEnrollment {
        step: String,
    },
    CancelVoiceEnrollment,
    DeleteVoiceProfile,
    SetVoiceProfileEnabled {
        enabled: bool,
    },
    GetVoiceProfileStatus,
    RespawnRecorder,
    ReloadShortcuts,
}

// ── State snapshot ─────────────────────────────────────────────────────────

/// Current daemon state as known by the GUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DaemonState {
    #[default]
    Unknown,
    Listening,
    Paused,
    Processing,
}

impl DaemonState {
    /// Return a short label for the tray.
    pub fn label(&self) -> &'static str {
        match self {
            DaemonState::Unknown => "Unknown",
            DaemonState::Listening => "Listening",
            DaemonState::Paused => "Paused",
            DaemonState::Processing => "Processing",
        }
    }
}

/// Internal state accumulator kept in sync by event handling.
struct DaemonStateAccumulator {
    state: DaemonState,
    paused: bool,
    master_paused: bool,
    auto_enter: bool,
    active_backend: String,
}

impl Default for DaemonStateAccumulator {
    fn default() -> Self {
        Self {
            state: DaemonState::Unknown,
            paused: false,
            master_paused: false,
            auto_enter: false,
            active_backend: "groq".to_string(),
        }
    }
}

impl DaemonStateAccumulator {
    /// Apply an incoming event and return any derived state changes.
    fn apply(&mut self, event: &DaemonEvent) {
        match event {
            DaemonEvent::Hello { version } => {
                if *version != PROTOCOL_VERSION {
                    tracing::warn!(
                        expected = PROTOCOL_VERSION,
                        received = version,
                        "protocol version mismatch"
                    );
                }
            }
            DaemonEvent::ListeningStarted => self.state = DaemonState::Listening,
            DaemonEvent::ProcessingStarted => self.state = DaemonState::Processing,
            DaemonEvent::Paused => {
                self.paused = true;
                if !self.master_paused {
                    self.state = DaemonState::Paused;
                }
            }
            DaemonEvent::Resumed => {
                self.paused = false;
                if !self.master_paused && self.state == DaemonState::Paused {
                    self.state = DaemonState::Listening;
                }
            }
            DaemonEvent::ResumedQuietly => {
                self.paused = false;
                if !self.master_paused && self.state == DaemonState::Paused {
                    self.state = DaemonState::Listening;
                }
            }
            DaemonEvent::MasterPauseChanged { master_paused } => {
                self.master_paused = *master_paused;
                if *master_paused {
                    self.state = DaemonState::Paused;
                } else if !self.paused && self.state == DaemonState::Paused {
                    self.state = DaemonState::Listening;
                }
            }
            DaemonEvent::AutoEnterEnabled => self.auto_enter = true,
            DaemonEvent::AutoEnterDisabled => self.auto_enter = false,
            DaemonEvent::ActiveTranscriberChanged { backend } => {
                self.active_backend = backend.clone();
            }
            _ => {}
        }
    }
}

// ── Connection helpers ─────────────────────────────────────────────────────

/// Resolve the UDS socket path the daemon listens on.
///
/// Matches the daemon's [`socket_path`](always::always::uds_server::socket_path)
/// logic: `$XDG_RUNTIME_DIR/always.sock` on Linux,
/// `~/Library/Caches/Always/always{suffix}.sock` on macOS.
pub fn resolve_socket_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        std::env::var("HOME")
            .ok()
            .map(|home| PathBuf::from(home).join("Library/Caches/Always/always.sock"))
            .or_else(|| dirs::home_dir().map(|h| h.join("Library/Caches/Always/always.sock")))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(runtime_dir).join("always.sock")
        } else if let Some(cache) = dirs::cache_dir() {
            cache.join("always.sock")
        } else {
            PathBuf::from("/tmp/always.sock")
        }
    }
    #[cfg(windows)]
    {
        // Daemon's named pipe — the Windows equivalent of the UDS path.
        // `ALWAYS_INSTANCE=dev` mirrors the daemon's `-dev` suffix.
        let suffix = std::env::var("ALWAYS_INSTANCE")
            .ok()
            .filter(|i| i != "prod" && !i.is_empty())
            .map(|i| format!("-{i}"))
            .unwrap_or_default();
        PathBuf::from(format!(r"\\.\pipe\always{suffix}"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        PathBuf::new()
    }
}

/// Platform stream for daemon IPC.
#[cfg(unix)]
type DaemonStream = UnixStream;
#[cfg(windows)]
type DaemonStream = NamedPipeClient;
/// Placeholder so the `cfg(not(any(unix, windows)))` arms type-check —
/// `connect_to_socket` bails before it is ever used.
#[cfg(not(any(unix, windows)))]
type DaemonStream = tokio::io::DuplexStream;

/// Split a connected stream into owned read/write halves.
#[cfg(unix)]
fn split_stream(stream: DaemonStream) -> (impl AsyncRead + Unpin, impl AsyncWrite + Unpin) {
    stream.into_split()
}

#[cfg(windows)]
fn split_stream(stream: DaemonStream) -> (impl AsyncRead + Unpin, impl AsyncWrite + Unpin) {
    tokio::io::split(stream)
}

#[cfg(not(any(unix, windows)))]
fn split_stream(stream: DaemonStream) -> (impl AsyncRead + Unpin, impl AsyncWrite + Unpin) {
    tokio::io::split(stream)
}

/// Connect to the daemon socket.
#[cfg(unix)]
async fn connect_to_socket(path: &std::path::Path) -> Result<DaemonStream> {
    use tokio::time::timeout;

    timeout(Duration::from_secs(5), UnixStream::connect(path))
        .await
        .map_err(|_| anyhow::anyhow!("connect timeout to {}", path.display()))?
        .context("failed to connect to daemon UDS socket")
}

/// Connect to the daemon's named pipe (Windows UDS equivalent).
#[cfg(windows)]
async fn connect_to_socket(path: &std::path::Path) -> Result<DaemonStream> {
    use tokio::time::timeout;

    timeout(Duration::from_secs(5), async {
        tokio::net::windows::named_pipe::ClientOptions::new().open(path)
    })
    .await
    .map_err(|_| anyhow::anyhow!("connect timeout to {}", path.display()))?
    .context("failed to connect to daemon named pipe")
}

#[cfg(not(any(unix, windows)))]
async fn connect_to_socket(_: &std::path::Path) -> Result<DaemonStream> {
    bail!("daemon IPC is not supported on this platform")
}

// ── UdsClient ─────────────────────────────────────────────────────────────

/// Handles a persistent connection to the Always daemon.
///
/// Spawns a background task that:
/// 1. Connects (with exponential backoff on failure).
/// 2. Sends commands when [`Self::send_command`] is called.
/// 3. Reads events and dispatches them to the callback.
///
/// The reconnection loop runs indefinitely until [`Self::disconnect`] is called.
/// What the client reports to its consumer: a raw daemon event line or a
/// connection-state change. Raw lines are forwarded verbatim so the GUI
/// sees every `DaemonEvent` variant — including ones this crate's enum
/// doesn't know yet.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    Event(String),
    Connected,
    Disconnected,
}

pub struct UdsClient {
    sender: Mutex<Option<mpsc::Sender<String>>>,
    state: Arc<Mutex<DaemonStateAccumulator>>,
    #[allow(clippy::type_complexity)]
    event_callbacks: Arc<Mutex<Vec<Box<dyn Fn(ClientEvent) + Send + Sync>>>>,
    reconnect_handle: tauri::async_runtime::JoinHandle<()>,
}

// Safety: `UdsClient` is Send because all shared state is behind Arc<Mutex<>>.
unsafe impl Send for UdsClient {}

impl UdsClient {
    /// Create a new client that will auto-connect in a background task.
    ///
    /// * `socket_path` — path to the daemon's UDS socket.
    /// * `on_event`   — callback invoked for every event the daemon sends.
    ///   Pass `None` to discard events.
    pub fn new<F>(socket_path: PathBuf, on_event: Option<F>) -> Self
    where
        F: Fn(ClientEvent) + Send + Sync + 'static,
    {
        let state = Arc::new(Mutex::new(DaemonStateAccumulator::default()));
        let event_callback = Arc::new(Mutex::new(
            on_event
                .map(|cb| vec![Box::new(cb) as Box<dyn Fn(ClientEvent) + Send + Sync>])
                .unwrap_or_default(),
        ));

        let (tx, mut rx) = mpsc::channel::<String>(64);
        let state_clone = Arc::clone(&state);
        let event_cb_clone = Arc::clone(&event_callback);

        // tauri::async_runtime, not tokio::spawn — `new` is called from
        // Tauri's `setup` on the main thread, outside a Tokio context.
        let reconnect_handle = tauri::async_runtime::spawn(async move {
            let mut retry_delay = Duration::from_secs(1);
            let mut reconnecting = false;
            let socket_path = Arc::new(socket_path);

            loop {
                match connect_to_socket(&socket_path).await {
                    Ok(mut stream) => {
                        tracing::info!(path = %socket_path.display(), "uds_connected");
                        retry_delay = Duration::from_secs(1); // reset on success
                        reconnecting = false;
                        {
                            let cb = event_cb_clone.lock().await;
                            for callback in cb.iter() {
                                callback(ClientEvent::Connected);
                            }
                        }

                        // Drain any buffered commands accumulated while disconnected.
                        while let Ok(cmd) = rx.try_recv() {
                            let _ = Self::write_one(&mut stream, &cmd).await;
                        }

                        let (reader, mut writer) = split_stream(stream);
                        let reader = BufReader::new(reader);
                        if Self::read_loop(
                            reader,
                            &mut rx,
                            &mut writer,
                            &state_clone,
                            &event_cb_clone,
                        )
                        .await
                        {
                            tracing::info!("uds_reader_exit: clean disconnect");
                        } else {
                            tracing::warn!("uds_reader_exit: connection lost");
                        }
                        {
                            let cb = event_cb_clone.lock().await;
                            for callback in cb.iter() {
                                callback(ClientEvent::Disconnected);
                            }
                        }
                        // `writer` is dropped here.
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, path = %socket_path.display(), "uds_connect_failed");
                        reconnecting = true;

                        // Give the reader loop a chance to finish.
                        tokio::time::sleep(Duration::from_millis(100)).await;

                        // Backoff: try to grab next available command from channel,
                        // but if channel is empty wait up to retry_delay.
                        let sleep = tokio::time::sleep(retry_delay);
                        tokio::pin!(sleep);

                        loop {
                            tokio::select! {
                                biased;
                                _ = &mut sleep => {
                                    break;
                                }
                                _ = rx.recv() => {
                                    // Commands received while disconnected — reconnect
                                    // immediately so they don't sit in the buffer.
                                    sleep.as_mut().reset(tokio::time::Instant::now());
                                }
                            }
                        }

                        retry_delay = std::cmp::min(retry_delay * 2, Duration::from_secs(30));
                    }
                }
            }
        });

        Self {
            sender: Mutex::new(Some(tx)),
            state,
            event_callbacks: event_callback,
            reconnect_handle,
        }
    }

    /// Send a command to the daemon. Queues it on the current connection,
    /// reconnecting first if necessary.
    pub async fn send_command(&self, cmd: DaemonCommand) -> Result<()> {
        let line = serde_json::to_string(&cmd).context("serialize command")?;
        self.send_line(line).await
    }

    /// Send a raw JSON command line verbatim — the caller decides the
    /// command shape, so new daemon commands never need an enum update.
    pub async fn send_line(&self, line: String) -> Result<()> {
        let sender = {
            let guard = self.sender.lock().await;
            guard.clone()
        };
        if let Some(tx) = sender {
            tx.send(line).await.context("command channel closed")
        } else {
            bail!("client is disconnected")
        }
    }

    /// Register (or replace) an event callback.
    pub fn on_event<F>(&self, callback: F)
    where
        F: Fn(ClientEvent) + Send + Sync + 'static,
    {
        let mut cb = self.event_callbacks.blocking_lock();
        cb.push(Box::new(callback));
    }

    /// Read the current daemon state snapshot.
    pub async fn get_state(&self) -> DaemonState {
        self.state.lock().await.state
    }

    /// Check if auto-enter is currently enabled.
    pub async fn is_auto_enter_enabled(&self) -> bool {
        self.state.lock().await.auto_enter
    }

    /// Whether the daemon IPC channel is currently connected.
    pub async fn is_connected(&self) -> bool {
        self.sender.lock().await.is_some()
    }

    /// Check if the daemon is currently paused.
    pub async fn is_paused(&self) -> bool {
        let s = self.state.lock().await;
        s.paused || s.master_paused
    }

    /// Get the active transcriber backend string.
    pub async fn active_backend(&self) -> String {
        self.state.lock().await.active_backend.clone()
    }

    /// Gracefully stop the background reconnection task.
    pub async fn disconnect(&self) {
        {
            let mut guard = self.sender.lock().await;
            *guard = None;
        }
        self.reconnect_handle.abort();
    }

    // ── internal helpers ───────────────────────────────────────────────

    async fn read_loop<R, W>(
        mut reader: BufReader<R>,
        rx: &mut mpsc::Receiver<String>,
        writer: &mut W,
        state: &Arc<Mutex<DaemonStateAccumulator>>,
        event_cb: &Arc<Mutex<Vec<Box<dyn Fn(ClientEvent) + Send + Sync>>>>,
    ) -> bool
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        // Drain initial-state burst into a buffer to avoid holding the
        // read lock for every single line.
        let mut buf = String::new();
        let mut state = state.lock().await;

        loop {
            buf.clear();
            tokio::select! {
                read = reader.read_line(&mut buf) => {
                    match read {
                        Ok(0) => return false, // EOF
                        Ok(_) => {
                            let line = buf.trim().to_string();
                            if line.is_empty() {
                                continue;
                            }
                            // Forward the raw line — the frontend parses JSON and
                            // understands variants this crate's enum doesn't.
                            {
                                let cb = event_cb.lock().await;
                                for callback in cb.iter() {
                                    callback(ClientEvent::Event(line.clone()));
                                }
                            }
                            if let Ok(event) = serde_json::from_str::<DaemonEvent>(&line) {
                                state.apply(&event);
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "uds_read_error");
                            return false;
                        }
                    }
                }
                cmd = rx.recv() => {
                    match cmd {
                        Some(cmd) => {
                            if let Err(e) = Self::write_one(writer, &cmd).await {
                                tracing::warn!(error = %e, "uds_write_error");
                                return false; // broken pipe → reconnect
                            }
                        }
                        // All senders dropped (client disconnected) — leave the
                        // loop so the reconnect cycle can re-establish.
                        None => return false,
                    }
                }
            }
        }
    }

    async fn write_one<W>(writer: &mut W, cmd: &str) -> Result<()>
    where
        W: AsyncWrite + Unpin,
    {
        let json = cmd.trim_end().to_string();
        writer
            .write_all(format!("{json}\n").as_bytes())
            .await
            .context("write command to socket")?;
        writer.flush().await.context("flush command to socket")?;
        Ok(())
    }
}

impl Drop for UdsClient {
    fn drop(&mut self) {
        self.reconnect_handle.abort();
    }
}

// ── Testing ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_pause_command_serialises() {
        let cmd = DaemonCommand::TogglePause;
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"TogglePause\""));
        let parsed: DaemonCommand = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, DaemonCommand::TogglePause));
    }

    #[test]
    fn hello_event_parses() {
        let json = r#"{"type":"Hello","data":{"version":12}}"#;
        let event = DaemonEvent::from_json_line(json).unwrap();
        assert!(matches!(event, DaemonEvent::Hello { version: 12 }));
    }

    #[test]
    fn paused_event_parses() {
        let json = r#"{"type":"Paused","data":null}"#;
        let event = DaemonEvent::from_json_line(json).unwrap();
        assert!(matches!(event, DaemonEvent::Paused));
    }

    #[test]
    fn resumed_event_parses() {
        let json = r#"{"type":"Resumed","data":null}"#;
        let event = DaemonEvent::from_json_line(json).unwrap();
        assert!(matches!(event, DaemonEvent::Resumed));
    }

    #[test]
    fn toggle_auto_enter_command_serialises() {
        let cmd = DaemonCommand::ToggleAutoEnter;
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"ToggleAutoEnter\""));
    }

    #[test]
    fn set_auto_enter_command_serialises() {
        let cmd = DaemonCommand::SetAutoEnter { enabled: true };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"SetAutoEnter\""));
        assert!(json.contains("\"enabled\""));
    }

    #[test]
    fn state_accumulator_tracks_pause_resume() {
        let mut acc = DaemonStateAccumulator::default();
        assert_eq!(acc.state, DaemonState::Unknown);

        acc.apply(&DaemonEvent::ListeningStarted);
        assert_eq!(acc.state, DaemonState::Listening);

        acc.apply(&DaemonEvent::Paused);
        assert_eq!(acc.state, DaemonState::Paused);

        acc.apply(&DaemonEvent::Resumed);
        assert_eq!(acc.state, DaemonState::Listening);

        acc.apply(&DaemonEvent::MasterPauseChanged {
            master_paused: true,
        });
        assert_eq!(acc.state, DaemonState::Paused);

        acc.apply(&DaemonEvent::MasterPauseChanged {
            master_paused: false,
        });
        assert_eq!(acc.state, DaemonState::Listening);
    }

    #[test]
    fn state_accumulator_tracks_auto_enter() {
        let mut acc = DaemonStateAccumulator::default();
        assert!(!acc.auto_enter);
        acc.apply(&DaemonEvent::AutoEnterEnabled);
        assert!(acc.auto_enter);
        acc.apply(&DaemonEvent::AutoEnterDisabled);
        assert!(!acc.auto_enter);
    }
}
