import { useCallback, useEffect, useRef, useState } from "react";
import type { DaemonApi } from "../hooks/useDaemonState";
import { DEFAULT_CONFIG, parseConfig, type Config } from "../lib/config";
import { configSet, configShow } from "../lib/tauri";
import { SettingsSidebar, type SettingsPanelKey } from "./settings/SettingsSidebar";
import { GeneralPanel } from "./settings/GeneralPanel";
import { ModelsPanel } from "./settings/ModelsPanel";
import { MyVoicePanel } from "./settings/MyVoicePanel";
import { PermissionsPanel } from "./settings/PermissionsPanel";
import { BehaviorPanel } from "./settings/BehaviorPanel";
import { ShortcutsPanel } from "./settings/ShortcutsPanel";
import { LibraryPanel } from "./settings/LibraryPanel";
import { HistoryPanel } from "./settings/HistoryPanel";
import { AboutPanel } from "./settings/AboutPanel";

// Which config key each Config field writes back through `always config set`.
const FIELD_KEYS: Record<string, { key: string; fmt: (v: any) => string }> = {
  sttEnergyThreshold: { key: "stt_energy_threshold", fmt: (v) => String(v) },
  hearEnergyThreshold: { key: "hear_energy_threshold", fmt: (v) => String(v) },
  sttCooldownMs: { key: "stt_cooldown_ms", fmt: (v) => String(Math.round(v)) },
  sttSilence: { key: "stt_silence_secs", fmt: (v) => String(v) },
  sttAdaptiveSilence: { key: "stt_adaptive_silence", fmt: (v) => String(v) },
  sttLivePreview: { key: "stt_live_preview", fmt: (v) => String(v) },
  liveTyping: { key: "live_typing", fmt: (v) => String(v) },
  sttAutoEnter: { key: "stt_auto_enter", fmt: (v) => String(v) },
  autoEnterDelayMs: { key: "auto_enter_delay_ms", fmt: (v) => String(Math.round(v)) },
  sileroThreshold: { key: "silero_threshold", fmt: (v) => String(v) },
  shortcutPause: { key: "shortcut_pause", fmt: (v) => v || "(none)" },
  shortcutAutoEnter: { key: "shortcut_auto_enter", fmt: (v) => v || "(none)" },
  shortcutForcePaste: { key: "shortcut_force_paste", fmt: (v) => v || "(none)" },
  shortcutCorrectionDialog: { key: "shortcut_correction_dialog", fmt: (v) => v || "(none)" },
  shortcutMasterPause: { key: "shortcut_master_pause", fmt: (v) => v || "(none)" },
  postprocessEnabled: { key: "postprocess_enabled", fmt: (v) => String(v) },
  postprocessProvider: { key: "postprocess_provider", fmt: (v) => String(v) },
  postprocessMode: { key: "postprocess_mode", fmt: (v) => String(v) },
  idlePauseSecs: { key: "idle_pause_secs", fmt: (v) => String(Math.round(v)) },
  audibleStatusSound: { key: "audible_status_sound", fmt: (v) => String(v) },
  lang: { key: "lang", fmt: (v) => v ?? "auto" },
};

export function SettingsWindow({ daemon }: { daemon: DaemonApi }) {
  const [panel, setPanel] = useState<SettingsPanelKey>("general");
  const [config, setConfig] = useState<Config>(DEFAULT_CONFIG);
  const [loaded, setLoaded] = useState(false);
  const saveTimers = useRef<Record<string, ReturnType<typeof setTimeout>>>({});

  const load = useCallback(async () => {
    try {
      const out = await configShow();
      setConfig(parseConfig(out));
    } catch {
      /* daemon CLI unavailable — keep defaults */
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  // Persist a config patch: optimistic local state + `always config set` per
  // field (debounced for the slider-ish numeric rows), mirroring the Swift
  // app's CLIService.setConfig write-through.
  const onConfigChange = useCallback((patch: Partial<Config>) => {
    setConfig((prev) => ({ ...prev, ...patch }));
    for (const [field, value] of Object.entries(patch)) {
      const meta = FIELD_KEYS[field];
      if (!meta) continue;
      clearTimeout(saveTimers.current[field]);
      saveTimers.current[field] = setTimeout(() => {
        configSet(meta.key, meta.fmt(value)).catch(() => {});
      }, 250);
    }
  }, []);

  return (
    <div className="h-screen flex bg-surface-900 text-surface-100 overflow-hidden">
      <SettingsSidebar selected={panel} onSelect={setPanel} state={daemon.state} />
      <div className="flex-1 min-w-0 overflow-y-auto">
        {!loaded ? (
          <div className="p-5 text-xs text-surface-500">Loading settings…</div>
        ) : (
          <>
            {panel === "general" && <GeneralPanel daemon={daemon} config={config} onConfigChange={onConfigChange} />}
            {panel === "models" && <ModelsPanel daemon={daemon} config={config} onConfigChange={onConfigChange} />}
            {panel === "myVoice" && <MyVoicePanel daemon={daemon} />}
            {panel === "permissions" && <PermissionsPanel daemon={daemon} />}
            {panel === "behavior" && <BehaviorPanel daemon={daemon} config={config} onConfigChange={onConfigChange} />}
            {panel === "shortcuts" && <ShortcutsPanel daemon={daemon} config={config} onConfigChange={onConfigChange} />}
            {panel === "library" && <LibraryPanel />}
            {panel === "history" && <HistoryPanel />}
            {panel === "about" && <AboutPanel daemon={daemon} />}
          </>
        )}
      </div>
    </div>
  );
}
