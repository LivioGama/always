import type { DaemonApi } from "../../hooks/useDaemonState";
import type { Config } from "../../lib/config";
import { useEffect, useState } from "react";
import { platform } from "../../lib/tauri";
import { shortcutsNeedDesktop } from "../../lib/shortcuts";
import { Caption, KeyCaptureButton, PanelTitle } from "../controls";

/// Settings → Shortcuts: global hotkey remapping. Mirrors ShortcutsPanel.swift —
/// one KeyCaptureButton per action; saving writes the config key and pokes the
/// daemon to reload shortcuts live.
export function ShortcutsPanel({
  daemon,
  config,
  onConfigChange,
}: {
  daemon: DaemonApi;
  config: Config;
  onConfigChange: (patch: Partial<Config>) => void;
}) {
  const [os, setOs] = useState("windows");
  useEffect(() => {
    platform().then(setOs).catch(() => {});
  }, []);
  // Linux on Wayland: Always cannot grab keys; the keys here are only recorded.
  const needDesktop = shortcutsNeedDesktop(os, daemon.state.inputMonitoringGranted);

  const save = (key: keyof Config) => (value: string) => {
    onConfigChange({ [key]: value } as Partial<Config>);
    daemon.reloadShortcuts();
  };

  return (
    <div className="p-5 space-y-3">
      <PanelTitle>Keyboard Shortcuts</PanelTitle>
      {!sReady(daemon) && (
        <div className="rounded-md bg-orange-500/10 border border-orange-500/30 px-3 py-2 text-xs text-orange-400">
          Daemon not connected — new shortcuts are saved but won't take effect until the daemon is running.
        </div>
      )}
      {needDesktop && (
        <div className="rounded-md bg-primary-500/10 border border-primary-500/30 px-3 py-2 text-xs text-surface-200">
          On Wayland Always cannot listen for keys itself. Bind keys in your desktop's settings to{" "}
          <code>always toggle-pause</code> (pause everywhere) and <code>always toggle-auto-enter</code>. Keys
          recorded here are what the setup tips show; they do not take effect by themselves.
        </div>
      )}
      <div>
        <PanelTitle>Global Shortcuts</PanelTitle>
        <div className="mt-1 divide-y divide-white/[0.08]">
          <KeyCaptureButton
            label="Master Pause / Mute"
            description="Globally pause or resume all dictation."
            shortcut={config.shortcutMasterPause}
            onSave={save("shortcutMasterPause")}
          />
          <KeyCaptureButton
            label="Pause / Resume"
            description="Toggle pause for the currently focused app only."
            shortcut={config.shortcutPause}
            onSave={save("shortcutPause")}
          />
          <KeyCaptureButton
            label="Toggle Auto-Enter"
            description="Enable or disable automatic Enter after pasting."
            shortcut={config.shortcutAutoEnter}
            onSave={save("shortcutAutoEnter")}
          />
          <KeyCaptureButton
            label="Paste Last Filtered"
            description="Re-paste the most recent filtered transcript."
            shortcut={config.shortcutForcePaste}
            onSave={save("shortcutForcePaste")}
          />
          <KeyCaptureButton
            label="Correction Dialog"
            description="Open a dialog to correct the last transcript."
            shortcut={config.shortcutCorrectionDialog}
            onSave={save("shortcutCorrectionDialog")}
          />
        </div>
      </div>
      <Caption className="pt-1">
        {needDesktop ? "Changes are recorded only — bind the keys in your desktop." : "Changes take effect immediately."}
      </Caption>
    </div>
  );
}

function sReady(d: DaemonApi) {
  return d.state.isDaemonConnected;
}
