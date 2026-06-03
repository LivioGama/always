use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Event types for daemon-to-GUI communication
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DaemonEvent {
    /// Daemon has started listening for voice input
    ListeningStarted,
    /// Daemon has stopped listening
    ListeningStopped,
    /// Daemon is processing audio (VAD detected speech)
    ProcessingStarted,
    /// Daemon has finished processing audio
    ProcessingStopped,
    /// Transcription has started
    TranscribingStarted,
    /// Transcription has stopped
    TranscribingStopped,
    /// Partial transcript (streaming update)
    TranscriptChunk { text: String },
    /// Final transcript result
    TranscriptFinal { text: String },
    /// Daemon is paused
    Paused,
    /// Daemon is resumed
    Resumed,
    /// Auto-enter mode enabled
    AutoEnterEnabled,
    /// Auto-enter mode disabled
    AutoEnterDisabled,
    /// Voice activity detected (early energy detection)
    VoiceActivityDetected,
    /// Voice activity ended
    VoiceActivityEnded,
    /// Heartbeat for connection health
    Heartbeat,
}

impl DaemonEvent {
    /// Convert event to JSON line
    pub fn to_json_line(&self) -> Result<String, serde_json::Error> {
        let json = serde_json::to_string(self)?;
        Ok(json + "\n")
    }

    /// Parse event from JSON line
    pub fn from_json_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

/// Commands that connected clients (the Mac app) can send to the daemon
/// over the UDS socket. Executing them inside the daemon process is what
/// allows the resulting events to reach all subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonCommand {
    TogglePause,
    ToggleAutoEnter,
}

impl DaemonCommand {
    pub fn from_json_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

/// Event broadcaster for sending events to connected clients
#[derive(Clone)]
pub struct EventBroadcaster {
    tx: broadcast::Sender<DaemonEvent>,
}

impl EventBroadcaster {
    /// Create a new event broadcaster
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(100);
        Self { tx }
    }

    /// Broadcast an event to all subscribers
    pub fn send(&self, event: DaemonEvent) {
        let _ = self.tx.send(event);
    }

    /// Subscribe to events
    pub fn subscribe(&self) -> broadcast::Receiver<DaemonEvent> {
        self.tx.subscribe()
    }

    /// Send listening started event
    pub fn listening_started(&self) {
        self.send(DaemonEvent::ListeningStarted);
    }

    /// Send listening stopped event
    pub fn listening_stopped(&self) {
        self.send(DaemonEvent::ListeningStopped);
    }

    /// Send processing started event
    pub fn processing_started(&self) {
        self.send(DaemonEvent::ProcessingStarted);
    }

    /// Send processing stopped event
    pub fn processing_stopped(&self) {
        self.send(DaemonEvent::ProcessingStopped);
    }

    /// Send transcribing started event
    pub fn transcribing_started(&self) {
        self.send(DaemonEvent::TranscribingStarted);
    }

    /// Send transcribing stopped event
    pub fn transcribing_stopped(&self) {
        self.send(DaemonEvent::TranscribingStopped);
    }

    /// Send transcript final event
    pub fn transcript_final(&self, text: String) {
        self.send(DaemonEvent::TranscriptFinal { text });
    }

    /// Send paused event
    pub fn paused(&self) {
        self.send(DaemonEvent::Paused);
    }

    /// Send resumed event
    pub fn resumed(&self) {
        self.send(DaemonEvent::Resumed);
    }

    /// Send auto-enter enabled event
    pub fn auto_enter_enabled(&self) {
        self.send(DaemonEvent::AutoEnterEnabled);
    }

    /// Send auto-enter disabled event
    pub fn auto_enter_disabled(&self) {
        self.send(DaemonEvent::AutoEnterDisabled);
    }

    /// Send voice activity detected event
    pub fn voice_activity_detected(&self) {
        self.send(DaemonEvent::VoiceActivityDetected);
    }

    /// Send voice activity ended event
    pub fn voice_activity_ended(&self) {
        self.send(DaemonEvent::VoiceActivityEnded);
    }
}

impl Default for EventBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

/// Global event broadcaster instance
static GLOBAL_BROADCASTER: once_cell::sync::Lazy<EventBroadcaster> =
    once_cell::sync::Lazy::new(EventBroadcaster::new);

/// Get the global event broadcaster
pub fn global_broadcaster() -> &'static EventBroadcaster {
    &GLOBAL_BROADCASTER
}
