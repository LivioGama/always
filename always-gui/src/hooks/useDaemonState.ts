import { useState, useCallback, useRef, useEffect } from "react";
import {
  DaemonState,
  DaemonStateType,
  DaemonEvent,
  ModelInfo,
  CorrectionPendingData,
  RecentEntry,
  ENROLLMENT_STEPS,
  EnrollmentStep,
} from "../types/uds";

function createInitialState(): DaemonState {
  return {
    state: "Idle",
    isPaused: false,
    isMasterPaused: false,
    masterPaused: false,
    isAutoEnter: true,
    currentTranscript: "",
    interimText: "",
    backend: "groq",
    focusedApp: null,
    enrolled: false,
    voiceEnabled: false,
    voiceSteps: [],
    inputMonitoringGranted: false,
    resumedApps: [],
    pauseSource: null,
    correctionPending: null,
    models: [],
    recentTranscripts: [],
  };
}

// Process a raw event (from Tauri IPC) into a structured DaemonEvent
function parseEvent(raw: any): DaemonEvent | null {
  if (!raw || typeof raw !== "string") return null;
  try {
    const parsed = JSON.parse(raw);
    if (!parsed || !parsed.type) return null;
    // Strip wrapping — Tauri may add extra keys
    return {
      type: parsed.type,
      data: parsed.data ?? null,
    } as DaemonEvent;
  } catch {
    return null;
  }
}

