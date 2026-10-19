import type { DaemonApi } from "../../hooks/useDaemonState";
import type { Config } from "../../lib/config";
import { Caption, Card, Divider, PanelTitle, Segmented, Btn } from "../controls";
import { PermissionsBanner } from "./PermissionsBanner";

/// Settings → General: sound cues, overlay mode, voice-typing allowlist.
/// Mirrors GeneralPanel.swift.
export function GeneralPanel({
  daemon,
  config,
  onConfigChange,
}: {
  daemon: DaemonApi;
  config: Config;
  onConfigChange: (patch: Partial<Config>) => void;
}) {
  const s = daemon.state;

  return (
    <div className="p-5 space-y-3.5">
      <PermissionsBanner daemon={daemon} />

      {/* Sound cues */}
      <Card>
        <div className="flex items-center gap-2.5">
          <svg
            className={`w-5 h-5 shrink-0 ${config.audibleStatusSound === "off" ? "text-surface-500" : "text-primary-400"}`}
            viewBox="0 0 24 24" fill="none" stroke="currentColor"
          >
            {config.audibleStatusSound === "off" ? (
              <path strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" d="M11 5L6 9H2v6h4l5 4V5zM22 9l-6 6M16 9l6 6" />
            ) : (
              <path strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" d="M11 5L6 9H2v6h4l5 4V5zM15.5 8.5a5 5 0 010 7M19 5a9 9 0 010 14" />
            )}
          </svg>
          <div className="flex-1 min-w-0">
            <div className="text-[13px] font-semibold text-white">Sound</div>
            <Caption>Play distinct sounds for listening, transcribing, success, and failure.</Caption>
          </div>
          <Segmented
            width={260}
            value={config.audibleStatusSound}
            onChange={(v) => onConfigChange({ audibleStatusSound: v })}
            options={[
              { label: "Off", value: "off" },
              { label: "Low", value: "low" },
              { label: "Medium", value: "medium" },
              { label: "High", value: "high" },
            ]}
          />
        </div>
      </Card>

      <Divider />

      {/* Overlay — kept for parity; on Windows the overlay isn't shipped yet */}
      <div>
        <PanelTitle>Status Overlay</PanelTitle>
        <div className="flex items-center gap-3 mt-2">
          <span className="text-[13px] text-surface-200" title="The floating indicator shown while you dictate. Compact shrinks it to a small pill; Hidden never shows it.">
            Listening / transcribing indicator
          </span>
          <div className="flex-1" />
          <Segmented
            width={280}
            value={localStorage.getItem("always_overlay_mode") ?? "normal"}
            onChange={(v) => localStorage.setItem("always_overlay_mode", v)}
            options={[
              { label: "Normal", value: "normal" },
              { label: "Compact", value: "compact" },
              { label: "Hidden", value: "hidden" },
            ]}
          />
        </div>
        <Caption className="mt-1">
          Appears centred near the bottom of the screen while Always hears you and transcribes. Changes apply immediately — no restart needed.
        </Caption>
      </div>

      <Divider />

      {/* Voice Typing Allowlist */}
      <div>
        <div className="flex items-baseline gap-2">
          <PanelTitle>Voice Typing Allowlist</PanelTitle>
          <div className="flex-1" />
          <Caption>{s.resumedApps.length} app{s.resumedApps.length === 1 ? "" : "s"} resumed</Caption>
        </div>
        <Caption className="mt-1">
          Always is paused by default. Add an app here to resume voice typing while you're in it.
        </Caption>

        {/* Master pause card */}
        <Card accent={!s.isMasterPaused} className={`mt-2.5 ${s.isMasterPaused ? "bg-orange-500/10" : ""}`}>
          <div className="flex items-center gap-2.5">
            <svg className={`w-5 h-5 ${s.isMasterPaused ? "text-orange-400" : "text-primary-400"}`} viewBox="0 0 24 24" fill="none" stroke="currentColor">
              {s.isMasterPaused ? (
                <path strokeWidth={2} strokeLinecap="round" d="M12 8v5m0 3h.01M12 3l9 16H3l9-16z" />
              ) : (
                <path strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" d="M12 3l7 3v5c0 4.5-3 8.5-7 10-4-1.5-7-5.5-7-10V6l7-3zm-3 9l2 2 4-4" />
              )}
            </svg>
            <div className="flex-1">
              <div className="text-[13px] font-semibold text-white">
                {s.isMasterPaused ? "Paused everywhere" : "Allowlist active"}
              </div>
              <Caption>
                {s.isMasterPaused
                  ? "Master kill switch is on — every resumed app is force-paused."
                  : "Resumed apps below are listening; everything else is paused."}
              </Caption>
            </div>
            <Btn disabled={!s.isDaemonConnected} onClick={daemon.togglePause}>
              {s.isMasterPaused ? "Lift pause" : "Pause everything"}
            </Btn>
          </div>
        </Card>

        {/* Focused app card */}
        <FocusedAppCard daemon={daemon} />

        {/* Resumed apps */}
        {s.resumedApps.length > 0 && (
          <>
            <Divider />
            <div className="text-xs font-semibold text-surface-400">Resumed apps</div>
            <div className="space-y-1">
              {[...s.resumedApps].sort().map((bundleId) => (
                <ResumedAppRow
                  key={bundleId}
                  bundleId={bundleId}
                  isFocused={s.focusedApp === bundleId}
                  onRemove={() => daemon.setAppPaused(bundleId, null)}
                />
              ))}
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function appName(bundleId: string | null): string {
  if (!bundleId) return "—";
  const base = bundleId.split(/[\\/]/).pop() ?? bundleId;
  return base.replace(/\.exe$/i, "");
}

function FocusedAppCard({ daemon }: { daemon: DaemonApi }) {
  const s = daemon.state;
  const bundleId = s.focusedApp;
  const isResumed = bundleId ? s.resumedApps.includes(bundleId) : false;
  const isPaused = s.isPaused;

  let stateText: string;
  if (s.isMasterPaused) stateText = 'Globally paused — use "Lift pause" above to resume all apps.';
  else if (s.isIdleAutoPaused) stateText = "Idle timeout — speak or switch to a resumed app to wake listening.";
  else if (isResumed && isPaused) stateText = "On the allowlist but paused for another reason.";
  else if (isResumed) stateText = "Voice typing is active here.";
  else stateText = "Paused (not on the allowlist).";

  return (
    <Card className="mt-2.5">
      <div className="flex items-center gap-2.5">
        <svg className={`w-5 h-5 ${isResumed && !isPaused ? "text-green-500" : "text-orange-400"}`} viewBox="0 0 24 24" fill="none" stroke="currentColor">
          {isResumed && !isPaused ? (
            <><circle cx="12" cy="12" r="9" strokeWidth={2} /><path strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" d="M8 12l2.5 2.5L16 9" /></>
          ) : (
            <><circle cx="12" cy="12" r="9" strokeWidth={2} /><path strokeWidth={2} strokeLinecap="round" d="M10 9v6m4-6v6" /></>
          )}
        </svg>
        <div className="flex-1 min-w-0">
          <Caption>{bundleId ? "Currently focused" : "No focused app"}</Caption>
          <div className="text-[13px] font-semibold text-white truncate">{appName(bundleId)}</div>
          <Caption>{stateText}</Caption>
        </div>
        {bundleId && (
          <Btn onClick={() => daemon.setAppPaused(bundleId, isResumed ? null : false)}>
            {isResumed ? "Remove from allowlist" : "Resume for this app"}
          </Btn>
        )}
      </div>
    </Card>
  );
}

function ResumedAppRow({ bundleId, isFocused, onRemove }: {
  bundleId: string;
  isFocused: boolean;
  onRemove: () => void;
}) {
  return (
    <div className={`flex items-center gap-2 px-2 py-1 rounded ${isFocused ? "bg-primary-500/10" : ""}`}>
      <svg
        className={`w-3 h-3 shrink-0 ${isFocused ? "text-primary-400" : "text-green-500"}`}
        viewBox="0 0 24 24" fill="currentColor"
      >
        {isFocused ? <path d="M8 5v14l11-7z" /> : <path d="M12 2a10 10 0 100 20 10 10 0 000-20zm-1.5 14.5l-4-4 1.4-1.4 2.6 2.6 5.6-5.6 1.4 1.4-7 7z" />}
      </svg>
      <span className="text-[13px] text-surface-200 truncate flex-1">{appName(bundleId)}</span>
      <button
        type="button"
        title="Remove from allowlist (this app will be paused again)"
        onClick={onRemove}
        className="text-surface-500 hover:text-red-400"
      >
        <svg className="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor">
          <path strokeWidth={2} strokeLinecap="round" d="M3 6h18M8 6V4h8v2m-9 0l1 14h8l1-14" />
        </svg>
      </button>
    </div>
  );
}
