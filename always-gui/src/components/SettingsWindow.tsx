import { useState, useCallback } from "react";
import {
  DaemonState,
  ModelInfo,
  ENROLLMENT_STEPS,
  SensitivityPreset,
} from "../types/uds";

interface SettingsWindowProps {
  state: DaemonState;
  onTogglePause: () => void;
  setAutoEnter: (enabled: boolean) => void;
  setConsumeMode: (enabled: boolean) => void;
  applyRuntimePreferences: (prefs: Partial<{
    auto_enter_delay_ms: number;
    energy_threshold: number;
    silence_secs: number;
    cooldown_ms: number;
    silero_threshold: number;
    adaptive_silence: boolean;
    stt_live_preview: boolean;
  }>) => void;
  setActiveTranscriber: (backend: string) => void;
  setLanguage: (lang: string) => void;
  startVoiceEnrollment: (step: string) => void;
  cancelVoiceEnrollment: () => void;
  deleteVoiceProfile: () => void;
  setVoiceProfileEnabled: (enabled: boolean) => void;
  downloadModel: (modelId: string) => void;
  deleteModel: (modelId: string) => void;
  onClose: () => void;
}

type Tab =
  | "general"
  | "behavior"
  | "shortcuts"
  | "voice"
  | "models"
  | "history"
  | "permissions"
  | "about";

const TABS: { key: Tab; label: string; icon: JSX.Element }[] = [
  {
    key: "general",
    label: "General",
    icon: (
      <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M3 5h12M9 3v2m1.048 9.5A18.022 18.022 0 016.412 9m6.088 9h7M11 21l5-10 5 10M12.751 5C11.783 10.77 8.07 15.61 3 18.129" />
      </svg>
    ),
  },
  {
    key: "behavior",
    label: "Behavior",
    icon: (
      <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 6V4m0 2a2 2 0 100 4m0-4a2 2 0 110 4m-6 8a2 2 0 100-4m0 4a2 2 0 110-4m0 4v2m0-6V4m6 6v10m6-2a2 2 0 100-4m0 4a2 2 0 110-4m0 4v2m0-6V4" />
      </svg>
    ),
  },
  {
    key: "shortcuts",
    label: "Shortcuts",
    icon: (
      <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M8 9l3 3-3 3m5 0h3M5 20h14a2 2 0 002-2V6a2 2 0 00-2-2H5a2 2 0 00-2 2v12a2 2 0 002 2z" />
      </svg>
    ),
  },
  {
    key: "voice",
    label: "My Voice",
    icon: (
      <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M5.121 17.804A13.937 13.937 0 0112 16c2.5 0 4.847.655 6.879 1.804M15 10a3 3 0 11-6 0 3 3 0 016 0zm6 2a9 9 0 11-18 0 9 9 0 0118 0z" />
      </svg>
    ),
  },
  {
    key: "models",
    label: "Models",
    icon: (
      <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4 7v10c0 2.21 3.582 4 8 4s8-1.79 8-4V7M4 7c0 2.21 3.582 4 8 4s8-1.79 8-4M4 7c0-2.21 3.582-4 8-4s8 1.79 8 4m0 5c0 2.21-3.582 4-8 4s-8-1.79-8-4" />
      </svg>
    ),
  },
  {
    key: "history",
    label: "History",
    icon: (
      <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 8v4l3 3m6-3a9 9 0 11-18 0 9 9 0 0118 0z" />
      </svg>
    ),
  },
  {
    key: "permissions",
    label: "Permissions",
    icon: (
      <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9 12l2 2 4-4m5.618-4.016A11.955 11.955 0 0112 2.944a11.955 11.955 0 01-8.618 3.04A12.02 12.02 0 003 9c0 5.591 3.824 10.29 9 11.622 5.176-1.332 9-6.03 9-11.622 0-1.042-.133-2.052-.382-3.016z" />
      </svg>
    ),
  },
  {
    key: "about",
    label: "About",
    icon: (
      <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M13 16h-1v-4h-1m1-4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
      </svg>
    ),
  },
];