export function useDaemonState() {
  const [state, setState] = useState<DaemonState>(createInitialState);
  const transcriptBuffer = useRef("");
  const pendingCorrections = useRef<Map<string, CorrectionPendingData>>(new Map());
  const [enrollmentStep, setEnrollmentStep] = useState<number>(0);
  const [enrollmentProgress, setEnrollmentProgress] = useState<{
    voiced_ms: number;
    target_ms: number;
    energy: number;
  }>({ voiced_ms: 0, target_ms: 0, energy: 0 });

  const processEvent = useCallback((rawEvent: any) => {
    const event = parseEvent(rawEvent);
    if (!event) return;

    setState((prev) => {
      const next = { ...prev };
      const evt = event as any;

      switch (evt.type) {
        // ── Lifecycle ──
        case "ListeningStarted":
          next.state = "Listening";
          break;
        case "ListeningStopped":
          next.state = "Idle";
          break;
        case "ProcessingStarted":
          next.state = "Processing";
          break;
        case "ProcessingStopped":
          next.state = "Transcribing";
          break;
        case "TranscribingStarted":
          next.state = "Transcribing";
          break;
        case "TranscribingStopped":
          if (next.currentTranscript) {
            next.state = "Idle";
          } else {
            next.state = "Listening";
          }
          break;

        // ── Pauses ──
        case "Paused":
        case "PausedQuietly":
          next.isPaused = true;
          next.state = "Paused";
          break;
        case "Resumed":
        case "ResumedQuietly":
          next.isPaused = false;
          next.state = "Listening";
          break;
        case "MasterPauseChanged":
          next.isMasterPaused = evt.data?.master_paused;
          next.masterPaused = evt.data?.master_paused;
          break;
        case "PauseSourceChanged":
          next.pauseSource = evt.data?.paused ? evt.data?.source : null;
          break;

        // ── Transcripts ──
        case "TranscriptChunk":
        case "TranscriptionInterim":
          next.interimText = evt.data?.text;
          break;
        case "TranscriptFinal": {
          const text = evt.data?.text;
          transcriptBuffer.current = text;
          next.currentTranscript = text;
          next.interimText = "";
          next.state = "Idle";
          // Add to recent transcripts
          const recent: RecentEntry = { text, timestamp: Date.now() };
          next.recentTranscripts = [recent, ...prev.recentTranscripts].slice(0, 50);
          break;
        }

        // ── Auto-Enter ──
        case "AutoEnterEnabled":
          next.isAutoEnter = true;
          break;
        case "AutoEnterDisabled":
          next.isAutoEnter = false;
          break;
        case "AutoEnterCountdownStarted":
          break;
        case "AutoEnterCountdownTick":
          break;
        case "AutoEnterCountdownCancelled":
        case "AutoEnterCountdownFinished":
          break;

        // ── Voice Profile ──
        case "VoiceProfileStatus":
          next.enrolled = evt.data?.enrolled;
          next.voiceEnabled = evt.data?.enabled;
          next.voiceSteps = evt.data?.steps ?? [];
          break;
        case "VoiceEnrollmentStarted":
          setEnrollmentProgress({ voiced_ms: 0, target_ms: 0, energy: 0 });
          break;
        case "VoiceEnrollmentLevel":
          setEnrollmentProgress({
            voiced_ms: evt.data?.voiced_ms,
            target_ms: evt.data?.target_ms,
            energy: evt.data?.energy,
          });
          break;
        case "VoiceEnrollmentSampleCaptured": {
          const currentStep = ENROLLMENT_STEPS.indexOf(evt.data?.step as any);
          if (currentStep >= 0 && currentStep < ENROLLMENT_STEPS.length - 1) {
            setEnrollmentStep(currentStep + 1);
          }
          break;
        }

        // ── Correction ──
        case "CorrectionPending": {
          const data: CorrectionPendingData = {
            id: evt.data?.id,
            wrong: evt.data?.wrong,
            right: evt.data?.right,
            lastTranscript: prev.currentTranscript,
          };
          pendingCorrections.current.set(evt.data?.id, data);
          next.correctionPending = data;
          break;
        }

        // ── Active Backend ──
        case "ActiveTranscriberChanged":
          next.backend = evt.data?.backend;
          break;

        // ── Focused App ──
        case "FocusedAppChanged":
          next.focusedApp = evt.data?.bundle_id;
          break;

        // ── Resume Apps ──
        case "ResumedAppsChanged":
          next.resumedApps = evt.data?.bundles ?? [];
          break;

        // ── Shortcut Listener ──
        case "ShortcutListenerStatus":
          next.inputMonitoringGranted = evt.data?.input_monitoring_granted ?? false;
          break;

        // ── Models ──
        case "ModelsList": {
          const models: ModelInfo[] = (evt.data?.models ?? []).map(
            (m: any) => ({
              model_id: m.model_id,
              name: m.name ?? m.model_id,
              description: m.description ?? "",
              is_downloaded: m.is_downloaded ?? false,
              is_downloading: m.is_downloading ?? false,
              download_percentage: m.percentage,
              size_bytes: m.size_bytes,
            }),
          );
          next.models = models;
          break;
        }
        case "ModelDownloadProgress":
          next.models = next.models.map((m) =>
            m.model_id === evt.data?.model_id
              ? { ...m, is_downloading: true, download_percentage: evt.data?.percentage }
              : m,
          );
          break;
        case "ModelDownloadComplete":
          next.models = next.models.map((m) =>
            m.model_id === evt.data?.model_id
              ? { ...m, is_downloading: false, is_downloaded: true }
              : m,
          );
          break;
        case "ModelDownloadCancelled":
        case "ModelDownloadFailed":
          next.models = next.models.map((m) =>
            m.model_id === evt.data?.model_id
              ? { ...m, is_downloading: false }
              : m,
          );
          break;

        // ── Error ──
        case "TranscriptionFailed":
          next.state = "Error";
          break;

        // ── Idle Auto Pause ──
        case "IdleAutoPaused":
          next.isPaused = true;
          next.state = "Paused";
          break;
        case "IdleAutoResumed":
          next.isPaused = false;
          next.state = "Listening";
          break;
      }

      return next;
    });
  }, []);

  const sendCommand = useCallback((cmd: string) => {
    const tauri = (window as any).__TAURI__;
    if (tauri?.core?.send) {
      tauri.core.send("uds_send", { data: cmd });
    }
  }, []);

  const togglePause = useCallback(() => {
    sendCommand(JSON.stringify({ type: "TogglePause" }));
  }, [sendCommand]);

  const toggleAutoEnter = useCallback(() => {
    sendCommand(JSON.stringify({ type: "ToggleAutoEnter" }));
  }, [sendCommand]);

  const setAutoEnter = useCallback(
    (enabled: boolean) => {
      sendCommand(JSON.stringify({ type: "SetAutoEnter", data: { enabled } }));
    },
    [sendCommand],
  );

  const setConsumeMode = useCallback(
    (enabled: boolean) => {
      sendCommand(
        JSON.stringify({ type: "SetConsumeMode", data: { enabled } }),
      );
    },
    [sendCommand],
  );

  const applyRuntimePreferences = useCallback(
    (prefs: Partial<{
      auto_enter_delay_ms: number;
      energy_threshold: number;
      silence_secs: number;
      cooldown_ms: number;
      silero_threshold: number;
      adaptive_silence: boolean;
      stt_live_preview: boolean;
    }>) => {
      sendCommand(
        JSON.stringify({
          type: "ApplyRuntimePreferences",
          data: {
            auto_enter_delay_ms: prefs.auto_enter_delay_ms ?? 4000,
            energy_threshold: prefs.energy_threshold ?? 0.012,
            silence_secs: prefs.silence_secs ?? 0.9,
            cooldown_ms: prefs.cooldown_ms ?? 800,
            silero_threshold: prefs.silero_threshold ?? 0.5,
            adaptive_silence: prefs.adaptive_silence,
            stt_live_preview: prefs.stt_live_preview,
          },
        }),
      );
    },
    [sendCommand],
  );

  const approveCorrection = useCallback(
    (id: string) => {
      sendCommand(JSON.stringify({ type: "ApproveCorrection", data: { id } }));
    },
    [sendCommand],
  );

  const rejectCorrection = useCallback(
    (id: string) => {
      sendCommand(JSON.stringify({ type: "RejectCorrection", data: { id } }));
    },
    [sendCommand],
  );

  const setActiveTranscriber = useCallback(
    (backend: string) => {
      sendCommand(
        JSON.stringify({ type: "SetActiveTranscriber", data: { backend } }),
      );
    },
    [sendCommand],
  );

  const setLanguage = useCallback(
    (lang: string) => {
      sendCommand(JSON.stringify({ type: "SetLanguage", data: { lang } }));
    },
    [sendCommand],
  );

  const startVoiceEnrollment = useCallback(
    (step: string) => {
      sendCommand(
        JSON.stringify({ type: "StartVoiceEnrollment", data: { step } }),
      );
    },
    [sendCommand],
  );

  const cancelVoiceEnrollment = useCallback(() => {
    sendCommand(JSON.stringify({ type: "CancelVoiceEnrollment" }));
  }, [sendCommand]);

  const deleteVoiceProfile = useCallback(() => {
    sendCommand(JSON.stringify({ type: "DeleteVoiceProfile" }));
  }, [sendCommand]);

  const setVoiceProfileEnabled = useCallback(
    (enabled: boolean) => {
      sendCommand(
        JSON.stringify({
          type: "SetVoiceProfileEnabled",
          data: { enabled },
        }),
      );
    },
    [sendCommand],
  );

  const downloadModel = useCallback(
    (modelId: string) => {
      sendCommand(
        JSON.stringify({ type: "DownloadModel", data: { model_id: modelId } }),
      );
    },
    [sendCommand],
  );

  const deleteModel = useCallback(
    (modelId: string) => {
      sendCommand(
        JSON.stringify({ type: "DeleteModel", data: { model_id: modelId } }),
      );
    },
    [sendCommand],
  );

  return {
    state,
    enrollmentStep,
    enrollmentProgress,
    processEvent,
    togglePause,
    toggleAutoEnter,
    setAutoEnter,
    setConsumeMode,
    applyRuntimePreferences,
    approveCorrection,
    rejectCorrection,
    setActiveTranscriber,
    setLanguage,
    startVoiceEnrollment,
    cancelVoiceEnrollment,
    deleteVoiceProfile,
    setVoiceProfileEnabled,
    downloadModel,
    deleteModel,
  };
}