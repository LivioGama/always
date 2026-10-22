import { useEffect, useState } from "react";
import { ENROLLMENT_STEPS, type DaemonApi } from "../../hooks/useDaemonState";
import { configShow, openUrl, platform } from "../../lib/tauri";
import { DEFAULT_CONFIG, parseConfig, type Config } from "../../lib/config";
import { formatShortcut, shortcutsNeedDesktop, visibleShortcuts } from "../../lib/shortcuts";
import { Btn, Caption } from "../controls";
import { Icon, type IconName } from "./icons";
import { Granted, IconCircle, RowCard, RowText, SOLID, type Tone } from "./rows";

export function WelcomeStep() {
  const items: [IconName, string, string][] = [
    ["mic", "Speak naturally", "Always transcribes your voice in real time."],
    ["keyboard", "Types anywhere", "Your words appear in whatever app is focused."],
    ["person", "Only your voice", "Optional voice profile ignores other speakers."],
  ];
  return (
    <div className="flex flex-col gap-3 px-5 pt-2">
      {items.map(([icon, title, detail]) => (
        <RowCard key={title} className="py-2.5">
          <IconCircle icon={icon} tone="accent" size={36} />
          <RowText title={title} detail={detail} />
        </RowCard>
      ))}
    </div>
  );
}

function PermissionCard({ icon, tone, title, detail, granted, actionTitle, onAction }: {
  icon: IconName; tone: Tone; title: string; detail: string; granted: boolean; actionTitle?: string; onAction?: () => void;
}) {
  return (
    <RowCard ok={granted} className="py-3.5">
      <IconCircle icon={granted ? "check" : icon} tone={granted ? "green" : tone} />
      <RowText title={title} detail={detail} />
      {granted ? <Granted /> : actionTitle && onAction ? <Btn prominent onClick={onAction}>{actionTitle}</Btn> : null}
    </RowCard>
  );
}

export function PermissionsStep({ daemon }: { daemon: DaemonApi }) {
  const [os, setOs] = useState("");
  useEffect(() => {
    platform().then(setOs).catch(() => {});
  }, []);
  const micOk = daemon.state.isDaemonConnected;
  const shortcutsOk = daemon.state.inputMonitoringGranted;
  return (
    <div className="flex flex-col gap-3 px-5">
      <PermissionCard
        icon="mic" tone="red" title="Microphone" granted={micOk}
        detail={micOk ? "Granted — the daemon is hearing audio." : os === "windows" ? "If dictation is silent, allow desktop apps under Settings → Privacy & security → Microphone." : "Required — this is how Always hears you."}
        actionTitle={os === "windows" ? "Open Settings" : undefined}
        onAction={() => openUrl("ms-settings:privacy-microphone")}
      />
      <PermissionCard
        icon="command" tone="indigo" title="Global Shortcuts" granted={shortcutsOk}
        detail={shortcutsOk ? "Active — global hotkeys are working." : os === "linux" ? "Optional — needs X11. On Wayland, use the CLI to toggle pause and auto-enter." : os === "windows" ? "Optional — another app may already own one of the shortcut chords." : "Optional — enables global shortcuts."}
      />
      {!micOk && (
        <Caption className="text-center px-5 pt-1">This window updates on its own once access is granted.</Caption>
      )}
    </div>
  );
}

export function VoiceStep({ daemon }: { daemon: DaemonApi }) {
  const s = daemon.state;
  const e = s.enrollment;
  const recording = e.recordingStep;
  const all = ENROLLMENT_STEPS.every((x) => s.voiceSteps.includes(x.key));
  return (
    <div className="flex flex-col gap-3 px-5">
      {ENROLLMENT_STEPS.map((x) => {
        const done = s.voiceSteps.includes(x.key);
        const active = recording === x.key;
        return (
          <RowCard key={x.key} className="py-3">
            <IconCircle icon={done ? "check" : "mic"} tone={done ? "green" : "purple"} />
            <RowText title={x.title} detail={active ? (e.isPreparing ? "Preparing voice model…" : "Listening — keep talking…") : x.prompt} />
            {active ? (
              <>
                {!e.isPreparing && e.targetMs > 0 && (
                  <div className="w-[60px] h-1.5 rounded-full bg-white/10 overflow-hidden">
                    <div className="h-full bg-primary-500" style={{ width: `${Math.min(100, (e.voicedMs / e.targetMs) * 100)}%` }} />
                  </div>
                )}
                <Btn onClick={daemon.cancelVoiceEnrollment}>Cancel</Btn>
              </>
            ) : (
              <Btn disabled={!s.isDaemonConnected || recording !== null || e.isPreparing} onClick={() => daemon.startVoiceEnrollment(x.key)}>
                {done ? "Re-record" : "Record"}
              </Btn>
            )}
          </RowCard>
        );
      })}
      {all ? (
        <p className="flex items-center justify-center gap-1.5 text-xs text-green-500 pt-1">
          <Icon name="seal" className="w-3.5 h-3.5" /> All three recorded — Always will only listen to you.
        </p>
      ) : (
        <Caption className="text-center px-5">Record all three and Always ignores every voice but yours. You can do this later in Settings.</Caption>
      )}
      {e.lastError && <p className="text-xs text-red-400 px-5">{e.lastError}</p>}
    </div>
  );
}

