use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Minimum gap between low-mic overlay hints so a quiet room doesn't
/// spam the HUD on every utterance.
const LOW_MIC_OVERLAY_COOLDOWN: Duration = Duration::from_secs(45);
static LAST_LOW_MIC_OVERLAY: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

/// Wire-format protocol version. Bump on any breaking change to
/// [`DaemonEvent`] or [`DaemonCommand`]. The daemon sends a `Hello` event
/// as the first frame of every UDS connection so GUI clients can refuse
/// to talk to a daemon they were not built against.
///
/// **v3 (2026-05-17):** Pause/Resume now mean "effective" pause (master
/// OR per-app rule). Added [`DaemonEvent::MasterPauseChanged`] and
/// [`DaemonEvent::ResumedAppsChanged`] so the UI can render the
/// allowlist + master kill switch separately.
///
/// **v4 (2026-05-19):** Local-model registry. New
/// [`DaemonCommand`] variants for the Settings → Models tab
/// (`ListModels`, `DownloadModel`, `CancelModelDownload`,
/// `DeleteModel`, `SetActiveTranscriber`) and matching
/// [`DaemonEvent`]s for catalog snapshot + download / verification /
/// extraction progress + active-backend changes.
///
/// **v5 (2026-05-20):** Live preference sync from Settings.
/// `SetAutoEnter` + `ApplyRuntimePreferences` so sensitivity and
/// auto-enter delay apply without a daemon restart.
///
/// **v6 (2026-05-25):** Low microphone volume warning event.
/// `LowMicrophoneVolume` notifies GUI when mic energy is barely above threshold.
///
/// **v9 (2026-06-13):** Pause-chord scope feedback. The pause hotkey is
/// now strictly per-app (master pause moved to its own chord) and the
/// daemon emits [`DaemonEvent::PauseScopeToggled`] so the GUI can flash
/// which scope was toggled.
///
/// **v10 (2026-07-10):** "My Voice" speaker verification. New
/// enrollment commands ([`DaemonCommand::StartVoiceEnrollment`] et al.)
/// and the `VoiceEnrollment*` / [`DaemonEvent::VoiceProfileStatus`]
/// event family driving the Settings → My Voice tab.
///
/// **v11 (2026-07-15):** Streaming transcription support. New
/// [`DaemonEvent::TranscriptionInterim`] and [`DaemonEvent::TranscriptionFinal`]
/// for streaming transcribers that emit progressive interim results
/// followed by final committed text. The existing `TranscriptChunk` /
/// `TranscriptFinal` remain for non-streaming chunk-based transcription.
pub const PROTOCOL_VERSION: u32 = 12;
pub const PEER_PAUSE_LEASE_CAPABILITY: &str = "peer-pause-lease-v1";

/// Unique identity of this compiled daemon artifact. Compared at startup to
/// the identity recorded by a running daemon; this identifies binaries, not
/// source files that have not yet been rebuilt.
pub const BUILD_ID: &str = env!("ALWAYS_BUILD_ID");

/// Why a recoverable transcript was not pasted (`TranscriptRejected.reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// The "My Voice" gate did not recognize the speaker.
    Speaker,
    /// Acoustic evidence: not the dictation language (Nepali to family…).
    ForeignSpeech,
    /// Always was paused (per-app, mic conflict, …) while it was spoken.
    Paused,
    /// A held final nobody answered for expired.
    Expired,
}

impl RejectReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Speaker => "speaker",
            Self::ForeignSpeech => "foreign_speech",
            Self::Paused => "paused",
            Self::Expired => "expired",
        }
    }

    /// Recovery prompt copy for this reason.
    pub fn prompt(self) -> &'static str {
        match self {
            Self::Speaker => "Not sure that was you — paste anyway?",
            Self::ForeignSpeech => "Didn't sound like English — paste anyway?",
            Self::Paused => "Heard while Always was paused — paste anyway?",
            Self::Expired => "Not pasted in time — paste anyway?",
        }
    }
}

