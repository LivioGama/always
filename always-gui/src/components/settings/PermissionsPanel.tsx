import { useEffect, useState } from "react";
import type { DaemonApi } from "../../hooks/useDaemonState";
import { openUrl, platform } from "../../lib/tauri";
import { Btn, Caption, PanelTitle, Spinner } from "../controls";

/// Settings → Permissions. Platform copy follows the native permission and
/// global-hotkey model reported by the daemon.
export function PermissionsPanel({ daemon }: { daemon: DaemonApi }) {
  const [os, setOs] = useState("windows");
  const [checking, setChecking] = useState(false);

  useEffect(() => {
    platform().then(setOs).catch(() => {});
  }, []);

  const s = daemon.state;
  const micOk = s.isDaemonConnected;
  const shortcutsOk = s.inputMonitoringGranted;

  const recheck = () => {
    setChecking(true);
    setTimeout(() => setChecking(false), 800);
  };

  return (
    <div className="p-5 space-y-4">
      <div>
        <PanelTitle>Permissions</PanelTitle>
        <Caption className="mt-1">
          {os === "windows"
            ? "Windows grants desktop apps mic access globally — but it can be turned off in Settings → Privacy & security → Microphone."
            : os === "linux"
              ? "Microphone access comes from your audio session. Global shortcuts require an X11 session."
              : "Always needs these permissions to hear you and type for you."}
        </Caption>
      </div>

      <PermissionRow
        icon="mic"
        ok={micOk}
        title="Microphone"
        okText="Granted — the daemon is hearing audio."
        badText="Can't reach the daemon, so the microphone state is unknown. Start the daemon, or check OS microphone privacy settings."
        actionTitle={os === "windows" ? "Open Microphone Settings" : os === "macos" ? "Open Settings" : undefined}
        onAction={() =>
          openUrl(
            os === "windows"
              ? "ms-settings:privacy-microphone"
            : os === "macos"
              ? "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
              : "",
          )
        }
      />

      <PermissionRow
        icon="keyboard"
        ok={shortcutsOk}
        title={os === "macos" ? "Input Monitoring" : "Global Shortcuts"}
        okText={
          os === "windows"
            ? "Active — the daemon is listening for global hotkeys."
            : "Granted — global shortcuts are active."
        }
        badText={
          os === "windows"
            ? "The daemon couldn't register the global hotkeys — another app may already own one of the chords. Reassign it in the Shortcuts tab."
            : os === "linux"
              ? "X11 is required for global shortcuts. On Wayland, use the CLI to toggle pause and auto-enter."
              : "Required for global shortcuts to work."
        }
        actionTitle={os === "macos" ? "Open Settings" : undefined}
        onAction={
          os === "macos"
            ? () => openUrl("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
            : undefined
        }
      />

      <div className="flex items-center gap-2 pt-1">
        <Btn onClick={recheck} disabled={checking}>
          {checking ? <Spinner /> : null}
          Re-check
        </Btn>
        <Caption>Statuses refresh live when the daemon reports them.</Caption>
      </div>
    </div>
  );
}

function PermissionRow({ icon, ok, title, okText, badText, actionTitle, onAction }: {
  icon: "mic" | "keyboard";
  ok: boolean;
  title: string;
  okText: string;
  badText: string;
  actionTitle?: string;
  onAction?: () => void;
}) {
  return (
    <div className="flex items-start gap-3 rounded-lg bg-white/[0.08] p-3">
      <span className="mt-0.5 text-surface-400">
        {icon === "mic" ? (
          <svg className="w-5 h-5" viewBox="0 0 24 24" fill="none" stroke="currentColor"><rect x="9" y="2" width="6" height="12" rx="3" strokeWidth={2} /><path strokeWidth={2} strokeLinecap="round" d="M5 10a7 7 0 0014 0m-7 7v5m-4 0h8" /></svg>
        ) : (
          <svg className="w-5 h-5" viewBox="0 0 24 24" fill="none" stroke="currentColor"><rect x="3" y="4" width="18" height="13" rx="2" strokeWidth={2} /><path strokeWidth={2} strokeLinecap="round" d="M8 21h8m-4-4v4" /></svg>
        )}
      </span>
      <span className={`mt-0.5 ${ok ? "text-green-500" : "text-orange-400"}`}>
        {ok ? (
          <svg className="w-4 h-4" viewBox="0 0 24 24" fill="currentColor">
            <path d="M12 2a10 10 0 100 20 10 10 0 000-20zm-1.5 14.5l-4-4 1.4-1.4 2.6 2.6 5.6-5.6 1.4 1.4-7 7z" />
          </svg>
        ) : (
          <svg className="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path strokeWidth={2} strokeLinecap="round" d="M12 8v5m0 3h.01M10.3 3.9L1.8 18a2 2 0 001.7 3h17a2 2 0 001.7-3L13.7 3.9a2 2 0 00-3.4 0z" />
          </svg>
        )}
      </span>
      <div className="flex-1 min-w-0">
        <div className="text-[13px] font-medium text-surface-100">{title}</div>
        <Caption>{ok ? okText : badText}</Caption>
      </div>
      {ok ? (
        <svg className="w-4 h-4 mt-0.5 text-green-500" viewBox="0 0 24 24" fill="currentColor"><path d="M12 2a10 10 0 100 20 10 10 0 000-20zm-1.5 14.5l-4-4 1.4-1.4 2.6 2.6 5.6-5.6 1.4 1.4-7 7z" /></svg>
      ) : actionTitle && onAction ? <Btn onClick={onAction}>{actionTitle}</Btn> : null}
    </div>
  );
}
