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
  | { kind: "failed"; stage?: string; message: string };

/// Newest release known to be newer than this build, tracked separately
/// from the transient `update` state — mirrors `UpdateModel.offer` in
/// Always/Sources/Always/Models/UpdateState.swift. It survives a failed
/// install (so "Try again" works) and a failed re-check (so a known update
/// is never hidden), and is cleared by an up-to-date / no-release answer.
export interface UpdateOffer {
  current: string;
  latest: string;
  url: string;
  canApply: boolean;
  applyHint: string;
}

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
  downloadBytes: Record<string, { downloaded: number; total: number }>;
  verifying: string[];
  extracting: string[];
  installing: string[];
  modelErrors: Record<string, string>;
  loadingBackend: string | null;
  recentTranscripts: { text: string; timestamp: number }[];
  update: UpdateState;
  updateOffer: UpdateOffer | null;
}

/// Humanized `UpdateCheckDone` failure reasons — same text as
/// `UpdateModel.reason(for:)` so the About card reads like the Swift one.
function checkFailedReason(result: string): string {
  if (result === "offline") return "you appear to be offline";
  if (result === "rate_limited") return "GitHub rate limit reached, try again later";
  return "unexpected error";
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
    backend: "none",
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
    downloadBytes: {},
    verifying: [],
    extracting: [],
    installing: [],
    modelErrors: {},
    loadingBackend: null,
    recentTranscripts: [],
    update: { kind: "unknown" },
    updateOffer: null,
  };
}