export function SettingsWindow({
  state,
  onTogglePause,
  setAutoEnter,
  setConsumeMode,
  applyRuntimePreferences,
  setActiveTranscriber,
  setLanguage,
  startVoiceEnrollment,
  cancelVoiceEnrollment,
  deleteVoiceProfile,
  setVoiceProfileEnabled,
  downloadModel,
  deleteModel,
  onClose,
}: SettingsWindowProps) {
  const [activeTab, setActiveTab] = useState<Tab>("general");

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm animate-fade-in">
      <div className="bg-surface-900 rounded-2xl border border-surface-700 shadow-2xl w-full max-w-4xl h-[700px] flex flex-col animate-slide-up">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-surface-800">
          <h2 className="text-lg font-semibold text-white">Settings</h2>
          <button
            onClick={onClose}
            className="p-1.5 text-surface-400 hover:text-white hover:bg-surface-800 rounded-lg transition-colors"
          >
            <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M6 18L18 6M6 6l12 12" />
            </svg>
          </button>
        </div>

        <div className="flex flex-1 overflow-hidden">
          {/* Sidebar */}
          <div className="w-44 bg-surface-950 border-r border-surface-800 p-2 flex flex-col gap-0.5">
            {TABS.map((tab) => (
              <button
                key={tab.key}
                onClick={() => setActiveTab(tab.key)}
                className={`flex items-center gap-2.5 px-3 py-2 text-sm rounded-lg transition-colors ${
                  activeTab === tab.key
                    ? "bg-primary-600/20 text-primary-400"
                    : "text-surface-400 hover:text-white hover:bg-surface-800"
                }`}
              >
                {tab.icon}
                {tab.label}
              </button>
            ))}
          </div>

          {/* Content */}
          <div className="flex-1 overflow-y-auto p-6">
            {activeTab === "general" && (
              <GeneralTab state={state} setLanguage={setLanguage} />
            )}
            {activeTab === "behavior" && (
              <BehaviorTab
                isAutoEnter={state.isAutoEnter}
                isPaused={state.isPaused}
                onTogglePause={onTogglePause}
                setAutoEnter={setAutoEnter}
                setConsumeMode={setConsumeMode}
                applyRuntimePreferences={applyRuntimePreferences}
              />
            )}
            {activeTab === "shortcuts" && <ShortcutsTab />}
            {activeTab === "voice" && (
              <VoiceTab
                enrolled={state.enrolled}
                enabled={state.voiceEnabled}
                steps={state.voiceSteps}
                startVoiceEnrollment={startVoiceEnrollment}
                cancelVoiceEnrollment={cancelVoiceEnrollment}
                deleteVoiceProfile={deleteVoiceProfile}
                setVoiceProfileEnabled={setVoiceProfileEnabled}
              />
            )}
            {activeTab === "models" && (
              <ModelsTab
                models={state.models}
                backend={state.backend}
                setActiveTranscriber={setActiveTranscriber}
                downloadModel={downloadModel}
                deleteModel={deleteModel}
              />
            )}
            {activeTab === "history" && <HistoryTab recentTranscripts={state.recentTranscripts} />}
            {activeTab === "permissions" && <PermissionsTab inputMonitoringGranted={state.inputMonitoringGranted} />}
            {activeTab === "about" && <AboutTab />}
          </div>
        </div>
      </div>
    </div>
  );
}

// ─── Tab Components ───────────────────────────────────────────────────────

function GeneralTab({
  state,
  setLanguage,
}: {
  state: DaemonState;
  setLanguage: (lang: string) => void;
}) {
  const languages = [
    { code: "auto", name: "Auto-detect" },
    { code: "en", name: "English" },
    { code: "es", name: "Spanish" },
    { code: "fr", name: "French" },
    { code: "de", name: "German" },
    { code: "ja", name: "Japanese" },
    { code: "zh", name: "Chinese" },
  ];

  return (
    <div className="space-y-6">
      <div>
        <h3 className="text-sm font-medium text-surface-300 mb-3">Language</h3>
        <select
          value={state.recentTranscripts.length > 0 ? "auto" : "en"}
          onChange={(e) => setLanguage(e.target.value)}
          className="w-full bg-surface-800 border border-surface-700 rounded-lg px-3 py-2 text-white text-sm focus:outline-none focus:ring-2 focus:ring-primary-500"
        >
          {languages.map((l) => (
            <option key={l.code} value={l.code}>
              {l.name}
            </option>
          ))}
        </select>
      </div>
      <div>
        <h3 className="text-sm font-medium text-surface-300 mb-3">Active Backend</h3>
        <div className="flex items-center gap-2 px-3 py-2 bg-surface-800 rounded-lg">
          <div className={`w-2 h-2 rounded-full ${state.backend === "groq" ? "bg-green-500" : "bg-blue-500"}`} />
          <span className="text-sm text-white">{state.backend}</span>
        </div>
      </div>
      <div>
        <h3 className="text-sm font-medium text-surface-300 mb-3">Protocol Version</h3>
        <p className="text-sm text-surface-400">v12</p>
      </div>
    </div>
  );
}

