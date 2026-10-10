// UDS Protocol types — derived from src/always/event.rs
// Protocol version: 12

// ─── DaemonEvent ───────────────────────────────────────────────────────────

export type DaemonEventType =
  | "Hello"
  | "ListeningStarted"
  | "ListeningStopped"
  | "ProcessingStarted"
  | "ProcessingStopped"
  | "TranscribingStarted"
  | "TranscribingStopped"
  | "TranscriptChunk"
  | "TranscriptFinal"
  | "TranscriptionInterim"
  | "GrammarCorrected"
  | "Paused"
  | "Resumed"
  | "PausedQuietly"
  | "ResumedQuietly"
  | "AutoEnterEnabled"
  | "AutoEnterDisabled"
  | "VoiceActivityDetected"
  | "VoiceActivityEnded"
  | "TranscriptionFiltered"
  | "CorrectionLogged"
  | "CorrectionPending"
  | "CorrectionCaptureResult"
  | "Heartbeat"
  | "AutoEnterCountdownStarted"
  | "AutoEnterCountdownTick"
  | "AutoEnterCountdownCancelled"
  | "AutoEnterCountdownFinished"
  | "IdleAutoPaused"
  | "IdleAutoResumed"
  | "FocusedAppChanged"
  | "MasterPauseChanged"
  | "PauseScopeToggled"
  | "LongRecordingWarning"
  | "PauseSourceChanged"
  | "ResumedAppsChanged"
  | "CorrectionDialogRequested"
  | "ModelsList"
  | "ModelDownloadProgress"
  | "ModelDownloadComplete"
  | "ModelDownloadCancelled"
  | "ModelDownloadFailed"
  | "ModelVerificationStarted"
  | "ModelVerificationCompleted"
  | "ModelExtractionStarted"
  | "ModelExtractionCompleted"
  | "ModelExtractionFailed"
  | "LowMicrophoneVolume"
  | "ActiveTranscriberChanged"
  | "TranscriptionFailed"
  | "SttFallbackEngaged"
  | "VoiceEnrollmentStarted"
  | "VoiceEnrollmentLevel"
  | "VoiceEnrollmentSampleCaptured"
  | "VoiceEnrollmentFailed"
  | "VoiceProfileStatus"
  | "ShortcutListenerStatus";

export interface DaemonEventBase {
  type: DaemonEventType;
}

// ─── Events with data ──────────────────────────────────────────────────────

export interface HelloEvent extends DaemonEventBase {
  type: "Hello";
  data: { version: number };
}

export interface TranscriptChunkEvent extends DaemonEventBase {
  type: "TranscriptChunk";
  data: { text: string };
}

export interface TranscriptFinalEvent extends DaemonEventBase {
  type: "TranscriptFinal";
  data: { text: string };
}

export interface TranscriptionInterimEvent extends DaemonEventBase {
  type: "TranscriptionInterim";
  data: { text: string };
}

export interface GrammarCorrectedEvent extends DaemonEventBase {
  type: "GrammarCorrected";
  data: { before: string; after: string };
}

export interface TranscriptionFilteredEvent extends DaemonEventBase {
  type: "TranscriptionFiltered";
  data: { reason: string };
}

export interface CorrectionLoggedEvent extends DaemonEventBase {
  type: "CorrectionLogged";
  data: { wrong: string; right: string };
}

export interface CorrectionPendingEvent extends DaemonEventBase {
  type: "CorrectionPending";
  data: { id: string; wrong: string; right: string };
}

export interface CorrectionCaptureResultEvent extends DaemonEventBase {
  type: "CorrectionCaptureResult";
  data: { outcome: string };
}

export interface AutoEnterCountdownStartedEvent extends DaemonEventBase {
  type: "AutoEnterCountdownStarted";
  data: { remaining_ms: number; total_ms: number };
}

export interface AutoEnterCountdownTickEvent extends DaemonEventBase {
  type: "AutoEnterCountdownTick";
  data: { remaining_ms: number };
}

export interface IdleAutoPausedEvent extends DaemonEventBase {
  type: "IdleAutoPaused";
  data: { seconds: number };
}

export interface FocusedAppChangedEvent extends DaemonEventBase {
  type: "FocusedAppChanged";
  data: { bundle_id: string | null };
}

export interface MasterPauseChangedEvent extends DaemonEventBase {
  type: "MasterPauseChanged";
  data: { master_paused: boolean };
}

