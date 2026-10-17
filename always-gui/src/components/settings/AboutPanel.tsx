import { useEffect, useState } from "react";
import type { DaemonApi } from "../../hooks/useDaemonState";
import { appVersion, openUrl } from "../../lib/tauri";
import { Btn, Caption, Divider, PanelTitle, Spinner } from "../controls";

const GITHUB_URL = "https://github.com/LivioGama/always";

/// Settings → About: version, update section, links. Mirrors
/// AboutPanel.swift + UpdateSection.swift.
export function AboutPanel({ daemon }: { daemon: DaemonApi }) {
  const [version, setVersion] = useState("");

  useEffect(() => {
    appVersion().then(setVersion).catch(() => setVersion("dev"));
  }, []);

  return (
    <div className="p-5 space-y-4">
      <div className="flex items-center gap-3">
        <div className="w-12 h-12 rounded-xl bg-primary-600 flex items-center justify-center">
          <svg className="w-7 h-7 text-white" viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" d="M12 3a3 3 0 013 3v5a3 3 0 01-6 0V6a3 3 0 013-3z" />
            <path strokeWidth={2} strokeLinecap="round" d="M19 10v1a7 7 0 01-14 0v-1M12 18v3" />
          </svg>
        </div>
        <div>
          <div className="text-lg font-semibold text-white">Always</div>
          <Caption>Version {version || "…"}</Caption>
        </div>
      </div>

      <Caption>
        Voice-first typing: speak, Always transcribes and pastes where your cursor is.
        Local or Groq cloud transcription, optional post-processing, per-app allowlist.
      </Caption>

      <Divider />

      <UpdateSection daemon={daemon} />

      <Divider />

      <div className="space-y-2">
        <PanelTitle>Links</PanelTitle>
        <div className="flex flex-wrap gap-2">
          <Btn onClick={() => openUrl(GITHUB_URL)}>GitHub</Btn>
          <Btn onClick={() => openUrl(`${GITHUB_URL}#readme`)}>Documentation</Btn>
          <Btn onClick={() => openUrl(`${GITHUB_URL}/issues`)}>Report an Issue</Btn>
          <Btn onClick={() => openUrl("https://github.com/sponsors/LivioGama")}>♥ Donate</Btn>
        </div>
      </div>

      <Caption>MIT License · Made by Livio Gamassia</Caption>
    </div>
  );
}

/// Mirrors UpdateSection.swift — check / available / install / progress / fail.
function UpdateSection({ daemon }: { daemon: DaemonApi }) {
  const u = daemon.state.update;

  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2">
        <PanelTitle>Updates</PanelTitle>
        <div className="flex-1" />
        <Btn
          small
          onClick={daemon.checkForUpdates}
          disabled={!daemon.state.isDaemonConnected || u.kind === "checking" || u.kind === "installing"}
        >
          {u.kind === "checking" ? <Spinner /> : null}
          Check for Updates
        </Btn>
      </div>

      {u.kind === "unknown" && (
        <Caption>The daemon checks GitHub releases. Click to check now.</Caption>
      )}
      {u.kind === "checking" && <Caption>Checking for updates…</Caption>}
      {u.kind === "upToDate" && (
        <div className="flex items-center gap-2 text-green-500">
          <svg className="w-4 h-4" viewBox="0 0 24 24" fill="currentColor">
            <path d="M12 2a10 10 0 100 20 10 10 0 000-20zm-1.5 14.5l-4-4 1.4-1.4 2.6 2.6 5.6-5.6 1.4 1.4-7 7z" />
          </svg>
          <Caption className="!text-green-400">You're up to date ({u.version}).</Caption>
        </div>
      )}
      {u.kind === "noRelease" && <Caption>No release found for this platform yet.</Caption>}
      {u.kind === "checkFailed" && (
        <Caption className="!text-red-400">Update check failed: {u.reason}</Caption>
      )}
      {u.kind === "available" && (
        <div className="rounded-lg bg-primary-500/10 border border-primary-500/30 p-2.5 space-y-2">
          <div className="text-sm text-surface-100">
            Version <span className="font-semibold text-primary-300">{u.latest}</span> is available
            <span className="text-surface-400"> (you have {u.current})</span>
          </div>
          <div className="flex items-center gap-2">
            {u.canApply ? (
              <Btn prominent small onClick={daemon.installUpdate}>Install Update</Btn>
            ) : (
              <Btn small onClick={() => openUrl(u.url)}>Download</Btn>
            )}
            {u.applyHint && <Caption>{u.applyHint}</Caption>}
          </div>
        </div>
      )}
      {u.kind === "installing" && (
        <div className="space-y-1">
          <div className="flex items-center gap-2">
            <Spinner />
            <Caption>{u.phase}{u.detail ? ` — ${u.detail}` : ""}</Caption>
          </div>
          <div className="h-1.5 rounded-full bg-surface-700/50 overflow-hidden">
            {u.percent != null ? (
              <div className="h-full bg-primary-500 transition-[width]" style={{ width: `${u.percent}%` }} />
            ) : (
              <div className="h-full w-1/3 bg-primary-500 animate-pulse" />
            )}
          </div>
        </div>
      )}
      {u.kind === "failed" && (
        <Caption className="!text-red-400">Update failed: {u.message}</Caption>
      )}
    </div>
  );
}