/// Event types for daemon-to-GUI communication
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DaemonEvent {
    /// Sent as the very first frame after a client connects. Carries the
    /// daemon's protocol version. The Mac app rejects the connection if
    /// the version is not the one it was built with.
    Hello {
        version: u32,
        /// Optional for compatibility with clients speaking to older daemons.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        build_id: Option<String>,
        /// Optional feature negotiation. Missing on older v12 daemons.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
    },
    /// Correlated result for the cross-instance pause lease command.
    PeerPauseLeaseResult {
        request_id: String,
        accepted: bool,
        lease_owned: bool,
    },
    /// Ground truth about the daemon's global-shortcut event tap. The
    /// GUI's own IOHIDCheckAccess reflects the GUI's TCC attribution —
    /// this event reports whether the DAEMON's listen-only CGEventTap is
    /// actually authorized, so the banner can say "shortcuts inactive"
    /// instead of leaving hotkeys silently dead. Included in every
    /// client's initial state burst and re-emitted on change.
    ShortcutListenerStatus {
        input_monitoring_granted: bool,
    },
    /// The microphone capture supervisor gave up on the recorder after
    /// repeated read timeouts / EOFs (`unavailable: true`), or audio
    /// resumed (`false`). Lets the GUI offer a "Mic stuck — Fix" action
    /// instead of leaving dictation silently dead. Included in every
    /// client's initial state burst while the mic is stuck.
    MicUnavailable {
        unavailable: bool,
    },
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
    TranscriptChunk {
        text: String,
    },
    /// Final transcript result (non-streaming / chunk-based)
    TranscriptFinal {
        text: String,
    },
    /// Consume mode only: the final just broadcast as `TranscriptFinal` was
    /// NOT pasted — its native paste job is parked under `id` until the
    /// consumer answers `ReleaseHeld` (paste it, with auto-enter, exactly as
    /// an unheld utterance) or `DiscardHeld` (it was claimed). Strings only,
    /// so the GUI's loose `[String: String]` payload decode accepts it.
    /// `at_us` is the daemon's wall clock (unix µs) at park time, for
    /// cross-process latency measurement.
    TranscriptHeld {
        id: String,
        text: String,
        at_us: String,
    },
    /// A voice-edit command ran ("scratch that", "change X to Y", …) or
    /// answered an Iris `VoiceEdit*` request (`voice_edit.rs`). Strings
    /// only, like `TranscriptHeld`, so the GUI's loose decode accepts it.
    /// `req` echoes the request id (empty for dictation-path edits); `ok`
    /// is "true"/"false"; `detail` is the decline reason or target source;
    /// `text` carries the edit target for `VoiceEditGetTarget`.
    VoiceEdit {
        #[serde(default)]
        req: String,
        op: String,
        ok: String,
        #[serde(default)]
        method: String,
        #[serde(default)]
        ms: String,
        #[serde(default)]
        detail: String,
        #[serde(default)]
        text: String,
    },
    /// Interim transcription result from a streaming transcriber.
    /// This text is provisional and may change as more audio is processed.
    /// Used by streaming engines (e.g., MoonshineStreaming, streaming APIs)
    /// to provide progressive updates to the UI before finalization.
    /// Distinct from `TranscriptChunk` which is used for chunk-based previews.
    TranscriptionInterim {
        text: String,
    },
    /// Live preview text heard BEFORE the "My Voice" gate verified the
    /// speaker (decision 2026-10-14: show immediately, dimmed). Only the
    /// GUI renders it; it is deliberately a distinct event type so no other
    /// consumer (Iris) ever routes unverified words. Once verified, the
    /// same text is re-sent as `TranscriptionInterim`.
    TranscriptionInterimPending {
        text: String,
    },
    /// Speaking-loudness meter (~20 Hz). Sent ONLY for voiced frames of
    /// the user's own speech: VAD says voice AND the speaker is verified
    /// (or My Voice is off) — never for room noise, fans, or media that
    /// the gate has not accepted. One terminal frame with `voiced: false`
    /// (energy 0) when the voice ends.
    /// - `energy`: RMS of the mic audio since the last tick, normalised to
    ///   full scale (0..1).
    /// - `energy_db`: the same RMS in dBFS (−100 floor for silence).
    /// - `bands`: coarse 5-band spectrum (≈80-250, 250-500, 500-1k, 1k-2k,
    ///   2k-4k Hz), each band's RMS share, summing to ≈1.
    VoiceLevel {
        energy: f64,
        energy_db: f64,
        voiced: bool,
        bands: [f32; 5],
    },
    /// "My Voice" rejected an utterance — it was NOT pasted — but its
    /// transcript is kept recoverable for a while (`rejected.rs`). A
    /// consumer may offer "paste anyway?" and answer `PasteRejected`.
    /// Strings only, like `TranscriptHeld`.
    TranscriptRejected {
        id: String,
        text: String,
        score: String,
        /// Why it was not pasted — see [`RejectReason`]. Consumers pick
        /// copy from this (or show `prompt` verbatim).
        #[serde(default)]
        reason: String,
        /// Ready-to-show recovery copy for this reason.
        #[serde(default)]
        prompt: String,
    },
    /// Grammar correction was applied — carries before/after for overlay feedback
    GrammarCorrected {
        before: String,
        after: String,
    },
    /// Daemon is paused
    Paused,
    /// Daemon is resumed
    Resumed,
    /// Daemon is paused as a side-effect of focus change (per-app rule).
    /// Functionally identical to `Paused` for state tracking, but the GUI
    /// MUST NOT flash the overlay — focus changes that the user initiated
    /// with a mouse / window switcher should not advertise themselves.
    PausedQuietly,
    /// Daemon is resumed as a side-effect of focus change (per-app rule).
    /// See `PausedQuietly` — no overlay flash.
    ResumedQuietly,
    /// Auto-enter mode enabled
    AutoEnterEnabled,
    /// Auto-enter mode disabled
    AutoEnterDisabled,
    /// Voice activity detected (early energy detection)
    VoiceActivityDetected,
    /// Voice activity ended
    VoiceActivityEnded,
    /// Transcription was rejected by the filter or hallucination detector.
    /// Carries a short, human-readable reason so the GUI can display it.
    TranscriptionFiltered {
        reason: String,
    },
    /// A `(wrong → right)` correction pair was just applied to
    /// `~/.always/glossary.json` (typically by the user pressing the
    /// correction-capture hotkey or approving a queued candidate).
    /// The GUI uses this to flash a brief toast.
    CorrectionLogged {
        wrong: String,
        right: String,
    },
    /// A passive clipboard re-copy looked like a correction but was
    /// not auto-applied — it sits in the pending-corrections queue
    /// awaiting user approval. Carries the queue entry's UUID as a
    /// string so the GUI can later send `ApproveCorrection`/`RejectCorrection`.
    CorrectionPending {
        id: String,
        wrong: String,
        right: String,
    },
    /// Outcome of an active manual-correction capture (⌃⌥X). Lets
    /// the GUI flash a status-bar overlay confirming what happened —
    /// `applied`, `no_recent_paste`, `no_change`, `no_correction_pairs`,
    /// or `error`. Per-pair detail flows separately via
    /// [`DaemonEvent::CorrectionLogged`].
    CorrectionCaptureResult {
        outcome: String,
    },
    /// Heartbeat for connection health
    Heartbeat,
    /// Auto-enter delay started — countdown overlay is now active.
    /// `total_ms` is the full delay; `remaining_ms == total_ms` on start.
    AutoEnterCountdownStarted {
        remaining_ms: u32,
        total_ms: u32,
    },
    /// Tick of the auto-enter countdown.
    AutoEnterCountdownTick {
        remaining_ms: u32,
    },
    /// Auto-enter countdown cancelled by user (key press) or override.
    AutoEnterCountdownCancelled,
    /// Auto-enter countdown reached zero — Return was just synthesized.
    AutoEnterCountdownFinished,
    /// Daemon auto-paused after going `seconds` with no voice activity.
    IdleAutoPaused {
        seconds: u32,
    },
    /// Daemon auto-resumed after the idle-pause condition cleared.
    IdleAutoResumed,
    /// Focused application changed (macOS only).
    FocusedAppChanged {
        bundle_id: Option<String>,
    },
    /// Master pause flag flipped — the user (or the audio/idle/mic
    /// watchdogs) explicitly toggled the global force-pause switch.
    /// `Paused`/`Resumed` continue to track *effective* state; this
    /// event is what the UI uses to label the global pause toggle
    /// ("Pause globally" vs "Resume globally").
    MasterPauseChanged {
        master_paused: bool,
    },
    /// A pause hotkey fired and the daemon resolved it to a concrete
    /// scope. Lets the GUI flash exactly what was toggled ("Resumed in
    /// Safari" / "Paused everywhere") so the chord never feels like it
    /// did something random. `scope` is `"master"` or `"app"`;
    /// `bundle_id` is set for app scope.
    PauseScopeToggled {
        scope: String,
        bundle_id: Option<String>,
        paused: bool,
    },
    /// Continuous speech crossed the warning threshold — the GUI flashes
    /// a heads-up that the recording hard-caps at `cap_secs`.
    LongRecordingWarning {
        elapsed_secs: u32,
        cap_secs: u32,
    },
    /// A watchdog pause source flipped. `source` is `"mic_conflict"` or
    /// `"audio_output"`; `detail` names the offending app when known
    /// ("Zoom"). Lets the UI say WHY it's paused instead of a generic
    /// badge. Distinct from `MasterPauseChanged`, which is user intent.
    PauseSourceChanged {
        source: String,
        paused: bool,
        detail: Option<String>,
    },
    /// Snapshot of the resumed-app allowlist (bundle ids whose
    /// `paused` override is set to `false`). Sent on connect and
    /// whenever a `SetAppPaused` command mutates the list.
    ResumedAppsChanged {
        bundles: Vec<String>,
    },
    /// Daemon wants the GUI to open the correction dialog. Carries the
    /// most recently-pasted transcript so the dialog can offer a
    /// best-guess match for the wrong word once the user types the
    /// intended one.
    CorrectionDialogRequested {
        last_transcript: String,
    },
    /// Snapshot of the local-model catalog (every entry with its
    /// current `is_downloaded` / `is_downloading` / `partial_size`
    /// fields). Broadcast in response to [`DaemonCommand::ListModels`]
    /// and whenever the catalog mutates so all connected clients see
    /// the same view.
    ModelsList {
        models: Vec<crate::managers::model_registry::ModelInfo>,
    },
    /// Streaming progress for an in-flight download. Throttled to
    /// ~10 events/sec at the registry layer.
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
    /// Microphone volume appears to be too low for reliable detection.
    /// The daemon is detecting voice but at very low energy levels.
    LowMicrophoneVolume {
        energy: f64,
    },
    /// Active STT backend changed (user picked a different model in
    /// Settings → Models, or deleted the currently-active one). The
    /// `backend` field is the canonical wire form — `groq` or
    /// `local:<model_id>`.
    ActiveTranscriberChanged {
        backend: String,
    },
    /// A requested backend switch failed to load (missing/corrupt model
    /// files, engine error). Always followed by `ActiveTranscriberChanged`
    /// naming the backend still running, so the GUI drops its "Loading…"
    /// and shows `error` on the row instead.
    ActiveTranscriberFailed {
        backend: String,
        error: String,
    },
    /// Transcription failed (e.g. Groq API error: bad key, quota, or
    /// network). Surfaced to the GUI as a red error overlay so the user
    /// isn't left on a stuck "Processing…". `kind` is a stable machine tag
    /// (`auth` | `quota` | `network` | `error`); `message` is short human text.
    TranscriptionFailed {
        kind: String,
        message: String,
    },
    /// The Groq circuit breaker opened and the daemon loaded the named
    /// local model to keep transcribing offline. Emitted once per daemon
    /// run (the engine stays loaded afterwards); the GUI flashes a brief
    /// "using local model" notice so degradation is never silent.
    SttFallbackEngaged {
        model: String,
    },
    /// "My Voice" enrollment accepted for `step`, but the daemon is still
    /// preparing (speaker model download/load). Recording has NOT begun;
    /// the GUI must not show "speak now" until `VoiceEnrollmentStarted`.
    VoiceEnrollmentPreparing {
        step: String,
    },
    /// "My Voice" enrollment recording started for `step`
    /// (`normal` | `lower` | `louder`). The daemon is now capturing the
    /// user's guided sample.
    VoiceEnrollmentStarted {
        step: String,
    },
    /// Live level meter + progress during an enrollment recording.
    /// Throttled to ~10 Hz. `voiced_ms` counts actual voiced audio
    /// accumulated toward `target_ms`.
    VoiceEnrollmentLevel {
        energy: f64,
        voiced_ms: u32,
        target_ms: u32,
    },
    /// One enrollment step's sample was captured and folded into the
    /// voiceprint. A `VoiceProfileStatus` follows with the new totals.
    VoiceEnrollmentSampleCaptured {
        step: String,
    },
    /// Enrollment recording failed or was cancelled. `message` is short
    /// human text ("cancelled", "no speech detected", download errors).
    VoiceEnrollmentFailed {
        step: String,
        message: String,
    },
    /// Snapshot of the "My Voice" profile: which steps are recorded,
    /// whether the profile is complete (`enrolled`), and whether the
    /// runtime gate pref is on. Included in the initial state burst and
    /// re-emitted on every mutation.
    VoiceProfileStatus {
        enrolled: bool,
        enabled: bool,
        steps: Vec<String>,
    },
    /// A newer release than the running build is published on GitHub
    /// (SPEC §12a Updates). Notify-only: nothing installs until the client
    /// sends [`DaemonCommand::InstallUpdate`] (or the user runs
    /// `always update`). Sent when a check finds an update and in the
    /// initial state burst of every client that connects while an update
    /// is known. Strings only, like `TranscriptHeld`, so the GUI's loose
    /// `[String: String]` payload decode accepts it.
    /// - `current` / `latest`: semver without the `v` (`"0.1.0"`).
    /// - `url`: the release page (`https://github.com/LivioGama/always/releases/tag/v<latest>`).
    /// - `can_apply`: `"true"` / `"false"` — whether `InstallUpdate` can
    ///   work for THIS install (false: dev instance, Linux deb/rpm, a
    ///   binary outside an installed location, no asset for this target).
    /// - `apply_hint`: empty when `can_apply` is `"true"`, else a
    ///   ready-to-show sentence saying what to do instead.
    UpdateAvailable {
        current: String,
        latest: String,
        url: String,
        can_apply: String,
        apply_hint: String,
    },
    /// Answer to [`DaemonCommand::CheckForUpdates`] (user-initiated checks
    /// only; background checks stay silent unless they find an update).
    /// Strings only. `result` is one of `up_to_date`, `update_available`
    /// (an `UpdateAvailable` follows), `no_release` (nothing published
    /// yet), `offline`, `rate_limited`, `error`. `latest` is `""` when the
    /// check did not learn it.
    UpdateCheckDone {
        result: String,
        current: String,
        latest: String,
    },
    /// Progress of an [`DaemonCommand::InstallUpdate`]. Strings only.
    /// `phase`: `downloading` (`percent` `"0"`..`"100"`, `detail` = asset
    /// name), `verifying` (minisign check of every downloaded file),
    /// `installing` (the verified installer was started detached; on macOS
    /// it quits the app and daemon, swaps `Always.app` and relaunches, so
    /// the UDS connection ends shortly after this frame). `percent` is
    /// `""` outside `downloading`.
    UpdateProgress {
        phase: String,
        percent: String,
        detail: String,
    },
    /// An [`DaemonCommand::InstallUpdate`] stopped before the installer
    /// started, or could not start it. Nothing was changed on disk outside
    /// the updater's own download directory. Strings only. `stage`:
    /// `refused` (this install cannot be updated from the GUI),
    /// `check`, `download`, `verify` (a signature or checksum did not
    /// verify: the download was discarded), `install` (spawning the
    /// installer failed). `message` is short human text.
    UpdateFailed {
        stage: String,
        message: String,
    },
}