export interface PauseScopeToggledEvent extends DaemonEventBase {
  type: "PauseScopeToggled";
  data: { scope: string; bundle_id: string | null; paused: boolean };
}

export interface LongRecordingWarningEvent extends DaemonEventBase {
  type: "LongRecordingWarning";
  data: { elapsed_secs: number; cap_secs: number };
}

export interface PauseSourceChangedEvent extends DaemonEventBase {
  type: "PauseSourceChanged";
  data: { source: string; paused: boolean; detail: string | null };
}

export interface ResumedAppsChangedEvent extends DaemonEventBase {
  type: "ResumedAppsChanged";
  data: { bundles: string[] };
}

export interface CorrectionDialogRequestedEvent extends DaemonEventBase {
  type: "CorrectionDialogRequested";
  data: { last_transcript: string };
}

export interface ModelDownloadProgressEvent extends DaemonEventBase {
  type: "ModelDownloadProgress";
  data: { model_id: string; downloaded: number; total: number; percentage: number };
}

export interface ModelDownloadCompleteEvent extends DaemonEventBase {
  type: "ModelDownloadComplete";
  data: { model_id: string };
}

export interface ModelDownloadCancelledEvent extends DaemonEventBase {
  type: "ModelDownloadCancelled";
  data: { model_id: string };
}

export interface ModelDownloadFailedEvent extends DaemonEventBase {
  type: "ModelDownloadFailed";
  data: { model_id: string; error: string };
}

export interface ModelVerificationStartedEvent extends DaemonEventBase {
  type: "ModelVerificationStarted";
  data: { model_id: string };
}

export interface ModelVerificationCompletedEvent extends DaemonEventBase {
  type: "ModelVerificationCompleted";
  data: { model_id: string };
}

export interface ModelExtractionStartedEvent extends DaemonEventBase {
  type: "ModelExtractionStarted";
  data: { model_id: string };
}

export interface ModelExtractionCompletedEvent extends DaemonEventBase {
  type: "ModelExtractionCompleted";
  data: { model_id: string };
}

export interface ModelExtractionFailedEvent extends DaemonEventBase {
  type: "ModelExtractionFailed";
  data: { model_id: string; error: string };
}

export interface LowMicrophoneVolumeEvent extends DaemonEventBase {
  type: "LowMicrophoneVolume";
  data: { energy: number };
}

export interface ActiveTranscriberChangedEvent extends DaemonEventBase {
  type: "ActiveTranscriberChanged";
  data: { backend: string };
}

export interface TranscriptionFailedEvent extends DaemonEventBase {
  type: "TranscriptionFailed";
  data: { kind: string; message: string };
}

export interface SttFallbackEngagedEvent extends DaemonEventBase {
  type: "SttFallbackEngaged";
  data: { model: string };
}

export interface VoiceEnrollmentStartedEvent extends DaemonEventBase {
  type: "VoiceEnrollmentStarted";
  data: { step: string };
}

export interface VoiceEnrollmentLevelEvent extends DaemonEventBase {
  type: "VoiceEnrollmentLevel";
  data: { energy: number; voiced_ms: number; target_ms: number };
}

export interface VoiceEnrollmentSampleCapturedEvent extends DaemonEventBase {
  type: "VoiceEnrollmentSampleCaptured";
  data: { step: string };
}

export interface VoiceEnrollmentFailedEvent extends DaemonEventBase {
  type: "VoiceEnrollmentFailed";
  data: { step: string; message: string };
}

export interface VoiceProfileStatusEvent extends DaemonEventBase {
  type: "VoiceProfileStatus";
  data: { enrolled: boolean; enabled: boolean; steps: string[] };
}

export interface ShortcutListenerStatusEvent extends DaemonEventBase {
  type: "ShortcutListenerStatus";
  data: { input_monitoring_granted: boolean };
}