function BehaviorTab({
  isAutoEnter,
  isPaused,
  onTogglePause,
  setAutoEnter,
  setConsumeMode,
  applyRuntimePreferences,
}: {
  isAutoEnter: boolean;
  isPaused: boolean;
  onTogglePause: () => void;
  setAutoEnter: (enabled: boolean) => void;
  setConsumeMode: (enabled: boolean) => void;
  applyRuntimePreferences: (prefs: any) => void;
}) {
  const [autoEnterDelay, setAutoEnterDelay] = useState(4000);
  const [silence, setSilence] = useState(0.9);
  const [sensitivity, setSensitivity] = useState<"high" | "normal" | "low">("normal");
  const [livePreview, setLivePreview] = useState(true);

  const presets: Record<string, { threshold: number }> = {
    high: { threshold: 0.005 },
    normal: { threshold: 0.012 },
    low: { threshold: 0.025 },
  };

  return (
    <div className="space-y-6">
      {/* Global Pause */}
      <div>
        <h3 className="text-sm font-medium text-surface-300 mb-3">Global Pause</h3>
        <button
          onClick={onTogglePause}
          className={`w-full px-4 py-2.5 rounded-lg text-sm font-medium transition-colors ${
            isPaused
              ? "bg-green-600 text-white hover:bg-green-500"
              : "bg-red-600 text-white hover:bg-red-500"
          }`}
        >
          {isPaused ? "Resume Always" : "Pause Always"}
        </button>
      </div>

      {/* Auto Enter */}
      <div>
        <h3 className="text-sm font-medium text-surface-300 mb-3">Auto Enter</h3>
        <div className="flex items-center justify-between px-3 py-2 bg-surface-800 rounded-lg">
          <span className="text-sm text-surface-300">Auto-press Enter after transcription</span>
          <button
            onClick={() => {
              setAutoEnter(!isAutoEnter);
            }}
            className={`relative w-11 h-6 rounded-full transition-colors ${
              isAutoEnter ? "bg-primary-600" : "bg-surface-600"
            }`}
          >
            <div
              className={`absolute top-0.5 w-5 h-5 bg-white rounded-full shadow transition-transform ${
                isAutoEnter ? "translate-x-5.5" : "translate-x-0.5"
              }`}
            />
          </button>
        </div>
        <div className="mt-3 flex items-center gap-3">
          <label className="text-xs text-surface-400">Delay (ms)</label>
          <input
            type="range"
            min={0}
            max={60000}
            step={500}
            value={autoEnterDelay}
            onChange={(e) => {
              setAutoEnterDelay(Number(e.target.value));
              applyRuntimePreferences({ auto_enter_delay_ms: Number(e.target.value) });
            }}
            className="flex-1 accent-primary-500"
          />
          <span className="text-xs text-surface-400 w-12 text-right">{autoEnterDelay}</span>
        </div>
      </div>

      {/* Consume Mode */}
      <div>
        <h3 className="text-sm font-medium text-surface-300 mb-3">Consume Mode</h3>
        <p className="text-xs text-surface-500 mb-2">
          Route transcriptions to stream consumers instead of pasting into the focused app.
        </p>
        <button
          onClick={() => setConsumeMode(false)}
          className="px-3 py-1.5 text-xs bg-surface-800 text-surface-300 rounded-md hover:bg-surface-700"
        >
          Disabled
        </button>
      </div>

      {/* Sensitivity */}
      <div>
        <h3 className="text-sm font-medium text-surface-300 mb-3">Mic Sensitivity</h3>
        <div className="flex gap-2">
          {(["high", "normal", "low"] as const).map((level) => (
            <button
              key={level}
              onClick={() => {
                setSensitivity(level);
                const preset = level as SensitivityPreset;
                applyRuntimePreferences({ energy_threshold: presets[level].threshold });
              }}
              className={`flex-1 px-3 py-2 text-xs rounded-lg font-medium transition-colors ${
                sensitivity === level
                  ? "bg-primary-600 text-white"
                  : "bg-surface-800 text-surface-400 hover:bg-surface-700"
              }`}
            >
              {level.charAt(0).toUpperCase() + level.slice(1)}
            </button>
          ))}
        </div>
      </div>

      {/* Silence Tolerance */}
      <div>
        <h3 className="text-sm font-medium text-surface-300 mb-3">Pause Tolerance</h3>
        <div className="flex items-center gap-3">
          <input
            type="range"
            min={300}
            max={15000}
            step={100}
            value={Math.round(silence * 1000)}
            onChange={(e) => {
              const val = Number(e.target.value) / 1000;
              setSilence(val);
              applyRuntimePreferences({ silence_secs: val });
            }}
            className="flex-1 accent-primary-500"
          />
          <span className="text-xs text-surface-400 w-16 text-right">{silence}s</span>
        </div>
      </div>

      {/* Live Preview */}
      <div>
        <h3 className="text-sm font-medium text-surface-300 mb-3">Live Preview</h3>
        <div className="flex items-center justify-between px-3 py-2 bg-surface-800 rounded-lg">
          <span className="text-sm text-surface-300">Show partial transcript while speaking</span>
          <button
            onClick={() => {
              setLivePreview(!livePreview);
              applyRuntimePreferences({ stt_live_preview: !livePreview });
            }}
            className={`relative w-11 h-6 rounded-full transition-colors ${
              livePreview ? "bg-primary-600" : "bg-surface-600"
            }`}
          >
            <div
              className={`absolute top-0.5 w-5 h-5 bg-white rounded-full shadow transition-transform ${
                livePreview ? "translate-x-5.5" : "translate-x-0.5"
              }`}
            />
          </button>
        </div>
      </div>
    </div>
  );
}

