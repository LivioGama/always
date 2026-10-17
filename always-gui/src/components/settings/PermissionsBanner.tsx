import { useEffect, useState } from "react";
import { openUrl, platform } from "../../lib/tauri";
import { Btn } from "../controls";

/// Non-modal banner surfacing missing permissions inline at the top of
/// Settings. Windows equivalent of PermissionsBanner.swift — on Windows
/// there's no TCC, so the actionable row is the OS microphone privacy
/// toggle; input monitoring isn't required.
export function PermissionsBanner() {
  const [os, setOs] = useState<string | null>(null);
  const [dismissed, setDismissed] = useState(
    () => sessionStorage.getItem("always_perms_banner_dismissed") === "1",
  );

  useEffect(() => {
    platform().then(setOs).catch(() => setOs("unknown"));
  }, []);

  if (dismissed || os === null || os === "linux" || os === "unknown") return null;

  return (
    <div className="rounded-lg bg-orange-500/[0.08] border border-orange-500/35 p-2.5 space-y-2">
      <div className="flex items-center gap-2">
        <svg className="w-4 h-4 text-orange-400" viewBox="0 0 24 24" fill="none" stroke="currentColor">
          <path strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" d="M12 3l7 3v5c0 4.5-3 8.5-7 10-4-1.5-7-5.5-7-10V6l7-3zM12 9v4m0 3h.01" />
        </svg>
        <span className="text-[13px] font-semibold text-orange-400">Permissions needed</span>
        <div className="flex-1" />
        <button
          type="button"
          title="Dismiss for this session"
          onClick={() => {
            sessionStorage.setItem("always_perms_banner_dismissed", "1");
            setDismissed(true);
          }}
          className="text-surface-500 hover:text-surface-200 text-[13px] leading-none"
        >
          ×
        </button>
      </div>

      <PermRow
        title="Microphone"
        detail={
          os === "windows"
            ? "Always needs microphone access to hear you speak. If dictation is silent, check Windows Settings → Privacy & security → Microphone and allow desktop apps."
            : "Always needs microphone access to hear you speak."
        }
        actionTitle="Open Settings"
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
    </div>
  );
}

function PermRow({ title, detail, actionTitle, onAction }: {
  title: string;
  detail: string;
  actionTitle: string;
  onAction: () => void;
}) {
  return (
    <div className="flex items-start gap-2.5">
      <div className="flex-1">
        <div className="text-xs font-semibold text-surface-200">{title}</div>
        <div className="text-[10px] text-surface-500 leading-snug">{detail}</div>
      </div>
      <Btn onClick={onAction}>{actionTitle}</Btn>
    </div>
  );
}