/// Upper bound (in chars) on any single user-/network-supplied string
/// field before it is serialized for broadcast. A pathologically long
/// transcript, error string, or model catalog entry would otherwise
/// balloon the line buffer of *every* connected client. Truncating here
/// keeps a single oversized event from degrading the whole UDS fan-out.
const MAX_EVENT_FIELD_CHARS: usize = 8192;

/// Marker appended to a field that was truncated, so a reader can tell
/// the value was elided rather than legitimately ending mid-word.
const ELISION_MARKER: &str = "…[truncated]";

/// Truncate `s` to at most [`MAX_EVENT_FIELD_CHARS`] characters on a char
/// boundary, appending [`ELISION_MARKER`] when truncation occurred.
/// Returns `None` when `s` is already within bounds so callers can skip
/// the allocation for the common case.
fn cap_field(s: &str) -> Option<String> {
    // `chars().count()` is O(n) but only paid for strings that are
    // suspiciously long (the rare case we care about); short strings hit
    // the cheap `len()` early-out below.
    if s.len() <= MAX_EVENT_FIELD_CHARS {
        // `len()` (bytes) <= cap (chars) implies char count <= cap, since
        // each char is >= 1 byte. Cheap, always-correct fast path.
        return None;
    }
    if s.chars().count() <= MAX_EVENT_FIELD_CHARS {
        return None;
    }
    let mut out: String = s.chars().take(MAX_EVENT_FIELD_CHARS).collect();
    out.push_str(ELISION_MARKER);
    Some(out)
}

