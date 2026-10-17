import { useState, useCallback, useRef } from "react";
import { sendDaemonCommand } from "../lib/tauri";

// ── Daemon state (mirror of Swift StateMonitor's observable surface) ────────

export interface CatalogModel {
  id: string;
  name: string;
  description: string;
  is_downloaded: boolean;
  is_downloading: boolean;
  partial_size: number;
  size_mb: number;
  accuracy_score: number;
  speed_score: number;
  supports_translation: boolean;
  supports_streaming: boolean;
  supportsMultipleLanguages: boolean;
  is_recommended: boolean;
  supported_languages: string[];
  supports_language_selection: boolean;
  is_verifying: boolean;
}

export type UpdateState =
  | { kind: "unknown" }
  | { kind: "checking" }
  | { kind: "upToDate"; version: string }
  | { kind: "noRelease" }
  | { kind: "checkFailed"; reason: string }
  | { kind: "available"; current: string; latest: string; url: string; canApply: boolean; applyHint: string }
  | { kind: "installing"; phase: string; percent: number | null; detail: string }
  | { kind: "failed"; message: string };

export interface VoiceEnrollment {
  recordingStep: string | null;
  level: number;
  voicedMs: number;
  targetMs: number;
  isPreparing: boolean;
  lastError: string | null;
}

export interface DaemonUiState {
  state: string;
  isPaused: boolean;
  isMasterPaused: boolean;
  isIdleAutoPaused: boolean;
  isAutoEnter: boolean;
  isDaemonConnected: boolean;
  currentTranscript: string;
  interimText: string;
  backend: string;          // active backend wire form ("groq", "apple", "local:<id>")
  focusedApp: string | null;
  enrolled: boolean;
  voiceEnabled: boolean;
  voiceSteps: string[];
  enrollment: VoiceEnrollment;
  inputMonitoringGranted: boolean;
  resumedApps: string[];
  pauseSource: string | null;
  correctionPending: { id: string; wrong: string; right: string } | null;
  models: CatalogModel[];
  downloadProgress: Record<string, number>;
  verifying: string[];
  extracting: string[];
  installing: string[];
  modelErrors: Record<string, string>;
  loadingBackend: string | null;
  recentTranscripts: { text: string; timestamp: number }[];
  update: UpdateState;
}

function initialState(): DaemonUiState {
  return {
    state: "Idle",
    isPaused: false,
    isMasterPaused: false,
    isIdleAutoPaused: false,
    isAutoEnter: true,
    isDaemonConnected: false,
    currentTranscript: "",
    interimText: "",
    backend: "groq",
    focusedApp: null,
    enrolled: false,
    voiceEnabled: false,
    voiceSteps: [],
    enrollment: { recordingStep: null, level: 0, voicedMs: 0, targetMs: 0, isPreparing: false, lastError: null },
    inputMonitoringGranted: false,
    resumedApps: [],
    pauseSource: null,
    correctionPending: null,
    models: [],
    downloadProgress: {},
    verifying: [],
    extracting: [],
    installing: [],
    modelErrors: {},
    loadingBackend: null,
    recentTranscripts: [],
    update: { kind: "unknown" },
  };
}