export type DaemonEvent =
  | HelloEvent
  | TranscriptChunkEvent
  | TranscriptFinalEvent
  | TranscriptionInterimEvent
  | GrammarCorrectedEvent
  | TranscriptionFilteredEvent
  | CorrectionLoggedEvent
  | CorrectionPendingEvent
  | CorrectionCaptureResultEvent
  | AutoEnterCountdownStartedEvent
  | AutoEnterCountdownTickEvent
  | IdleAutoPausedEvent
  | FocusedAppChangedEvent
  | MasterPauseChangedEvent
  | PauseScopeToggledEvent
  | LongRecordingWarningEvent
  | PauseSourceChangedEvent
  | ResumedAppsChangedEvent
  | CorrectionDialogRequestedEvent
  | ModelDownloadProgressEvent
  | ModelDownloadCompleteEvent
  | ModelDownloadCancelledEvent
  | ModelDownloadFailedEvent
  | ModelVerificationStartedEvent
  | ModelVerificationCompletedEvent
  | ModelExtractionStartedEvent
  | ModelExtractionCompletedEvent
  | ModelExtractionFailedEvent
  | LowMicrophoneVolumeEvent
  | ActiveTranscriberChangedEvent
  | TranscriptionFailedEvent
  | SttFallbackEngagedEvent
  | VoiceEnrollmentStartedEvent
  | VoiceEnrollmentLevelEvent
  | VoiceEnrollmentSampleCapturedEvent
  | VoiceEnrollmentFailedEvent
  | VoiceProfileStatusEvent
  | ShortcutListenerStatusEvent
  // ── Unit events (no data) ──
  | { type: "ListeningStarted"; data: null }
  | { type: "ListeningStopped"; data: null }
  | { type: "ProcessingStarted"; data: null }
  | { type: "ProcessingStopped"; data: null }
  | { type: "TranscribingStarted"; data: null }
  | { type: "TranscribingStopped"; data: null }
  | { type: "Paused"; data: null }
  | { type: "Resumed"; data: null }
  | { type: "PausedQuietly"; data: null }
  | { type: "ResumedQuietly"; data: null }
  | { type: "AutoEnterEnabled"; data: null }
  | { type: "AutoEnterDisabled"; data: null }
  | { type: "AutoEnterCountdownCancelled"; data: null }
  | { type: "AutoEnterCountdownFinished"; data: null }
  | { type: "IdleAutoResumed"; data: null }
  | { type: "Heartbeat"; data: null };

// ─── Events without data (unit events) ─────────────────────────────────────

type UnitEventType =
  | "ListeningStarted"
  | "ListeningStopped"
  | "ProcessingStarted"
  | "ProcessingStopped"
  | "TranscribingStarted"
  | "TranscribingStopped"
  | "Paused"
  | "Resumed"
  | "PausedQuietly"
  | "ResumedQuietly"
  | "AutoEnterEnabled"
  | "AutoEnterDisabled"
  | "VoiceActivityDetected"
  | "VoiceActivityEnded"
  | "AutoEnterCountdownCancelled"
  | "AutoEnterCountdownFinished"
  | "IdleAutoResumed"
  | "Heartbeat";

// ─── DaemonCommand ─────────────────────────────────────────────────────────

export type DaemonCommandType =
  | "TogglePause"
  | "ToggleAutoEnter"
  | "SetAutoEnter"
  | "SetConsumeMode"
  | "ApplyRuntimePreferences"
  | "ApproveCorrection"
  | "RejectCorrection"
  | "CaptureCorrection"
  | "SetPaused"
  | "CancelAutoEnterCountdown"
  | "NotifyFocusedAppChanged"
  | "NotifySystemAudioState"
  | "LogCorrection"
  | "SetAppPaused"
  | "ListModels"
  | "DownloadModel"
  | "CancelModelDownload"
  | "DeleteModel"
  | "SetActiveTranscriber"
  | "SetLanguage"
  | "StartVoiceEnrollment"
  | "CancelVoiceEnrollment"
  | "DeleteVoiceProfile"
  | "SetVoiceProfileEnabled"
  | "GetVoiceProfileStatus"
  | "RespawnRecorder"
  | "ReloadShortcuts";

export interface TogglePauseCommand {
  type: "TogglePause";
}

export interface ToggleAutoEnterCommand {
  type: "ToggleAutoEnter";
}

export interface SetAutoEnterCommand {
  type: "SetAutoEnter";
  data: { enabled: boolean };
}

export interface SetConsumeModeCommand {
  type: "SetConsumeMode";
  data: { enabled: boolean };
}

export interface ApplyRuntimePreferencesCommand {
  type: "ApplyRuntimePreferences";
  data: {
    auto_enter_delay_ms: number;
    energy_threshold: number;
    silence_secs: number;
    cooldown_ms: number;
    silero_threshold: number;
    adaptive_silence?: boolean;
    audible_status_sound?: string;
    stt_live_preview?: boolean;
  };
}