function ShortcutsTab() {
  const shortcuts = [
    { label: "Pause/Resume", default: "⌥ Space" },
    { label: "Global Pause", default: "⌃⌥P" },
    { label: "Global Resume", default: "⌃⌥⇧P" },
    { label: "Capture Correction", default: "⌃⌥X" },
    { label: "Grammar Correction", default: "⌥G" },
    { label: "Consume Mode", default: "⌃⌥C" },
  ];

  return (
    <div>
      <h3 className="text-sm font-medium text-surface-300 mb-4">Keyboard Shortcuts</h3>
      <div className="space-y-2">
        {shortcuts.map((s, i) => (
          <div
            key={i}
            className="flex items-center justify-between px-3 py-2.5 bg-surface-800 rounded-lg"
          >
            <span className="text-sm text-surface-300">{s.label}</span>
            <kbd className="text-xs text-surface-400 bg-surface-700 px-2 py-1 rounded">
              {s.default}
            </kbd>
          </div>
        ))}
      </div>
      <p className="text-xs text-surface-500 mt-4">
        Changes are applied after reloading shortcuts from Settings.
      </p>
    </div>
  );
}

function VoiceTab({
  enrolled,
  enabled,
  steps,
  startVoiceEnrollment,
  cancelVoiceEnrollment,
  deleteVoiceProfile,
  setVoiceProfileEnabled,
}: {
  enrolled: boolean;
  enabled: boolean;
  steps: string[];
  startVoiceEnrollment: (step: string) => void;
  cancelVoiceEnrollment: () => void;
  deleteVoiceProfile: () => void;
  setVoiceProfileEnabled: (enabled: boolean) => void;
}) {
  const progress = ENROLLMENT_STEPS.map((s) => steps.includes(s));

  return (
    <div className="space-y-6">
      <div>
        <h3 className="text-sm font-medium text-surface-300 mb-3">"My Voice" Gate</h3>
        <p className="text-xs text-surface-500 mb-3">
          Only transcribe your voice. Requires enrollment of 3 voice samples at different volumes.
        </p>
        <div className="flex items-center justify-between px-3 py-2 bg-surface-800 rounded-lg">
          <span className="text-sm text-surface-300">Enabled</span>
          <button
            onClick={() => setVoiceProfileEnabled(!enabled)}
            className={`relative w-11 h-6 rounded-full transition-colors ${
              enabled ? "bg-primary-600" : "bg-surface-600"
            }`}
          >
            <div
              className={`absolute top-0.5 w-5 h-5 bg-white rounded-full shadow transition-transform ${
                enabled ? "translate-x-5.5" : "translate-x-0.5"
              }`}
            />
          </button>
        </div>
      </div>

      {/* Enrollment Progress */}
      <div>
        <h3 className="text-sm font-medium text-surface-300 mb-3">Enrollment Progress</h3>
        <div className="space-y-2">
          {ENROLLMENT_STEPS.map((s, i) => (
            <div key={s} className="flex items-center gap-3 px-3 py-2 bg-surface-800 rounded-lg">
              <div className={`w-6 h-6 rounded-full flex items-center justify-center text-xs font-medium ${
                progress[i]
                  ? "bg-green-500 text-white"
                  : "bg-surface-700 text-surface-400"
              }`}>
                {progress[i] ? (
                  <svg className="w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                    <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={3} d="M5 13l4 4L19 7" />
                  </svg>
                ) : (
                  i + 1
                )}
              </div>
              <span className="text-sm text-surface-300 flex-1 capitalize">{s}</span>
              {!progress[i] && i === progress.findIndex((p) => !p) && (
                <button
                  onClick={() => startVoiceEnrollment(s)}
                  className="px-3 py-1 text-xs bg-primary-600 text-white rounded-md hover:bg-primary-500"
                >
                  Record
                </button>
              )}
            </div>
          ))}
        </div>
      </div>

      {/* Actions */}
      <div className="flex gap-3">
        {enrolled && (
          <button
            onClick={deleteVoiceProfile}
            className="px-3 py-2 text-xs text-red-400 bg-red-500/10 hover:bg-red-500/20 rounded-lg transition-colors"
          >
            Delete Voice Profile
          </button>
        )}
      </div>
    </div>
  );
}

