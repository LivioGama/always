import { useEffect, useState } from "react";
import type { DaemonApi } from "../../hooks/useDaemonState";
import { appVersion, openUrl, platform } from "../../lib/tauri";
import { Btn, Caption, Divider, PanelTitle, Spinner } from "../controls";

const REPOSITORY_URL = "https://github.com/LivioGama/always";
const DONATE_URL = "https://donate.stripe.com/aFa7sN1jZdHD6b82dY4Ja00";
const LICENSE = "AGPL-3.0";

/// Settings → About. Mirrors AboutPanel.swift exactly: app info, updates,
/// description, links, support-development (Donate), license.
export function AboutPanel({ daemon }: { daemon: DaemonApi }) {
  const [version, setVersion] = useState("");
  const [os, setOs] = useState("");

  useEffect(() => {
    appVersion().then(setVersion).catch(() => setVersion("unknown"));
    platform().then(setOs).catch(() => {});
  }, []);

  const platformName = os === "windows" ? "Windows" : os === "macos" ? "macOS" : "Linux";

  return (
    <div className="p-5 space-y-5">
      {/* App Info */}
      <div className="space-y-2 pb-2.5">
        <div className="text-[32px] font-bold text-white leading-tight">Always</div>
        <div className="text-lg text-surface-400">
          The first speech-to-text software designed to be always on and free your hands from the affordance
        </div>
        <Caption>Version {version || "unknown"}</Caption>
      </div>

      <Divider />

      <UpdateSection daemon={daemon} />

      <Divider />

      {/* Description */}
      <div className="space-y-2">
        <PanelTitle>About</PanelTitle>
        <p className="text-[13px] text-surface-400">
          Always provides continuous voice transcription using Groq's STT API with intelligent
          post-processing. It runs as a background daemon and integrates with your {platformName} workflow.
        </p>
      </div>

      <Divider />

      {/* Links */}
      <div className="space-y-2">
        <PanelTitle>Links</PanelTitle>
        <div className="flex flex-wrap gap-2.5">
          <LinkButton title="GitHub Repository" url={REPOSITORY_URL} icon="globe" />
          <LinkButton title="Documentation" url={`${REPOSITORY_URL}#readme`} icon="book" />
          <LinkButton title="Issues" url={`${REPOSITORY_URL}/issues`} icon="ant" />
        </div>
      </div>

      <Divider />

      {/* Support Development */}
      <div className="space-y-2">
        <PanelTitle>Support Development</PanelTitle>
        <p className="text-[13px] text-surface-400">
          Always is free and open source. If it saves you time, consider supporting its development.
        </p>
        <LinkButton title="Donate" url={DONATE_URL} icon="heart" prominent />
      </div>

      <Divider />

      {/* License */}
      <div className="space-y-2">
        <PanelTitle>License</PanelTitle>
        <p className="text-[13px] text-surface-400">{LICENSE}</p>
      </div>
    </div>
  );
}

function LinkButton({ title, url, icon, prominent = false }: {
  title: string;
  url: string;
  icon: "globe" | "book" | "ant" | "heart";
  prominent?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={() => openUrl(url)}
      className={`inline-flex items-center gap-1.5 px-3 py-1.5 rounded-md text-[13px] font-medium transition-colors ${
        prominent
          ? "bg-primary-600 text-white hover:bg-primary-500"
          : "bg-surface-800 text-surface-200 hover:bg-surface-700 border border-surface-700"
      }`}
    >
      <LinkIcon kind={icon} />
      {title}
    </button>
  );
}

function LinkIcon({ kind }: { kind: "globe" | "book" | "ant" | "heart" }) {
  const cls = "w-3.5 h-3.5";
  switch (kind) {
    case "heart":
      return (
        <svg className={cls} viewBox="0 0 24 24" fill="currentColor">
          <path d="M12 21s-7.5-4.6-10-9.3C.4 8.6 2.4 5 6 5c2.2 0 3.6 1.2 4.5 2.6L12 9l1.5-1.4C14.4 6.2 15.8 5 18 5c3.6 0 5.6 3.6 4 6.7C19.5 16.4 12 21 12 21z" />
        </svg>
      );
    case "globe":
      return (
        <svg className={cls} viewBox="0 0 24 24" fill="none" stroke="currentColor">
          <circle cx="12" cy="12" r="9" strokeWidth={2} />
          <path strokeWidth={2} d="M3 12h18M12 3c2.5 2.6 3.9 5.7 3.9 9s-1.4 6.4-3.9 9c-2.5-2.6-3.9-5.7-3.9-9S9.5 5.6 12 3z" />
        </svg>
      );
    case "book":
      return (
        <svg className={cls} viewBox="0 0 24 24" fill="none" stroke="currentColor">
          <path strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" d="M4 19.5A2.5 2.5 0 016.5 17H20V4a2 2 0 00-2-2H6.5A2.5 2.5 0 004 4.5v15zM20 17v5H6.5a2.5 2.5 0 010-5" />
        </svg>
      );
    case "ant":
      return (
        <svg className={cls} viewBox="0 0 24 24" fill="none" stroke="currentColor">
          <circle cx="12" cy="12" r="8" strokeWidth={2} />
          <path strokeWidth={2} strokeLinecap="round" d="M12 8v4l2 2" />
        </svg>
      );
  }
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
          <div className="text-[13px] text-surface-100">
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
