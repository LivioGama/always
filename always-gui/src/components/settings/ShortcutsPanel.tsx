import type { DaemonApi } from "../../hooks/useDaemonState";
import type { Config } from "../../lib/config";
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
  const save = (key: keyof Config) => (value: string) => {
    onConfigChange({ [key]: value } as Partial<Config>);
    daemon.reloadShortcuts();
  };

  return (
    <div className="p-5 space-y-3">
      <Caption>
        Click a shortcut to record a new one. Changes apply immediately — no restart needed.
      </Caption>
      {!sReady(daemon) && (
        <div className="rounded-md bg-orange-500/10 border border-orange-500/30 px-3 py-2 text-xs text-orange-400">
          Daemon not connected — new shortcuts are saved but won't take effect until the daemon is running.
        </div>
      )}
      <div>
        <PanelTitle>Global Shortcuts</PanelTitle>
        <div className="mt-1 divide-y divide-surface-800/60">
          <KeyCaptureButton
            label="Master Pause / Resume"
            description="Pause or resume Always everywhere (kill switch)"
            shortcut={config.shortcutMasterPause}
            onSave={save("shortcutMasterPause")}
          />
          <KeyCaptureButton
            label="Toggle Pause for This App"
            description="Pause or resume voice typing in the focused app (allowlist)"
            shortcut={config.shortcutPause}
            onSave={save("shortcutPause")}
          />
          <KeyCaptureButton
            label="Toggle Auto-Enter"
            description="Turn the post-paste Return press on or off"
            shortcut={config.shortcutAutoEnter}
            onSave={save("shortcutAutoEnter")}
          />
          <KeyCaptureButton
            label="Re-Paste Last Transcript"
            description="Paste the most recent transcript again"
            shortcut={config.shortcutForcePaste}
            onSave={save("shortcutForcePaste")}
          />
          <KeyCaptureButton
            label="Correction Dialog"
            description="Open the 'you misheard me' dialog to teach Always"
            shortcut={config.shortcutCorrectionDialog}
            onSave={save("shortcutCorrectionDialog")}
          />
        </div>
      </div>
    </div>
  );
}

function sReady(d: DaemonApi) {
  return d.state.isDaemonConnected;
}