export interface ApproveCorrectionCommand {
  type: "ApproveCorrection";
  data: { id: string };
}

export interface RejectCorrectionCommand {
  type: "RejectCorrection";
  data: { id: string };
}

export interface CaptureCorrectionCommand {
  type: "CaptureCorrection";
}

export interface SetPausedCommand {
  type: "SetPaused";
  data: { paused: boolean; reason?: string };
}

export interface CancelAutoEnterCountdownCommand {
  type: "CancelAutoEnterCountdown";
}

export interface LogCorrectionCommand {
  type: "LogCorrection";
  data: { intended: string };
}

export interface ListModelsCommand {
  type: "ListModels";
}

export interface DownloadModelCommand {
  type: "DownloadModel";
  data: { model_id: string };
}

export interface CancelModelDownloadCommand {
  type: "CancelModelDownload";
  data: { model_id: string };
}

export interface DeleteModelCommand {
  type: "DeleteModel";
  data: { model_id: string };
}

export interface SetActiveTranscriberCommand {
  type: "SetActiveTranscriber";
  data: { backend: string };
}

export interface SetLanguageCommand {
  type: "SetLanguage";
  data: { lang: string };
}

export interface StartVoiceEnrollmentCommand {
  type: "StartVoiceEnrollment";
  data: { step: string };
}

export interface CancelVoiceEnrollmentCommand {
  type: "CancelVoiceEnrollment";
}

export interface DeleteVoiceProfileCommand {
  type: "DeleteVoiceProfile";
}

export interface SetVoiceProfileEnabledCommand {
  type: "SetVoiceProfileEnabled";
  data: { enabled: boolean };
}

export interface GetVoiceProfileStatusCommand {
  type: "GetVoiceProfileStatus";
}

export type DaemonCommand =
  | TogglePauseCommand
  | ToggleAutoEnterCommand
  | SetAutoEnterCommand
  | SetConsumeModeCommand
  | ApplyRuntimePreferencesCommand
  | ApproveCorrectionCommand
  | RejectCorrectionCommand
  | CaptureCorrectionCommand
  | SetPausedCommand
  | CancelAutoEnterCountdownCommand
  | LogCorrectionCommand
  | ListModelsCommand
  | DownloadModelCommand
  | CancelModelDownloadCommand
  | DeleteModelCommand
  | SetActiveTranscriberCommand
  | SetLanguageCommand
  | StartVoiceEnrollmentCommand
  | CancelVoiceEnrollmentCommand
  | DeleteVoiceProfileCommand
  | SetVoiceProfileEnabledCommand
  | GetVoiceProfileStatusCommand;

// ─── Helpers ───────────────────────────────────────────────────────────────

export function serializeCommand(cmd: DaemonCommand): string {
  return JSON.stringify(cmd) + "\n";
}

export interface ModelInfo {
  model_id: string;
  name: string;
  description: string;
  is_downloaded: boolean;
  is_downloading: boolean;
  download_percentage?: number;
  size_bytes?: number;
}

// ─── Daemon State ──────────────────────────────────────────────────────────

export type DaemonStateType =
  | "Idle"
  | "Listening"
  | "Processing"
  | "Transcribing"
  | "Paused"
  | "Error";

export interface DaemonState {
  state: DaemonStateType;
  isPaused: boolean;
  isMasterPaused: boolean;
  masterPaused: boolean;
  isAutoEnter: boolean;
  currentTranscript: string;
  interimText: string;
  backend: string;
  focusedApp: string | null;
  enrolled: boolean;
  voiceEnabled: boolean;
  voiceSteps: string[];
  inputMonitoringGranted: boolean;
  resumedApps: string[];
  pauseSource: string | null;
  correctionPending: CorrectionPendingData | null;
  models: ModelInfo[];
  recentTranscripts: RecentEntry[];
}

export interface CorrectionPendingData {
  id: string;
  wrong: string;
  right: string;
  lastTranscript: string;
}

export interface RecentEntry {
  text: string;
  timestamp: number;
}

export const ENROLLMENT_STEPS = ["normal", "lower", "louder"] as const;
export type EnrollmentStep = (typeof ENROLLMENT_STEPS)[number];

export type SensitivityPreset = "high" | "normal" | "low";