function ModelsTab({
  models,
  backend,
  setActiveTranscriber,
  downloadModel,
  deleteModel,
}: {
  models: ModelInfo[];
  backend: string;
  setActiveTranscriber: (backend: string) => void;
  downloadModel: (modelId: string) => void;
  deleteModel: (modelId: string) => void;
}) {
  return (
    <div className="space-y-4">
      <div>
        <h3 className="text-sm font-medium text-surface-300 mb-3">Active Backend</h3>
        <div className="flex items-center gap-2 px-3 py-2 bg-surface-800 rounded-lg">
          <div className={`w-2 h-2 rounded-full ${backend === "groq" ? "bg-green-500" : "bg-blue-500"}`} />
          <span className="text-sm text-white">{backend}</span>
        </div>
      </div>

      <div>
        <h3 className="text-sm font-medium text-surface-300 mb-3">Local Models</h3>
        {models.length === 0 ? (
          <p className="text-sm text-surface-500">No local models installed.</p>
        ) : (
          <div className="space-y-2">
            {models.map((m) => (
              <div
                key={m.model_id}
                className={`flex items-center justify-between px-3 py-2.5 rounded-lg border ${
                  backend === `local:${m.model_id}`
                    ? "border-primary-500/30 bg-primary-500/5"
                    : "border-surface-700 bg-surface-800"
                }`}
              >
                <div>
                  <p className="text-sm text-white font-medium">{m.name}</p>
                  <p className="text-xs text-surface-400">
                    {m.model_id}
                    {m.size_bytes && ` · ${(m.size_bytes / 1e6).toFixed(0)}MB`}
                  </p>
                </div>
                <div className="flex items-center gap-2">
                  {m.is_downloading && (
                    <span className="text-xs text-primary-400">{m.download_percentage?.toFixed(0)}%</span>
                  )}
                  {m.is_downloaded ? (
                    backend === `local:${m.model_id}` ? (
                      <span className="text-xs text-green-400">Active</span>
                    ) : (
                      <button
                        onClick={() => setActiveTranscriber(`local:${m.model_id}`)}
                        className="px-2 py-1 text-xs bg-surface-700 text-surface-300 rounded hover:bg-surface-600"
                      >
                        Switch
                      </button>
                    )
                  ) : (
                    <button
                      onClick={() => downloadModel(m.model_id)}
                      disabled={m.is_downloading}
                      className="px-2 py-1 text-xs bg-primary-600 text-white rounded hover:bg-primary-500 disabled:opacity-40"
                    >
                      Download
                    </button>
                  )}
                  {m.is_downloaded && (
                    <button
                      onClick={() => deleteModel(m.model_id)}
                      className="p-1 text-surface-500 hover:text-red-400"
                    >
                      <svg className="w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16" />
                      </svg>
                    </button>
                  )}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function HistoryTab({
  recentTranscripts,
}: {
  recentTranscripts: { text: string; timestamp: number }[];
}) {
  if (recentTranscripts.length === 0) {
    return (
      <div>
        <h3 className="text-sm font-medium text-surface-300 mb-3">Recent Transcripts</h3>
        <p className="text-sm text-surface-500">No transcripts yet. Start speaking to see history here.</p>
      </div>
    );
  }

  return (
    <div>
      <h3 className="text-sm font-medium text-surface-300 mb-3">Recent Transcripts</h3>
      <div className="space-y-2 max-h-96 overflow-y-auto">
        {recentTranscripts.map((entry, i) => (
          <div key={i} className="px-3 py-2 bg-surface-800 rounded-lg">
            <p className="text-sm text-surface-200">{entry.text}</p>
            <p className="text-[10px] text-surface-500 mt-1">
              {new Date(entry.timestamp).toLocaleTimeString()}
            </p>
          </div>
        ))}
      </div>
    </div>
  );
}

function PermissionsTab({
  inputMonitoringGranted,
}: {
  inputMonitoringGranted: boolean;
}) {
  return (
    <div className="space-y-4">
      <h3 className="text-sm font-medium text-surface-300">System Permissions</h3>

      <div className={`flex items-center justify-between px-3 py-2.5 rounded-lg border ${
        inputMonitoringGranted ? "border-green-500/30 bg-green-500/5" : "border-surface-700 bg-surface-800"
      }`}>
        <div>
          <p className="text-sm text-white">Input Monitoring</p>
          <p className="text-xs text-surface-400">Required for global keyboard shortcuts</p>
        </div>
        <span className={`text-xs px-2 py-1 rounded-full ${
          inputMonitoringGranted ? "bg-green-500/20 text-green-400" : "bg-red-500/20 text-red-400"
        }`}>
          {inputMonitoringGranted ? "Granted" : "Denied"}
        </span>
      </div>

      <div className="flex items-center justify-between px-3 py-2.5 bg-surface-800 rounded-lg border border-surface-700">
        <div>
          <p className="text-sm text-white">Microphone</p>
          <p className="text-xs text-surface-400">Required for audio capture</p>
        </div>
        <span className="text-xs px-2 py-1 rounded-full bg-green-500/20 text-green-400">Granted</span>
      </div>
    </div>
  );
}

function AboutTab() {
  return (
    <div className="space-y-4">
      <div className="flex items-center gap-4 mb-6">
        <div className="w-12 h-12 rounded-xl bg-primary-600 flex items-center justify-center">
          <svg className="w-6 h-6 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19 11a7 7 0 01-7 7m0 0a7 7 0 01-7-7m7 7v4m0 0H8m4 0h4m-4-8a3 3 0 01-3-3V5a3 3 0 116 0v6a3 3 0 01-3 3z" />
          </svg>
        </div>
        <div>
          <h3 className="text-lg font-semibold text-white">Always</h3>
          <p className="text-sm text-surface-400">Version 0.1.0</p>
        </div>
      </div>

      <div className="space-y-2 text-sm text-surface-400">
        <p>Always is an always-on speech-to-text assistant for macOS.</p>
        <p>Protocol version: v12</p>
        <p>License: AGPL-3.0-only</p>
        <p>
          Repository:{" "}
          <a
            href="https://github.com/LivioGama/always"
            target="_blank"
            rel="noopener noreferrer"
            className="text-primary-400 hover:underline"
          >
            github.com/LivioGama/always
          </a>
        </p>
      </div>
    </div>
  );
}