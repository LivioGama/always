//! Thin client for the Always daemon over Unix Domain Socket.
//!
//! Connects to the platform-specific socket path via
//! [`dirs::cache_dir()`]. Uses the JSON-over-lines protocol v12 that the
//! daemon (`always` crate) speaks — each line is a standalone JSON object
//! with `{"type":"…","data":…}` structure.
//!
//! Reconnects with exponential backoff (1 s .. 30 s max) so transient
//! daemon restarts or cold-start pauses do not tear down the GUI.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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
    Hello { version: u32 },

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
    TranscriptChunk { text: String },
    TranscriptFinal { text: String },
    TranscriptionInterim { text: String },
    GrammarCorrected { before: String, after: String },

    Paused,
    Resumed,
    PausedQuietly,
    ResumedQuietly,

    AutoEnterEnabled,
    AutoEnterDisabled,

    VoiceActivityDetected,
    VoiceActivityEnded,

    TranscriptionFiltered { reason: String },
    CorrectionLogged { wrong: String, right: String },
    CorrectionPending { id: String, wrong: String, right: String },
    CorrectionCaptureResult { outcome: String },
    Heartbeat,

    AutoEnterCountdownStarted { remaining_ms: u32, total_ms: u32 },
    AutoEnterCountdownTick { remaining_ms: u32 },
    AutoEnterCountdownCancelled,
    AutoEnterCountdownFinished,

    IdleAutoPaused { seconds: u32 },
    IdleAutoResumed,

    FocusedAppChanged { bundle_id: Option<String> },
    MasterPauseChanged { master_paused: bool },
    PauseScopeToggled { scope: String, bundle_id: Option<String>, paused: bool },
    LongRecordingWarning { elapsed_secs: u32, cap_secs: u32 },
    PauseSourceChanged { source: String, paused: bool, detail: Option<String> },
    ResumedAppsChanged { bundles: Vec<String> },
    CorrectionDialogRequested { last_transcript: String },

    ModelsList { models: Vec<ModelInfo> },
    ModelDownloadProgress { model_id: String, downloaded: u64, total: u64, percentage: f64 },
    ModelDownloadComplete { model_id: String },
    ModelDownloadCancelled { model_id: String },
    ModelDownloadFailed { model_id: String, error: String },
    ModelVerificationStarted { model_id: String },
    ModelVerificationCompleted { model_id: String },
    ModelExtractionStarted { model_id: String },
    ModelExtractionCompleted { model_id: String },
    ModelExtractionFailed { model_id: String, error: String },

    LowMicrophoneVolume { energy: f64 },
    ActiveTranscriberChanged { backend: String },
    TranscriptionFailed { kind: String, message: String },
    SttFallbackEngaged { model: String },

    VoiceEnrollmentStarted { step: String },
    VoiceEnrollmentLevel { energy: f64, voiced_ms: u32, target_ms: u32 },
    VoiceEnrollmentSampleCaptured { step: String },
    VoiceEnrollmentFailed { step: String, message: String },
    VoiceProfileStatus { enrolled: bool, enabled: bool, steps: Vec<String> },
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
    SetAutoEnter { enabled: bool },
    SetConsumeMode { enabled: bool },
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
    ApproveCorrection { id: String },
    RejectCorrection { id: String },
    CaptureCorrection,
    SetPaused { paused: bool, reason: Option<String> },
    CancelAutoEnterCountdown,
    NotifyFocusedAppChanged { bundle_id: Option<String> },
    NotifySystemAudioState { playing: bool },
    LogCorrection { intended: String },
    SetAppPaused { bundle_id: String, paused: Option<bool> },
    ListModels,
    DownloadModel { model_id: String },
    CancelModelDownload { model_id: String },
    DeleteModel { model_id: String },
    SetActiveTranscriber { backend: String },
    SetLanguage { lang: String },
    StartVoiceEnrollment { step: String },
    CancelVoiceEnrollment,
    DeleteVoiceProfile,
    SetVoiceProfileEnabled { enabled: bool },
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
    #[cfg(not(target_os = "macos"))]
    {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(runtime_dir).join("always.sock")
        } else if let Some(cache) = dirs::cache_dir() {
            cache.join("always.sock")
        } else {
            PathBuf::from("/tmp/always.sock")
        }
    }
}

/// Connect to the daemon socket.
#[cfg(unix)]
async fn connect_to_socket(path: &std::path::Path) -> Result<UnixStream> {
    use tokio::time::timeout;

    timeout(Duration::from_secs(5), UnixStream::connect(path))
        .await
        .map_err(|_| anyhow::anyhow!("connect timeout to {}", path.display()))?
        .context("failed to connect to daemon UDS socket")
}