export function useDaemonState() {
  const [state, setState] = useState<DaemonUiState>(initialState);
  const backendSwitchAttempt = useRef(0);
  const checkAttempt = useRef(0);
  const handoffAttempt = useRef(0);

  const setConnected = useCallback((v: boolean) => {
    setState((prev) => {
      if (v) return { ...prev, isDaemonConnected: true };
      // Mirrors `UpdateModel.daemonDisconnected()`: a pending check is
      // forgotten, an install that had not handed off to the installer is
      // a failure, and the handoff itself (phase "installing") is
      // EXPECTED — the installer quits this app and the daemon.
      const u = prev.update;
      let update = u;
      if (u.kind === "checking") update = { kind: "unknown" };
      else if (u.kind === "installing" && u.phase !== "installing") {
        update = {
          kind: "failed",
          stage: "install",
          message: "Lost the connection to the Always daemon during the update.",
        };
      }
      return { ...prev, isDaemonConnected: false, update };
    });
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
          backendSwitchAttempt.current += 1;
          break;
        case "ActiveTranscriberFailed": {
          const backend = String(d.backend ?? "");
          const errorKey = backend.startsWith("local:") ? backend.slice(6) : backend;
          next.modelErrors = { ...prev.modelErrors, [errorKey]: d.error ?? "Couldn't load this transcription model." };
          next.loadingBackend = null;
          backendSwitchAttempt.current += 1;
          break;
        }

        case "ModelsList": {
          const models = (d.models ?? []).map((m: any) => ({
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
          next.models = models;
          const downloadedIds = new Set<string>(models.filter((m: CatalogModel) => m.is_downloaded).map((m: CatalogModel) => m.id));
          next.installing = prev.installing.filter((id) => !downloadedIds.has(id));
          next.extracting = prev.extracting.filter((id) => !downloadedIds.has(id));
          next.verifying = prev.verifying.filter((id) => !downloadedIds.has(id));
          if (downloadedIds.size) {
            const progress = { ...prev.downloadProgress };
            const byteCounts = { ...prev.downloadBytes };
            for (const id of downloadedIds) delete progress[id];
            for (const id of downloadedIds) delete byteCounts[id];
            next.downloadProgress = progress;
            next.downloadBytes = byteCounts;
            const errors = { ...prev.modelErrors };
            for (const id of downloadedIds) delete errors[id];
            next.modelErrors = errors;
          }
          break;
        }
        case "ModelDownloadProgress": {
          const progress = { ...prev.downloadProgress };
          const bytes = { ...prev.downloadBytes };
          if ((d.percentage ?? 0) >= 100) {
            delete progress[d.model_id];
            delete bytes[d.model_id];
            next.installing = [...new Set([...prev.installing, d.model_id])];
          } else {
            progress[d.model_id] = d.percentage ?? 0;
            bytes[d.model_id] = { downloaded: d.downloaded ?? 0, total: d.total ?? 0 };
          }
          next.downloadProgress = progress;
          next.downloadBytes = bytes;
          break;
        }
        case "ModelDownloadComplete": {
          const p = { ...prev.downloadProgress };
          delete p[d.model_id];
          next.downloadProgress = p;
          const b = { ...prev.downloadBytes };
          delete b[d.model_id];
          next.downloadBytes = b;
          next.installing = [...new Set([...prev.installing, d.model_id])];
          sendDaemonCommand({ type: "ListModels" }).catch(() => {});
          break;
        }
        case "ModelDownloadCancelled": {
          const p = { ...prev.downloadProgress };
          delete p[d.model_id];
          next.downloadProgress = p;
          const b = { ...prev.downloadBytes };
          delete b[d.model_id];
          next.downloadBytes = b;
          next.installing = prev.installing.filter((id) => id !== d.model_id);
          next.extracting = prev.extracting.filter((id) => id !== d.model_id);
          next.verifying = prev.verifying.filter((id) => id !== d.model_id);
          break;
        }
        case "ModelDownloadFailed": {
          const p = { ...prev.downloadProgress };
          delete p[d.model_id];
          next.downloadProgress = p;
          const b = { ...prev.downloadBytes };
          delete b[d.model_id];
          next.downloadBytes = b;
          next.installing = prev.installing.filter((id) => id !== d.model_id);
          next.extracting = prev.extracting.filter((id) => id !== d.model_id);
          next.verifying = prev.verifying.filter((id) => id !== d.model_id);
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
          next.installing = prev.installing.filter((id) => id !== d.model_id);
          next.modelErrors = { ...prev.modelErrors, [d.model_id]: d.error ?? "Extraction failed" };
          break;

        // ── Updates (SPEC §12a) ──
        // Same reduction as UpdateModel.apply: the offer is tracked separately
        // from the visible state so re-announcements and failed re-checks
        // never hide a known update, and a live download is never flipped
        // back to the offer card.
        case "UpdateAvailable": {
          const latest = String(d.latest ?? "");
          if (!latest) break; // nothing showable
          const offer: UpdateOffer = {
            current: String(d.current ?? ""),
            latest,
            url: String(d.url ?? ""),
            canApply: String(d.can_apply ?? "") === "true",
            applyHint: String(d.apply_hint ?? ""),
          };
          next.updateOffer = offer;
          const u = prev.update;
          if (u.kind === "installing" && u.phase !== "installing") break;
          next.update = { kind: "available", ...offer };
          break;
        }
        case "UpdateCheckDone": {
          const r = String(d.result ?? "error");
          const current = String(d.current ?? "");
          const latest = String(d.latest ?? "");
          const offer = prev.updateOffer;
          if (prev.update.kind === "installing") break; // an install owns the state
          if (r === "up_to_date") {
            next.updateOffer = null;
            next.update = { kind: "upToDate", version: current };
          } else if (r === "no_release") {
            next.updateOffer = null;
            next.update = { kind: "noRelease" };
          } else if (r === "update_available") {
            // An UpdateAvailable follows; until then keep the current
            // state, or confirm the exact release we already know.
            if (offer && (latest === "" || offer.latest === latest)) {
              next.update = { kind: "available", ...offer };
            }
          } else if (offer) {
            // A failed re-check must not hide a known update.
            next.update = { kind: "available", ...offer };
          } else {
            next.update = { kind: "checkFailed", reason: checkFailedReason(r) };
          }
          break;
        }
        case "UpdateProgress": {
          const phase = String(d.phase ?? "");
          const percent =
            d.percent === "" || d.percent == null
              ? null
              : Math.min(100, Math.max(0, Number(d.percent)));
          if (phase !== "downloading" && phase !== "verifying" && phase !== "installing") break;
          next.update = {
            kind: "installing",
            phase,
            percent,
            detail: d.detail ?? "",
          };
          break;
        }
        case "UpdateFailed": {
          const stage = String(d.stage ?? "");
          const message = String(d.message ?? "");
          next.update = {
            kind: "failed",
            stage,
            message: message || "The update could not be installed.",
          };
          break;
        }

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
          live_typing: prefs.live_typing,
          spoken_enter: prefs.spoken_enter,
          spoken_enter_phrases: prefs.spoken_enter_phrases,
          spoken_enter_at_end: prefs.spoken_enter_at_end,
        },
      }),
    reloadShortcuts: () => cmd({ type: "ReloadShortcuts" }),
    setupMode: (enabled: boolean) => cmd({ type: "SetupMode", data: { enabled } }),

    listModels: () => cmd({ type: "ListModels" }),
    downloadModel: (model_id: string) => cmd({ type: "DownloadModel", data: { model_id } }),
    cancelModelDownload: (model_id: string) => cmd({ type: "CancelModelDownload", data: { model_id } }),
    deleteModel: (model_id: string) => cmd({ type: "DeleteModel", data: { model_id } }),
    setActiveTranscriber: (backend: string) => {
      const attempt = ++backendSwitchAttempt.current;
      const errorKey = backend.startsWith("local:") ? backend.slice(6) : backend;
      setState((p) => {
        const errors = { ...p.modelErrors };
        delete errors[errorKey];
        if (backend.startsWith("local:")) delete errors.groq;
        return { ...p, modelErrors: errors, loadingBackend: backend };
      });
      cmd({ type: "SetActiveTranscriber", data: { backend } });
      setTimeout(() => {
        if (backendSwitchAttempt.current !== attempt) return;
        setState((p) => p.loadingBackend === backend ? {
          ...p,
          loadingBackend: null,
          modelErrors: { ...p.modelErrors, [errorKey]: "Didn't finish loading. Try again, or pick another model." },
        } : p);
      }, 120_000);
    },
    setLanguage: (lang: string | null) => cmd({ type: "SetLanguage", data: { lang: lang ?? "auto" } }),

    getVoiceProfileStatus: () => cmd({ type: "GetVoiceProfileStatus" }),
    getShortcutListenerStatus: () => cmd({ type: "GetShortcutListenerStatus" }),
    startVoiceEnrollment: (step: string) => cmd({ type: "StartVoiceEnrollment", data: { step } }),
    cancelVoiceEnrollment: () => {
      setState((p) => ({ ...p, enrollment: { ...p.enrollment, recordingStep: null, isPreparing: false } }));
      cmd({ type: "CancelVoiceEnrollment" });
    },
    deleteVoiceProfile: () => cmd({ type: "DeleteVoiceProfile" }),
    setVoiceProfileEnabled: (enabled: boolean) => {
      setState((prev) => ({ ...prev, voiceEnabled: enabled }));
      cmd({ type: "SetVoiceProfileEnabled", data: { enabled } });
    },

    approveCorrection: (id: string) => cmd({ type: "ApproveCorrection", data: { id } }),
    rejectCorrection: (id: string) => cmd({ type: "RejectCorrection", data: { id } }),
    logCorrection: (intended: string) => cmd({ type: "LogCorrection", data: { intended } }),

    checkForUpdates: () => {
      // Mirrors UpdateModel.beginCheck(): a known offer or a live install
      // keeps the screen as is; everything else shows the spinner. The
      // daemon answers in well under a second — a 20 s watchdog turns a
      // silent loss into a visible failure (UpdateService.checkTimeout).
      const attempt = ++checkAttempt.current;
      setState((p) => {
        const u = p.update;
        if (u.kind === "available" || u.kind === "installing") return p;
        return { ...p, update: { kind: "checking" } };
      });
      cmd({ type: "CheckForUpdates" });
      setTimeout(() => {
        if (checkAttempt.current !== attempt) return;
        setState((p) =>
          p.update.kind === "checking"
            ? { ...p, update: { kind: "checkFailed", reason: "no answer from the Always daemon" } }
            : p,
        );
      }, 20_000);
    },
    installUpdate: () => {
      // Mirrors UpdateModel.beginInstall(): only when the daemon said this
      // install can apply, and only once. The percent-less download frame
      // shows until UpdateProgress supplies one.
      const attempt = ++handoffAttempt.current;
      setState((p) => {
        const offer = p.updateOffer;
        if (!offer || !offer.canApply) return p;
        if (p.update.kind === "installing") return p;
        return {
          ...p,
          update: { kind: "installing", phase: "downloading", percent: null, detail: "" },
        };
      });
      cmd({ type: "InstallUpdate" });
      // The installer's handoff (phase "installing") quits this app and the
      // daemon, so the UDS connection ending from there on is EXPECTED. If
      // it never actually arrives (UpdateService.handoffTimeout), say so —
      // the update-install.log holds the rest.
      setTimeout(() => {
        if (handoffAttempt.current !== attempt) return;
        setState((p) => {
          const u = p.update;
          if (u.kind !== "installing" || u.phase !== "installing") return p;
          return {
            ...p,
            update: {
              kind: "failed",
              stage: "install",
              message: "The installer did not restart Always. See ~/Library/Logs/Always/update-install.log.",
            },
          };
        });
      }, 120_000);
    },
  };

  return api;
}

export type DaemonApi = ReturnType<typeof useDaemonState>;

export const ENROLLMENT_STEPS = [
  { key: "normal", title: "Normal voice", prompt: "Read a few sentences at your usual speaking volume." },
  { key: "lower", title: "Quieter voice", prompt: "Repeat, a little softer — like talking late at night." },
  { key: "louder", title: "Louder voice", prompt: "Once more, louder — like calling across a room." },
];