impl DaemonEvent {
    /// Convert event to JSON line.
    ///
    /// Over-long string fields are capped to `MAX_EVENT_FIELD_CHARS`
    /// before serialization so one giant transcript / error / catalog
    /// entry cannot balloon every connected client's line buffer. The
    /// public enum shape and the serialization format for normal-size
    /// events are unchanged.
    pub fn to_json_line(&self) -> Result<String, serde_json::Error> {
        let json = match self.capped() {
            Some(capped) => serde_json::to_string(&capped)?,
            None => serde_json::to_string(self)?,
        };
        Ok(json + "\n")
    }

    /// Return a size-capped clone of `self` when any oversized string
    /// field needs truncation, or `None` when the event is already within
    /// bounds (the overwhelming common case — no clone, no allocation).
    fn capped(&self) -> Option<DaemonEvent> {
        match self {
            DaemonEvent::TranscriptChunk { text } => {
                cap_field(text).map(|text| DaemonEvent::TranscriptChunk { text })
            }
            DaemonEvent::TranscriptFinal { text } => {
                cap_field(text).map(|text| DaemonEvent::TranscriptFinal { text })
            }
            DaemonEvent::TranscriptHeld { id, text, at_us } => {
                cap_field(text).map(|text| DaemonEvent::TranscriptHeld {
                    id: id.clone(),
                    text,
                    at_us: at_us.clone(),
                })
            }
            DaemonEvent::TranscriptionInterim { text } => {
                cap_field(text).map(|text| DaemonEvent::TranscriptionInterim { text })
            }
            DaemonEvent::TranscriptionInterimPending { text } => {
                cap_field(text).map(|text| DaemonEvent::TranscriptionInterimPending { text })
            }
            DaemonEvent::TranscriptRejected {
                id,
                text,
                score,
                reason,
                prompt,
            } => cap_field(text).map(|text| DaemonEvent::TranscriptRejected {
                id: id.clone(),
                text,
                score: score.clone(),
                reason: reason.clone(),
                prompt: prompt.clone(),
            }),
            DaemonEvent::GrammarCorrected { before, after } => {
                let cb = cap_field(before);
                let ca = cap_field(after);
                if cb.is_some() || ca.is_some() {
                    Some(DaemonEvent::GrammarCorrected {
                        before: cb.unwrap_or_else(|| before.clone()),
                        after: ca.unwrap_or_else(|| after.clone()),
                    })
                } else {
                    None
                }
            }
            DaemonEvent::TranscriptionFiltered { reason } => {
                cap_field(reason).map(|reason| DaemonEvent::TranscriptionFiltered { reason })
            }
            DaemonEvent::TranscriptionFailed { kind, message } => {
                cap_field(message).map(|message| DaemonEvent::TranscriptionFailed {
                    kind: kind.clone(),
                    message,
                })
            }
            DaemonEvent::CorrectionDialogRequested { last_transcript } => {
                cap_field(last_transcript).map(|last_transcript| {
                    DaemonEvent::CorrectionDialogRequested { last_transcript }
                })
            }
            DaemonEvent::ModelDownloadFailed { model_id, error } => {
                cap_field(error).map(|error| DaemonEvent::ModelDownloadFailed {
                    model_id: model_id.clone(),
                    error,
                })
            }
            DaemonEvent::ModelExtractionFailed { model_id, error } => {
                cap_field(error).map(|error| DaemonEvent::ModelExtractionFailed {
                    model_id: model_id.clone(),
                    error,
                })
            }
            DaemonEvent::ActiveTranscriberFailed { backend, error } => {
                cap_field(error).map(|error| DaemonEvent::ActiveTranscriberFailed {
                    backend: backend.clone(),
                    error,
                })
            }
            _ => None,
        }
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
#[serde(tag = "type", content = "data")]
pub enum DaemonCommand {
    TogglePause,
    /// Explicit, idempotent "resume listening" (the idle pill's Play):
    /// clears master, idle and watchdog pauses; never pauses.
    Resume,
    ToggleAutoEnter,
    /// Set auto-enter on/off to an explicit value (Settings toggle).
    SetAutoEnter {
        enabled: bool,
    },
    /// "Consume mode" — route transcription to the daemon's stream consumers
    /// (any UDS client / the transcript-stream file) INSTEAD of the
    /// clipboard+paste+enter path. This is the generic hook that lets an
    /// external controller drive Always for its own use case: while enabled
    /// the daemon transcribes regardless of per-app/master pause (there is no
    /// focused app to paste into) and suppresses all pasting. Cleared when the
    /// last client disconnects so normal dictation resumes. Idempotent.
    SetConsumeMode {
        enabled: bool,
    },
    /// Hand a held final (see `DaemonEvent::TranscriptHeld`) back to the
    /// native paste + auto-enter path. `id` is the decimal id from the
    /// frame; when it is missing/unknown, `text` is matched exactly.
    ReleaseHeld {
        #[serde(default)]
        id: String,
        #[serde(default)]
        text: Option<String>,
    },
    /// Drop a held final: the consumer claimed it. Same addressing as
    /// `ReleaseHeld`.
    DiscardHeld {
        #[serde(default)]
        id: String,
        #[serde(default)]
        text: Option<String>,
    },
    /// Play one status cue through the normal path (`cue()`: honours the
    /// `audible_status_sound` setting, logs `status_sound_played`). Used by
    /// the launch smoke test; harmless for anyone else. `cue` is one of
    /// listening, transcribing, done, error, cancel, command.
    PlayStatusCue {
        cue: String,
    },
    /// Iris's action-progress status (task/goal/step text/action label).
    /// Accepted so the frame is not an "unknown variant" error; the daemon
    /// does not render it — Iris shows action progress in its own notch.
    SetActStatus {
        #[serde(default)]
        task: String,
        #[serde(default)]
        goal: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        action: String,
    },
    /// Paste a transcript the "My Voice" gate rejected (see
    /// `DaemonEvent::TranscriptRejected`) — the user confirmed it was
    /// them. Empty / "0" id = the most recent one.
    PasteRejected {
        #[serde(default)]
        id: String,
    },
    /// Hot-reload sensitivity + auto-enter delay without restart.
    ApplyRuntimePreferences {
        auto_enter_delay_ms: u32,
        energy_threshold: f64,
        silence_secs: f64,
        cooldown_ms: u32,
        silero_threshold: f32,
        /// Optional so payloads from older GUI builds still decode.
        #[serde(default)]
        adaptive_silence: Option<bool>,
        /// Optional so payloads from older GUI builds still decode.
        #[serde(default)]
        audible_status_sound: Option<String>,
        /// Live mid-speech preview on non-streaming backends.
        /// Optional so payloads from older GUI builds still decode.
        #[serde(default)]
        stt_live_preview: Option<bool>,
        /// Live typing (§5). Optional so payloads from older GUI builds
        /// still decode.
        #[serde(default)]
        live_typing: Option<bool>,
    },
    /// Approve a pending correction in the queue and apply it to the
    /// glossary. The daemon emits `CorrectionLogged` on success.
    ApproveCorrection {
        id: String,
    },
    /// Drop a pending correction without applying.
    RejectCorrection {
        id: String,
    },
    /// Manually trigger the active capture path (read user's selection,
    /// diff vs `last_pasted`, apply). Useful for clients without the
    /// global hotkey installed.
    CaptureCorrection,
    /// Explicit pause set/clear with optional reason string (for logs).
    /// Used by the Swift audio-output monitor and per-app overrides.
    SetPaused {
        paused: bool,
        reason: Option<String>,
    },
    /// Acquire/release an ownership-tagged pause on the other Always
    /// instance. Peers must negotiate `PEER_PAUSE_LEASE_CAPABILITY` first.
    PeerPauseLease {
        request_id: String,
        paused: bool,
    },
    /// Cancel the active auto-enter countdown (e.g. Mac app saw a
    /// keystroke land in the focused app).
    CancelAutoEnterCountdown,
    /// macOS Swift app reports the user switched focused application.
    NotifyFocusedAppChanged {
        bundle_id: Option<String>,
    },
    /// macOS Swift app reports the system audio-output device started
    /// or stopped producing sound. Daemon may auto-pause/resume.
    NotifySystemAudioState {
        playing: bool,
    },
    /// User submitted the correction dialog with the intended spelling.
    /// Daemon diffs against `last_pasted`/`last_transcript`, finds the
    /// closest wrong-word match, and updates the glossary (add entry,
    /// remove over-fired entry, or bump weight).
    LogCorrection {
        intended: String,
    },
    /// Set (or clear) the per-app `paused` override for `bundle_id`.
    /// `paused = Some(false)` → app is on the resumed-allowlist.
    /// `paused = Some(true)` → app is force-paused even though it
    /// would otherwise inherit the (paused-by-default) global rule.
    /// `paused = None` → remove the override entirely; the app reverts
    /// to the default (paused).
    SetAppPaused {
        bundle_id: String,
        paused: Option<bool>,
    },
    /// Settings → Models requested a catalog snapshot. Daemon
    /// responds with [`DaemonEvent::ModelsList`].
    ListModels,
    /// Begin downloading `model_id`. Idempotent — already-downloaded
    /// or in-progress IDs are no-ops. Progress events stream on the
    /// `ModelDownload*` channel until completion or cancel.
    DownloadModel {
        model_id: String,
    },
    /// Cancel an in-flight download. Partial file is kept so the next
    /// `DownloadModel` for the same id resumes.
    CancelModelDownload {
        model_id: String,
    },
    /// Remove a downloaded model from disk.
    DeleteModel {
        model_id: String,
    },
    /// Switch the active STT backend. `backend` is the canonical wire
    /// form parsed by
    /// [`crate::stt_dispatch::TranscriberBackendChoice`] — `groq` or
    /// `local:<model_id>`. Daemon emits
    /// [`DaemonEvent::ActiveTranscriberChanged`] on success.
    SetActiveTranscriber {
        backend: String,
    },
    /// Change the transcription language live. `lang` is an ISO 639-1 code
    /// or "auto". Daemon updates `cfg.lang`, persists it, and rebuilds the
    /// active transcriber so the change takes effect without a restart.
    /// Critical for engines like Canary that bake the language into the
    /// decode prompt (a stale language silently mistranscribes).
    SetLanguage {
        lang: String,
    },
    /// Settings → My Voice: record the guided sample for `step`
    /// (`normal` | `lower` | `louder`). The daemon interrupts idle
    /// listening, records until enough voiced audio accumulates, and
    /// streams `VoiceEnrollment*` progress events.
    StartVoiceEnrollment {
        step: String,
    },
    /// Abort an in-flight (or queued) enrollment recording.
    CancelVoiceEnrollment,
    /// Delete the enrolled voiceprint entirely. The runtime gate
    /// degrades to ungated behavior until re-enrollment.
    DeleteVoiceProfile,
    /// Flip the "only listen to my voice" gate pref. Persisted to the
    /// DB and hot-applied to the running config.
    SetVoiceProfileEnabled {
        enabled: bool,
    },
    /// Client (Settings tab open / reconnect) wants a fresh
    /// [`DaemonEvent::VoiceProfileStatus`] snapshot.
    GetVoiceProfileStatus,
    /// Client (Settings open / reconnect) wants a fresh
    /// [`DaemonEvent::ShortcutListenerStatus`]. The startup burst can be
    /// missed by a client whose listeners attach late.
    GetShortcutListenerStatus,
    /// macOS Swift app reports the system default input device changed
    /// (user picked a different mic in System Settings). Daemon kills
    /// and respawns the `rec` child so it opens the new default device
    /// without requiring an app relaunch.
    RespawnRecorder,
    /// Settings → Shortcuts saved a new key combo. Daemon re-reads the
    /// prefs DB and swaps the live keyboard listener's shortcut set —
    /// no restart needed. Sent after each `always config set shortcut_*`
    /// call from the Swift app.
    ReloadShortcuts,
    /// Iris → voice edit: run an edit command (same grammar as dictation,
    /// e.g. "scratch that", "change X to Y"). `force` = Iris was addressed
    /// explicitly, so the paste-window gate is skipped (secure fields and
    /// verification still apply). Answered with `DaemonEvent::VoiceEdit`.
    VoiceEdit {
        #[serde(default)]
        req: String,
        command: String,
        #[serde(default)]
        force: bool,
    },
    /// Iris heartbeat: its rewrite handler is connected, so Always may
    /// claim "make that shorter" / "translate that to …" and hand it off
    /// (`DaemonEvent::VoiceEdit { op: "rewrite_request" }`). Expires 15 s
    /// after the last ping.
    VoiceEditRewriterAlive,
    /// Iris → read what "that" is: the selection, else the last paste.
    /// Answered with `DaemonEvent::VoiceEdit { op: "get_target", text,
    /// detail: "selection" | "last_paste" | "none" | "secure" }`.
    VoiceEditGetTarget {
        #[serde(default)]
        req: String,
    },
    /// Iris → replace the target (as read by `VoiceEditGetTarget`) with
    /// `text` in ONE undoable edit. Refused when the target no longer
    /// equals `expect` or the field is secure.
    VoiceEditReplaceTarget {
        #[serde(default)]
        req: String,
        expect: String,
        text: String,
    },
    /// Setup mode — bypasses per-app pause rules so the daemon allows
    /// recording + pasting while the onboarding wizard is visible.
    /// This lets users test voice typing during setup without having
    /// to add an app to the resumed-app allowlist first. Sent by the
    /// Swift GUI when onboarding appears/dismisses.
    SetupMode {
        enabled: bool,
    },
    /// "Check for updates" (About panel / menu). Runs one GitHub release
    /// check now, even when the background check is disabled by
    /// `update_check` / `ALWAYS_UPDATE_CHECK=0` (an explicit request).
    /// Answered with [`DaemonEvent::UpdateCheckDone`] (and
    /// [`DaemonEvent::UpdateAvailable`] when a newer release exists).
    CheckForUpdates,
    /// "Install update". Downloads the platform asset plus the release's
    /// installer, verifies every file against the embedded minisign key,
    /// then starts the verified installer detached (SPEC §12a). Progress
    /// streams as [`DaemonEvent::UpdateProgress`], failure as
    /// [`DaemonEvent::UpdateFailed`]. Refused (`UpdateFailed { stage:
    /// "refused" }`) when the last `UpdateAvailable` had `can_apply:
    /// "false"`, and when another install is already running.
    InstallUpdate,
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
    /// True while a `VoiceActivityDetected` has been broadcast without a
    /// matching `VoiceActivityEnded`. Lets every utterance exit path emit
    /// `voice_activity_ended()` unconditionally — the broadcaster swallows
    /// the call when nothing is active, so idle `Silence` cycles don't
    /// flood the UDS clients with redundant terminal events.
    voice_active: Arc<AtomicBool>,
    /// Same transition guard for `TranscribingStarted`/`TranscribingStopped`.
    transcribing_active: Arc<AtomicBool>,
}

impl EventBroadcaster {
    /// Create a new event broadcaster
    pub fn new() -> Self {
        // Capacity is a burst buffer, not a queue depth target. 100 was
        // small enough to overflow on ordinary bursts — a model-catalog
        // refresh, a rapid pause/resume, an utterance's worth of chunk
        // events — and every overflow cost the GUI its event stream (see
        // the `Lagged` handling in `uds_server::handle_client`). The
        // events are small; the extra headroom is a few KB.
        let (tx, _rx) = broadcast::channel(1024);
        Self {
            tx,
            voice_active: Arc::new(AtomicBool::new(false)),
            transcribing_active: Arc::new(AtomicBool::new(false)),
        }
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

    /// Send transcribing started event. Always broadcast (re-emits double
    /// as a keep-alive heartbeat for the GUI's stale-state watchdog).
    pub fn transcribing_started(&self) {
        if !self.transcribing_active.swap(true, Ordering::SeqCst) {
            crate::always::status_sound::cue(
                crate::always::status_sound::StatusSound::Transcribing,
            );
        }
        self.send(DaemonEvent::TranscribingStarted);
    }

    /// Send transcribing stopped event — only when transcribing was
    /// announced and not yet stopped (safe to call from any exit path).
    pub fn transcribing_stopped(&self) {
        if self.transcribing_active.swap(false, Ordering::SeqCst) {
            self.send(DaemonEvent::TranscribingStopped);
        }
    }

    /// Send partial transcript chunk (speculative / streaming preview)
    pub fn transcript_chunk(&self, text: String) {
        self.send(DaemonEvent::TranscriptChunk { text });
    }

    /// Announce a consume-mode final whose paste is parked under `id`.
    pub fn transcript_held(&self, id: u64, text: String) {
        let at_us = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros())
            .unwrap_or(0);
        self.send(DaemonEvent::TranscriptHeld {
            id: id.to_string(),
            text,
            at_us: at_us.to_string(),
        });
    }

    /// Send transcript final event
    pub fn transcript_final(&self, text: String) {
        crate::always::status_sound::cue(crate::always::status_sound::StatusSound::Success);
        self.send(DaemonEvent::TranscriptFinal { text });
    }

    /// `TranscriptFinal` without the Success cue: a consume-mode final that
    /// was parked. Its cue plays on the verdict instead — Success when it is
    /// released to paste, Command when the consumer claims it.
    pub fn transcript_final_quiet(&self, text: String) {
        self.send(DaemonEvent::TranscriptFinal { text });
    }

    /// Unverified live preview (rendered dimmed by the GUI only).
    pub fn transcription_interim_pending(&self, text: String) {
        self.send(DaemonEvent::TranscriptionInterimPending { text });
    }

    /// Live capture level tick (see `DaemonEvent::VoiceLevel`).
    pub fn voice_level(&self, energy: f64, voiced: bool, bands: [f32; 5]) {
        let energy_db = if energy > 1e-5 {
            20.0 * energy.log10()
        } else {
            -100.0
        };
        self.send(DaemonEvent::VoiceLevel {
            energy,
            energy_db: (energy_db * 10.0).round() / 10.0,
            voiced,
            bands,
        });
    }

    /// A "My Voice"-rejected utterance's transcript, recoverable by id.
    pub fn transcript_rejected(&self, id: u64, text: String, score: f32) {
        self.transcript_rejected_for(id, text, score, RejectReason::Speaker);
    }

    /// A not-pasted utterance's transcript, recoverable by id, with the
    /// reason it was held back (drives the recovery prompt copy).
    pub fn transcript_rejected_for(&self, id: u64, text: String, score: f32, reason: RejectReason) {
        self.send(DaemonEvent::TranscriptRejected {
            id: id.to_string(),
            text,
            score: format!("{score:.3}"),
            reason: reason.as_str().to_string(),
            prompt: reason.prompt().to_string(),
        });
    }

    /// Send interim transcription result from a streaming transcriber.
    /// This text is provisional and may change as more audio is processed.
    /// Used by streaming engines to provide progressive updates to the UI.
    pub fn transcription_interim(&self, text: String) {
        self.send(DaemonEvent::TranscriptionInterim { text });
    }

    /// Notify the UI that async grammar correction replaced the pasted text
    pub fn grammar_corrected(&self, before: String, after: String) {
        self.send(DaemonEvent::GrammarCorrected { before, after });
    }

    /// Send paused event
    pub fn paused(&self) {
        self.send(DaemonEvent::Paused);
    }

    /// Same as `paused` but tells the GUI to update state silently — no
    /// overlay flash. Used by focus-change-driven per-app pause so a
    /// manual mouse window switch doesn't pop pause/play badges.
    pub fn paused_quietly(&self) {
        self.send(DaemonEvent::PausedQuietly);
    }

    /// Send resumed event
    pub fn resumed(&self) {
        self.send(DaemonEvent::Resumed);
    }

    /// Silent counterpart to `resumed` — see `paused_quietly`.
    pub fn resumed_quietly(&self) {
        self.send(DaemonEvent::ResumedQuietly);
    }

    /// Send auto-enter enabled event
    pub fn auto_enter_enabled(&self) {
        self.send(DaemonEvent::AutoEnterEnabled);
    }

    /// Send auto-enter disabled event
    pub fn auto_enter_disabled(&self) {
        self.send(DaemonEvent::AutoEnterDisabled);
    }

    /// Send voice activity detected event. Always broadcast (re-emits
    /// double as a keep-alive heartbeat for the GUI's stale-state watchdog).
    pub fn voice_activity_detected(&self) {
        if !self.voice_active.swap(true, Ordering::SeqCst) {
            crate::always::status_sound::cue(crate::always::status_sound::StatusSound::Listening);
        }
        self.send(DaemonEvent::VoiceActivityDetected);
    }

    /// Send voice activity ended event — only when voice activity was
    /// announced and not yet ended (safe to call from any exit path).
    pub fn voice_activity_ended(&self) {
        if self.voice_active.swap(false, Ordering::SeqCst) {
            self.send(DaemonEvent::VoiceActivityEnded);
        }
    }

    /// Report whether the daemon's keyboard event tap is authorized
    /// (Input Monitoring). See [`DaemonEvent::ShortcutListenerStatus`].
    pub fn shortcut_listener_status(&self, input_monitoring_granted: bool) {
        self.send(DaemonEvent::ShortcutListenerStatus {
            input_monitoring_granted,
        });
    }

    /// Report that the microphone recorder is stuck (`true`) or delivering
    /// audio again (`false`). See [`DaemonEvent::MicUnavailable`].
    pub fn mic_unavailable(&self, unavailable: bool) {
        self.send(DaemonEvent::MicUnavailable { unavailable });
    }

    /// Send transcription-filtered event with the human-readable reason.
    pub fn transcription_filtered(&self, reason: impl Into<String>) {
        crate::always::status_sound::cue(crate::always::status_sound::StatusSound::Cancel);
        self.send(DaemonEvent::TranscriptionFiltered {
            reason: reason.into(),
        });
    }

    /// Send a transcription-failed event (Groq/STT error) so the GUI can
    /// flash a red error overlay instead of leaving a stuck "Processing…".
    pub fn transcription_failed(&self, kind: impl Into<String>, message: impl Into<String>) {
        crate::always::status_sound::cue(crate::always::status_sound::StatusSound::Failure);
        self.send(DaemonEvent::TranscriptionFailed {
            kind: kind.into(),
            message: message.into(),
        });
    }

    /// Announce that the Groq breaker opened and the named local model
    /// took over transcription. See [`DaemonEvent::SttFallbackEngaged`].
    pub fn stt_fallback_engaged(&self, model: impl Into<String>) {
        self.send(DaemonEvent::SttFallbackEngaged {
            model: model.into(),
        });
    }

    /// Send the protocol-version handshake. Must be the first frame on
    /// every new UDS connection.
    pub fn hello(&self) {
        self.send(DaemonEvent::Hello {
            version: PROTOCOL_VERSION,
            build_id: Some(BUILD_ID.to_string()),
            capabilities: vec![PEER_PAUSE_LEASE_CAPABILITY.to_string()],
        });
    }

    /// Send `CorrectionLogged` event (typically from the hotkey-driven
    /// capture path or after an approval).
    pub fn correction_logged(&self, wrong: impl Into<String>, right: impl Into<String>) {
        self.send(DaemonEvent::CorrectionLogged {
            wrong: wrong.into(),
            right: right.into(),
        });
    }

    /// Send `CorrectionPending` event (from passive watcher).
    pub fn correction_pending(
        &self,
        id: impl Into<String>,
        wrong: impl Into<String>,
        right: impl Into<String>,
    ) {
        self.send(DaemonEvent::CorrectionPending {
            id: id.into(),
            wrong: wrong.into(),
            right: right.into(),
        });
    }

    /// Broadcast the outcome of an active correction-capture press
    /// (⌃⌥X). `outcome` is one of: `applied`, `no_recent_paste`,
    /// `no_change`, `no_correction_pairs`, `error`.
    pub fn correction_capture_result(&self, outcome: impl Into<String>) {
        self.send(DaemonEvent::CorrectionCaptureResult {
            outcome: outcome.into(),
        });
    }

    pub fn auto_enter_countdown_started(&self, remaining_ms: u32, total_ms: u32) {
        self.send(DaemonEvent::AutoEnterCountdownStarted {
            remaining_ms,
            total_ms,
        });
    }

    pub fn auto_enter_countdown_tick(&self, remaining_ms: u32) {
        self.send(DaemonEvent::AutoEnterCountdownTick { remaining_ms });
    }

    pub fn auto_enter_countdown_cancelled(&self) {
        self.send(DaemonEvent::AutoEnterCountdownCancelled);
    }

    pub fn auto_enter_countdown_finished(&self) {
        self.send(DaemonEvent::AutoEnterCountdownFinished);
    }

    pub fn idle_auto_paused(&self, seconds: u32) {
        self.send(DaemonEvent::IdleAutoPaused { seconds });
    }

    pub fn idle_auto_resumed(&self) {
        self.send(DaemonEvent::IdleAutoResumed);
    }

    pub fn focused_app_changed(&self, bundle_id: Option<String>) {
        self.send(DaemonEvent::FocusedAppChanged { bundle_id });
    }

    pub fn low_microphone_volume(&self, energy: f64) {
        self.send(DaemonEvent::LowMicrophoneVolume { energy });
    }

    /// Rate-limited wrapper for [`Self::low_microphone_volume`].
    pub fn low_microphone_volume_maybe(&self, energy: f64) {
        let mut last = LAST_LOW_MIC_OVERLAY.lock();
        if last.is_some_and(|t| t.elapsed() < LOW_MIC_OVERLAY_COOLDOWN) {
            return;
        }
        *last = Some(Instant::now());
        self.low_microphone_volume(energy);
    }

    pub fn master_pause_changed(&self, master_paused: bool) {
        self.send(DaemonEvent::MasterPauseChanged { master_paused });
    }

    /// Announce which scope a pause hotkey just toggled. `bundle_id` is
    /// `Some` for per-app toggles, `None` for the master switch.
    pub fn pause_scope_toggled(&self, scope: &str, bundle_id: Option<String>, paused: bool) {
        self.send(DaemonEvent::PauseScopeToggled {
            scope: scope.to_string(),
            bundle_id,
            paused,
        });
    }

    /// One-shot warning that a continuous recording is approaching the
    /// hard cap (emitted once per utterance by the VAD loop).
    pub fn long_recording_warning(&self, elapsed_secs: u32, cap_secs: u32) {
        self.send(DaemonEvent::LongRecordingWarning {
            elapsed_secs,
            cap_secs,
        });
    }

    pub fn voice_enrollment_preparing(&self, step: &str) {
        self.send(DaemonEvent::VoiceEnrollmentPreparing {
            step: step.to_string(),
        });
    }

    pub fn voice_enrollment_started(&self, step: &str) {
        self.send(DaemonEvent::VoiceEnrollmentStarted {
            step: step.to_string(),
        });
    }

    pub fn voice_enrollment_level(&self, energy: f64, voiced_ms: u32, target_ms: u32) {
        self.send(DaemonEvent::VoiceEnrollmentLevel {
            energy,
            voiced_ms,
            target_ms,
        });
    }

    pub fn voice_enrollment_sample_captured(&self, step: &str) {
        self.send(DaemonEvent::VoiceEnrollmentSampleCaptured {
            step: step.to_string(),
        });
    }

    pub fn voice_enrollment_failed(&self, step: &str, message: impl Into<String>) {
        self.send(DaemonEvent::VoiceEnrollmentFailed {
            step: step.to_string(),
            message: message.into(),
        });
    }

    pub fn voice_profile_status(&self, enrolled: bool, enabled: bool, steps: Vec<String>) {
        self.send(DaemonEvent::VoiceProfileStatus {
            enrolled,
            enabled,
            steps,
        });
    }

    /// A watchdog pause source flipped (mic conflict / audio output).
    pub fn pause_source_changed(&self, source: &str, paused: bool, detail: Option<String>) {
        self.send(DaemonEvent::PauseSourceChanged {
            source: source.to_string(),
            paused,
            detail,
        });
    }

    pub fn resumed_apps_changed(&self, bundles: Vec<String>) {
        self.send(DaemonEvent::ResumedAppsChanged { bundles });
    }

    pub fn correction_dialog_requested(&self, last_transcript: impl Into<String>) {
        self.send(DaemonEvent::CorrectionDialogRequested {
            last_transcript: last_transcript.into(),
        });
    }
}

impl Default for EventBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

/// Global event broadcaster instance
static GLOBAL_BROADCASTER: std::sync::LazyLock<EventBroadcaster> =
    std::sync::LazyLock::new(EventBroadcaster::new);

/// Get the global event broadcaster
pub fn global_broadcaster() -> &'static EventBroadcaster {
    &GLOBAL_BROADCASTER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_reports_the_daemon_build_identity() {
        let hello = serde_json::to_value(DaemonEvent::Hello {
            version: PROTOCOL_VERSION,
            build_id: Some(BUILD_ID.to_string()),
            capabilities: vec![PEER_PAUSE_LEASE_CAPABILITY.to_string()],
        })
        .expect("hello serializes");
        assert!(hello["data"]["build_id"].is_string());
    }

    #[test]
    fn hello_from_an_older_daemon_remains_decodable() {
        let hello: DaemonEvent = serde_json::from_str(r#"{"type":"Hello","data":{"version":12}}"#)
            .expect("older hello remains compatible");
        assert!(matches!(
            hello,
            DaemonEvent::Hello {
                build_id: None,
                capabilities,
                ..
            } if capabilities.is_empty()
        ));
    }

    #[test]
    fn peer_pause_lease_command_is_correlated_and_decodable() {
        let command = DaemonCommand::from_json_line(
            r#"{"type":"PeerPauseLease","data":{"request_id":"lease-7","paused":true}}"#,
        )
        .expect("peer lease command parses");
        assert!(matches!(
            command,
            DaemonCommand::PeerPauseLease {
                request_id,
                paused: true
            } if request_id == "lease-7"
        ));
    }

    #[test]
    fn set_consume_mode_command_parses_from_iris_wire_format() {
        // The exact JSON Iris writes to the socket. Must decode to the command.
        let on =
            DaemonCommand::from_json_line(r#"{"type":"SetConsumeMode","data":{"enabled":true}}"#)
                .expect("enable must parse");
        assert!(matches!(
            on,
            DaemonCommand::SetConsumeMode { enabled: true }
        ));
        let off =
            DaemonCommand::from_json_line(r#"{"type":"SetConsumeMode","data":{"enabled":false}}"#)
                .expect("disable must parse");
        assert!(matches!(
            off,
            DaemonCommand::SetConsumeMode { enabled: false }
        ));
    }

    #[test]
    fn set_act_status_from_iris_is_accepted() {
        let c = DaemonCommand::from_json_line(
            r#"{"type":"SetActStatus","data":{"task":"movie","goal":"watch","text":"Opening","action":""}}"#,
        )
        .expect("SetActStatus must parse");
        assert!(matches!(c, DaemonCommand::SetActStatus { ref task, .. } if task == "movie"));
    }

    #[test]
    fn held_commands_parse_from_iris_wire_format() {
        let r = DaemonCommand::from_json_line(r#"{"type":"ReleaseHeld","data":{"id":"7"}}"#)
            .expect("release must parse");
        assert!(matches!(r, DaemonCommand::ReleaseHeld { ref id, text: None } if id == "7"));
        let d = DaemonCommand::from_json_line(
            r#"{"type":"DiscardHeld","data":{"text":"open terminal"}}"#,
        )
        .expect("discard by text must parse");
        assert!(
            matches!(d, DaemonCommand::DiscardHeld { ref id, text: Some(ref t) } if id.is_empty() && t == "open terminal")
        );
    }

    #[test]
    fn new_preview_level_and_rejected_events_serialize() {
        let ev = DaemonEvent::VoiceLevel {
            energy: 0.5,
            energy_db: -6.0,
            voiced: true,
            bands: [0.1, 0.2, 0.3, 0.2, 0.2],
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            json.starts_with(r#"{"type":"VoiceLevel","data":{"energy":0.5,"energy_db":-6.0,"voiced":true,"bands":[0.1,"#),
            "{json}"
        );
        let ev = DaemonEvent::TranscriptionInterimPending { text: "hi".into() };
        assert_eq!(
            serde_json::to_string(&ev).unwrap(),
            r#"{"type":"TranscriptionInterimPending","data":{"text":"hi"}}"#
        );
        let ev = DaemonEvent::TranscriptRejected {
            id: "2".into(),
            text: "x".into(),
            score: "0.120".into(),
            reason: RejectReason::ForeignSpeech.as_str().into(),
            prompt: RejectReason::ForeignSpeech.prompt().into(),
        };
        assert_eq!(
            serde_json::to_string(&ev).unwrap(),
            r#"{"type":"TranscriptRejected","data":{"id":"2","text":"x","score":"0.120","reason":"foreign_speech","prompt":"Didn't sound like English — paste anyway?"}}"#
        );
        assert_eq!(
            RejectReason::Speaker.prompt(),
            "Not sure that was you — paste anyway?"
        );
        let c =
            DaemonCommand::from_json_line(r#"{"type":"PasteRejected","data":{"id":"2"}}"#).unwrap();
        assert!(matches!(c, DaemonCommand::PasteRejected { ref id } if id == "2"));
    }

    #[test]
    fn transcript_held_serializes_as_strings() {
        let ev = DaemonEvent::TranscriptHeld {
            id: "3".into(),
            text: "hi".into(),
            at_us: "1".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(
            json,
            r#"{"type":"TranscriptHeld","data":{"id":"3","text":"hi","at_us":"1"}}"#
        );
    }

    fn drain(rx: &mut broadcast::Receiver<DaemonEvent>) -> Vec<DaemonEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    #[test]
    fn voice_activity_ended_is_swallowed_when_nothing_active() {
        let b = EventBroadcaster::new();
        let mut rx = b.subscribe();
        // Idle utterance cycles (Silence results) call ended unconditionally —
        // nothing must reach the wire.
        b.voice_activity_ended();
        b.voice_activity_ended();
        assert!(drain(&mut rx).is_empty());
    }

    #[test]
    fn voice_activity_ended_fires_once_per_detected() {
        let b = EventBroadcaster::new();
        let mut rx = b.subscribe();
        b.voice_activity_detected();
        b.voice_activity_ended();
        b.voice_activity_ended(); // duplicate terminal from a second exit path
        let events = drain(&mut rx);
        assert!(matches!(events[0], DaemonEvent::VoiceActivityDetected));
        assert!(matches!(events[1], DaemonEvent::VoiceActivityEnded));
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn voice_activity_detected_reemits_as_heartbeat() {
        let b = EventBroadcaster::new();
        let mut rx = b.subscribe();
        // Lease heartbeat re-emits detected while an utterance is live;
        // every re-emit must reach the GUI to refresh its watchdog.
        b.voice_activity_detected();
        b.voice_activity_detected();
        b.voice_activity_detected();
        b.voice_activity_ended();
        let events = drain(&mut rx);
        assert_eq!(events.len(), 4);
        assert!(matches!(events[3], DaemonEvent::VoiceActivityEnded));
    }

    #[test]
    fn transcribing_stopped_is_transition_guarded() {
        let b = EventBroadcaster::new();
        let mut rx = b.subscribe();
        b.transcribing_stopped(); // never started — swallowed
        b.transcribing_started();
        b.transcribing_stopped();
        b.transcribing_stopped(); // duplicate — swallowed
        let events = drain(&mut rx);
        assert!(matches!(events[0], DaemonEvent::TranscribingStarted));
        assert!(matches!(events[1], DaemonEvent::TranscribingStopped));
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn transcript_chunks_do_not_change_lifecycle_state() {
        let b = EventBroadcaster::new();
        let mut rx = b.subscribe();
        b.transcript_chunk("first rolling chunk".to_string());
        b.transcript_chunk("second rolling chunk".to_string());
        b.transcribing_stopped();
        b.voice_activity_ended();

        let events = drain(&mut rx);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], DaemonEvent::TranscriptChunk { .. }));
        assert!(matches!(events[1], DaemonEvent::TranscriptChunk { .. }));
    }
}