#[cfg(not(unix))]
async fn connect_to_socket(_: &std::path::Path) -> Result<UnixStream> {
    bail!("Unix domain sockets are not supported on this platform")
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
pub struct UdsClient {
    sender: Mutex<Option<mpsc::Sender<DaemonCommand>>>,
    state: Arc<Mutex<DaemonStateAccumulator>>,
    #[allow(clippy::type_complexity)]
    event_callback: Arc<Mutex<Option<Box<dyn Fn(DaemonEvent) + Send + Sync>>>>,
    reconnect_handle: tokio::task::JoinHandle<()>,
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
        F: Fn(DaemonEvent) + Send + Sync + 'static,
    {
        let state = Arc::new(Mutex::new(DaemonStateAccumulator::default()));
        let event_callback = Arc::new(Mutex::new(on_event.map(|cb| Box::new(cb) as Box<dyn Fn(DaemonEvent) + Send + Sync>)));

        let (tx, mut rx) = mpsc::channel::<DaemonCommand>(64);
        let state_clone = Arc::clone(&state);
        let event_cb_clone = Arc::clone(&event_callback);

        let reconnect_handle = tokio::spawn(async move {
            let mut retry_delay = Duration::from_secs(1);
            let mut reconnecting = false;
            let socket_path = Arc::new(socket_path);

            loop {
                match connect_to_socket(&socket_path).await {
                    Ok(stream) => {
                        tracing::info!(path = %socket_path.display(), "uds_connected");
                        retry_delay = Duration::from_secs(1); // reset on success
                        reconnecting = false;

                        // Drain any buffered commands accumulated while disconnected.
                        while let Ok(cmd) = rx.try_recv() {
                            let _ = Self::write_one(&stream, &cmd).await;
                        }

                        let (reader, mut writer) = stream.into_split();
                        let reader = BufReader::new(reader);
                        if Self::read_loop(reader, &mut rx, &writer, &state_clone, &event_cb_clone).await {
                            tracing::info!("uds_reader_exit: clean disconnect");
                        } else {
                            tracing::warn!("uds_reader_exit: connection lost");
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
            event_callback,
            reconnect_handle,
        }
    }

    /// Send a command to the daemon. Queues it on the current connection,
    /// reconnecting first if necessary.
    pub async fn send_command(&self, cmd: DaemonCommand) -> Result<()> {
        let sender = {
            let guard = self.sender.lock().await;
            guard.clone()
        };
        if let Some(tx) = sender {
            tx.send(cmd).await.context("command channel closed")
        } else {
            bail!("client is disconnected")
        }
    }

    /// Register (or replace) an event callback.
    pub fn on_event<F>(&self, callback: F)
    where
        F: Fn(DaemonEvent) + Send + Sync + 'static,
    {
        let mut cb = self.event_callback.blocking_lock();
        *cb = Some(Box::new(callback));
    }

    /// Read the current daemon state snapshot.
    pub async fn get_state(&self) -> DaemonState {
        self.state.lock().await.state
    }

    /// Check if auto-enter is currently enabled.
    pub async fn is_auto_enter_enabled(&self) -> bool {
        self.state.lock().await.auto_enter
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
        let _ = self.reconnect_handle.await;
    }

    // ── internal helpers ───────────────────────────────────────────────

    async fn read_loop(
        mut reader: BufReader<tokio::net::unix::ReadHalf<'static>>,
        rx: &mut mpsc::Receiver<DaemonCommand>,
        writer: &tokio::net::unix::WriteHalf<'static>,
        state: &Arc<Mutex<DaemonStateAccumulator>>,
        event_cb: &Arc<Mutex<Option<Box<dyn Fn(DaemonEvent) + Send + Sync>>>>,
    ) -> bool {
        // Drain initial-state burst into a buffer to avoid holding the
        // read lock for every single line.
        let mut buf = String::new();
        let mut state = state.lock().await;

        loop {
            buf.clear();
            match reader.read_line(&mut buf).await {
                Ok(0) => return false, // EOF
                Ok(_) => {
                    let line = buf.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(event) = DaemonEvent::from_json_line(&line) {
                        state.apply(&event);
                        let cb = event_cb.lock().await;
                        if let Some(callback) = cb.as_ref() {
                            callback(event.clone());
                        }
                        drop(cb);
                    } else {
                        tracing::warn!(line = %line, "uds_parse_event_failed");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "uds_read_error");
                    return false;
                }
            }
        }
    }

    async fn write_one(
        writer: &tokio::net::unix::WriteHalf<'static>,
        cmd: &DaemonCommand,
    ) -> Result<()> {
        let json = serde_json::to_string(cmd).context("serialize command")?;
        let mut writer = writer.clone();
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

        acc.apply(&DaemonEvent::MasterPauseChanged { master_paused: true });
        assert_eq!(acc.state, DaemonState::Paused);

        acc.apply(&DaemonEvent::MasterPauseChanged { master_paused: false });
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