export function useDaemonState() {
  const [state, setState] = useState<DaemonUiState>(initialState);

  const setConnected = useCallback((v: boolean) => {
    setState((prev) => ({ ...prev, isDaemonConnected: v }));
  }, []);

  // Process one raw JSON event line from the daemon.
  const processEvent = useCallback((raw: string) => {
    let evt: any;
    try {
      evt = JSON.parse(raw);
    } catch {
      return;
    }
    if (!evt?.type) return;
    const d = evt.data ?? {};

    setState((prev) => {
      const next: DaemonUiState = { ...prev, enrollment: { ...prev.enrollment } };
      switch (evt.type) {
        case "ListeningStarted": next.state = "Listening"; break;
        case "ListeningStopped": next.state = "Idle"; break;
        case "ProcessingStarted": next.state = "Processing"; break;
        case "ProcessingStopped":
        case "TranscribingStarted": next.state = "Transcribing"; break;
        case "TranscribingStopped":
          next.state = next.currentTranscript ? "Idle" : "Listening";
          break;
        case "Paused":
        case "PausedQuietly":
          next.isPaused = true; next.state = "Paused"; break;
        case "Resumed":
        case "ResumedQuietly":
          next.isPaused = false; next.state = "Listening"; break;
        case "MasterPauseChanged":
          next.isMasterPaused = !!d.master_paused; break;
        case "PauseSourceChanged":
          next.pauseSource = d.paused ? d.source : null; break;
        case "IdleAutoPaused":
          next.isPaused = true; next.isIdleAutoPaused = true; next.state = "Paused"; break;
        case "IdleAutoResumed":
          next.isPaused = false; next.isIdleAutoPaused = false; next.state = "Listening"; break;

        case "TranscriptChunk":
        case "TranscriptionInterim":
          next.interimText = d.text ?? ""; break;
        case "TranscriptFinal": {
          const text = d.text ?? "";
          next.currentTranscript = text;
          next.interimText = "";
          next.state = "Idle";
          next.recentTranscripts = [{ text, timestamp: Date.now() }, ...prev.recentTranscripts].slice(0, 50);
          break;
        }
        case "AutoEnterEnabled": next.isAutoEnter = true; break;
        case "AutoEnterDisabled": next.isAutoEnter = false; break;

        case "VoiceProfileStatus":
          next.enrolled = !!d.enrolled;
          next.voiceEnabled = !!d.enabled;
          next.voiceSteps = d.steps ?? [];
          break;
        case "VoiceEnrollmentPreparing":
          next.enrollment.isPreparing = true;
          next.enrollment.recordingStep = d.step ?? next.enrollment.recordingStep;
          next.enrollment.lastError = null;
          break;
        case "VoiceEnrollmentStarted":
          next.enrollment.recordingStep = d.step ?? null;
          next.enrollment.isPreparing = false;
          next.enrollment.voicedMs = 0;
          next.enrollment.targetMs = 0;
          next.enrollment.lastError = null;
          break;
        case "VoiceEnrollmentLevel":
          next.enrollment.level = d.energy ?? 0;
          next.enrollment.voicedMs = d.voiced_ms ?? 0;
          next.enrollment.targetMs = d.target_ms ?? 0;
          break;
        case "VoiceEnrollmentSampleCaptured":
          next.enrollment.recordingStep = null;
          next.enrollment.isPreparing = false;
          break;
        case "VoiceEnrollmentFailed":
          next.enrollment.recordingStep = null;
          next.enrollment.isPreparing = false;
          next.enrollment.lastError = d.message ?? "Enrollment failed";
          break;

        case "CorrectionPending":
          next.correctionPending = { id: d.id, wrong: d.wrong, right: d.right };
          break;
        case "FocusedAppChanged": next.focusedApp = d.bundle_id ?? null; break;
        case "ResumedAppsChanged": next.resumedApps = d.bundles ?? []; break;
        case "ShortcutListenerStatus":
          next.inputMonitoringGranted = !!d.input_monitoring_granted; break;
        case "ActiveTranscriberChanged":
          next.backend = d.backend ?? prev.backend;
          next.loadingBackend = null;
          break;

        case "ModelsList": {
          next.models = (d.models ?? []).map((m: any) => ({
            id: m.id,
            name: m.name ?? m.id,
            description: m.description ?? "",
            is_downloaded: !!m.is_downloaded,
            is_downloading: !!m.is_downloading,
            partial_size: m.partial_size ?? 0,
            size_mb: m.size_mb ?? 0,
            accuracy_score: m.accuracy_score ?? 0,
            speed_score: m.speed_score ?? 0,
            supports_translation: !!m.supports_translation,
            supports_streaming: !!m.supports_streaming,
            supportsMultipleLanguages: (m.supported_languages ?? []).length > 1,
            is_recommended: !!m.is_recommended,
            supported_languages: m.supported_languages ?? [],
            supports_language_selection: !!m.supports_language_selection,
            is_verifying: !!m.is_verifying,
          }));
          break;
        }
        case "ModelDownloadProgress":
          next.downloadProgress = { ...prev.downloadProgress, [d.model_id]: d.percentage ?? 0 };
          break;
        case "ModelDownloadComplete":
        case "ModelDownloadCancelled": {
          const p = { ...prev.downloadProgress };
          delete p[d.model_id];
          next.downloadProgress = p;
          break;
        }
        case "ModelDownloadFailed": {
          const p = { ...prev.downloadProgress };
          delete p[d.model_id];
          next.downloadProgress = p;
          next.modelErrors = { ...prev.modelErrors, [d.model_id]: d.error ?? "Download failed" };
          break;
        }
        case "ModelVerificationStarted":
          next.verifying = [...new Set([...prev.verifying, d.model_id])]; break;
        case "ModelVerificationCompleted":
          next.verifying = prev.verifying.filter((id) => id !== d.model_id); break;
        case "ModelExtractionStarted":
          next.extracting = [...new Set([...prev.extracting, d.model_id])]; break;
        case "ModelExtractionCompleted":
          next.extracting = prev.extracting.filter((id) => id !== d.model_id);
          next.installing = [...new Set([...prev.installing, d.model_id])];
          break;
        case "ModelExtractionFailed":
          next.extracting = prev.extracting.filter((id) => id !== d.model_id);
          next.modelErrors = { ...prev.modelErrors, [d.model_id]: d.error ?? "Extraction failed" };
          break;

        // ── Updates (SPEC §12a) ──
        case "UpdateAvailable":
          next.update = {
            kind: "available",
            current: d.current ?? "",
            latest: d.latest ?? "",
            url: d.url ?? "",
            canApply: d.can_apply === "true",
            applyHint: d.apply_hint ?? "",
          };
          break;
        case "UpdateCheckDone": {
          const r = d.result;
          if (r === "up_to_date") next.update = { kind: "upToDate", version: d.current ?? "" };
          else if (r === "no_release") next.update = { kind: "noRelease" };
          else if (r === "update_available") break; // UpdateAvailable event follows
          else next.update = { kind: "checkFailed", reason: r ?? "error" };
          break;
        }
        case "UpdateProgress":
          next.update = {
            kind: "installing",
            phase: d.phase ?? "",
            percent: d.percent === "" || d.percent == null ? null : Number(d.percent),
            detail: d.detail ?? "",
          };
          break;
        case "UpdateFailed":
          next.update = { kind: "failed", message: d.message ?? "Update failed" };
          break;

        case "TranscriptionFailed": next.state = "Error"; break;
      }
      return next;
    });
  }, []);

  // ── Command senders (raw JSON lines through the Tauri bridge) ────────────

  const cmd = useCallback((c: object) => {
    sendDaemonCommand(c).catch(() => {});
  }, []);

  const api = {
    state,
    setConnected,
    processEvent,

    togglePause: () => cmd({ type: "TogglePause" }),
    resume: () => cmd({ type: "Resume" }),
    toggleAutoEnter: () => cmd({ type: "ToggleAutoEnter" }),
    setAutoEnter: (enabled: boolean) => cmd({ type: "SetAutoEnter", data: { enabled } }),
    setPaused: (paused: boolean, reason?: string) => cmd({ type: "SetPaused", data: { paused, reason: reason ?? null } }),
    setAppPaused: (bundle_id: string, paused: boolean | null) => cmd({ type: "SetAppPaused", data: { bundle_id, paused } }),
    applyRuntimePreferences: (prefs: any) =>
      cmd({
        type: "ApplyRuntimePreferences",
        data: {
          auto_enter_delay_ms: prefs.auto_enter_delay_ms ?? 4000,
          energy_threshold: prefs.energy_threshold ?? 0.012,
          silence_secs: prefs.silence_secs ?? 0.9,
          cooldown_ms: prefs.cooldown_ms ?? 150,
          silero_threshold: prefs.silero_threshold ?? 0.5,
          adaptive_silence: prefs.adaptive_silence,
          audible_status_sound: prefs.audible_status_sound,
          stt_live_preview: prefs.stt_live_preview,
        },
      }),
    reloadShortcuts: () => cmd({ type: "ReloadShortcuts" }),
    setupMode: (enabled: boolean) => cmd({ type: "SetupMode", data: { enabled } }),

    listModels: () => cmd({ type: "ListModels" }),
    downloadModel: (model_id: string) => cmd({ type: "DownloadModel", data: { model_id } }),
    cancelModelDownload: (model_id: string) => cmd({ type: "CancelModelDownload", data: { model_id } }),
    deleteModel: (model_id: string) => cmd({ type: "DeleteModel", data: { model_id } }),
    setActiveTranscriber: (backend: string) => {
      setState((p) => ({ ...p, loadingBackend: backend }));
      cmd({ type: "SetActiveTranscriber", data: { backend } });
    },
    setLanguage: (lang: string | null) => cmd({ type: "SetLanguage", data: { lang: lang ?? "auto" } }),

    getVoiceProfileStatus: () => cmd({ type: "GetVoiceProfileStatus" }),
    startVoiceEnrollment: (step: string) => cmd({ type: "StartVoiceEnrollment", data: { step } }),
    cancelVoiceEnrollment: () => {
      setState((p) => ({ ...p, enrollment: { ...p.enrollment, recordingStep: null, isPreparing: false } }));
      cmd({ type: "CancelVoiceEnrollment" });
    },
    deleteVoiceProfile: () => cmd({ type: "DeleteVoiceProfile" }),
    setVoiceProfileEnabled: (enabled: boolean) => cmd({ type: "SetVoiceProfileEnabled", data: { enabled } }),

    approveCorrection: (id: string) => cmd({ type: "ApproveCorrection", data: { id } }),
    rejectCorrection: (id: string) => cmd({ type: "RejectCorrection", data: { id } }),
    logCorrection: (intended: string) => cmd({ type: "LogCorrection", data: { intended } }),

    checkForUpdates: () => {
      setState((p) => ({ ...p, update: { kind: "checking" } }));
      cmd({ type: "CheckForUpdates" });
    },
    installUpdate: () => cmd({ type: "InstallUpdate" }),
  };

  return api;
}

export type DaemonApi = ReturnType<typeof useDaemonState>;

export const ENROLLMENT_STEPS = [
  { key: "normal", title: "Normal voice", prompt: "Read a few sentences at your usual speaking volume." },
  { key: "lower", title: "Quieter voice", prompt: "Repeat, a little softer — like talking late at night." },
  { key: "louder", title: "Louder voice", prompt: "Once more, louder — like calling across a room." },
];