const TIPS: { key: keyof Config; tone: Tone; title: string; detail: (combo: string) => string }[] = [
  { key: "shortcutAutoEnter", tone: "blue", title: "Auto-enter is on", detail: (k) => `Your text is typed and sent automatically. Press ${k} to toggle the final Enter — your words appear but nothing gets sent.` },
  { key: "shortcutForcePaste", tone: "orange", title: "Paste filtered text anyway", detail: (k) => `Always filters profanity and filler words. If you want the raw, unfiltered transcript pasted at your cursor, press ${k}.` },
  { key: "shortcutCorrectionDialog", tone: "green", title: "Correct & feed your vocabulary", detail: (k) => `Said something and the transcript got it wrong? Press ${k}, type the correction, and Always learns the fix for next time.` },
];

export function TipsStep({ daemon }: { daemon: DaemonApi }) {
  const [os, setOs] = useState("");
  const [config, setConfig] = useState<Config>(DEFAULT_CONFIG);
  useEffect(() => {
    platform().then(setOs).catch(() => {});
    configShow().then((out) => setConfig(parseConfig(out))).catch(() => {});
  }, []);
  // The keys actually configured, not a fixed list. On Wayland Always cannot
  // listen for keys: show the command each one has to run instead.
  const needDesktop = shortcutsNeedDesktop(os, daemon.state.inputMonitoringGranted);
  if (needDesktop) {
    return (
      <div className="flex flex-col gap-3 px-5">
        <Caption className="text-center">
          On Wayland Always cannot listen for keys itself. Add these to your desktop's keyboard shortcuts (on Hyprland: a bind per line in your config) — the keys are suggestions, choose your own.
        </Caption>
        {visibleShortcuts(config, os, true).map((a) => (
          <RowCard key={a.key} className="py-3.5 border-white/[0.12]">
            <span className={`shrink-0 rounded-lg px-3 py-1.5 font-mono text-[13px] font-bold text-white shadow-md ${SOLID.blue}`}>{a.combo}</span>
            <div className="flex flex-col gap-1 min-w-0">
              <RowText bold title={a.label} detail={a.detail} />
              {a.command && <code className="self-start text-[11px] text-surface-200 bg-black/30 px-1.5 py-0.5 rounded">{a.command}</code>}
            </div>
          </RowCard>
        ))}
      </div>
    );
  }
  const tips = TIPS.map((t) => ({ ...t, combo: formatShortcut(config[t.key] as string) }));
  const pauseAll = formatShortcut(config.shortcutMasterPause as string);
  return (
    <div className="flex flex-col gap-3 px-5">
      {tips.map((t) =>
        t.combo ? (
          <RowCard key={t.key} className="py-3.5 border-white/[0.12]">
            <span className={`shrink-0 rounded-lg px-3 py-1.5 font-mono text-[13px] font-bold text-white shadow-md ${SOLID[t.tone]}`}>{t.combo}</span>
            <RowText bold title={t.title} detail={t.detail(t.combo)} />
          </RowCard>
        ) : null,
      )}
      {pauseAll && <Caption className="text-center px-5">{pauseAll} pauses everywhere.</Caption>}
    </div>
  );
}

/// Final step: the apps voice typing is ON in (Linux analogue of OnboardingAppAllowlist).
export function ReadyStep({ daemon }: { daemon: DaemonApi }) {
  const s = daemon.state;
  const candidates = [...new Set([...(s.focusedApp ? [s.focusedApp] : []), ...s.resumedApps])];
  const toggle = (id: string) => daemon.setAppPaused(id, s.resumedApps.includes(id) ? null : false);
  const name = (id: string) => (id.split(/[\\/]/).pop() ?? id).replace(/\.exe$/i, "");
  return (
    <div className="flex flex-col items-center gap-4 px-5 min-h-0">
      <div className="relative mt-1 shrink-0">
        <span className="inline-flex items-center justify-center w-20 h-20 rounded-[18px] bg-primary-600 text-white shadow-lg shadow-green-500/30">
          <Icon name="mic" className="w-10 h-10" />
        </span>
        <span className="absolute -right-2 -bottom-2 rounded-full bg-surface-950 text-green-500">
          <Icon name="checkCircle" className="w-[26px] h-[26px]" />
        </span>
      </div>
      <div className="w-full max-w-[440px] flex flex-col gap-2 min-h-0">
        <h3 className="text-base font-semibold text-white">Turn voice typing on in:</h3>
        {candidates.length === 0 ? (
          <Caption>Focus the app you want to dictate into — it will show up here.</Caption>
        ) : (
          <div className="flex flex-col gap-2 overflow-y-auto max-h-[150px]">
            {candidates.map((id) => {
              const on = s.resumedApps.includes(id);
              return (
                <button key={id} type="button" onClick={() => toggle(id)} className="flex items-center gap-2 text-sm text-white text-left">
                  <span className={`w-4 h-4 rounded border flex items-center justify-center shrink-0 ${on ? "bg-primary-500 border-primary-500" : "border-surface-600"}`}>
                    {on && <Icon name="check" className="w-3 h-3 text-white" stroke={3} />}
                  </span>
                  <span className="truncate">{name(id)}</span>
                  {id === s.focusedApp && <Caption className="text-primary-400">focused now</Caption>}
                </button>
              );
            })}
          </div>
        )}
        <Caption>Every other app stays paused, so your words never land somewhere you didn't choose. Change this any time in Settings.</Caption>
      </div>
    </div>
  );
}
