import { useEffect, useState } from "react";
import { ENROLLMENT_STEPS, type DaemonApi } from "../hooks/useDaemonState";
import { openUrl, platform } from "../lib/tauri";
import { Btn, Caption, LevelMeter, Spinner } from "./controls";

type Step = "welcome" | "permissions" | "voice" | "tips" | "ready";
const STEPS: Step[] = ["welcome", "permissions", "voice", "tips", "ready"];

/// First-run onboarding. Mirrors OnboardingView.swift: welcome → permission
/// grants → pick the apps Always resumes in (allowlist) → done.
export function OnboardingView({
  daemon,
  onComplete,
  onOpenSettings,
}: {
  daemon: DaemonApi;
  onComplete: () => void;
  onOpenSettings: () => void;
}) {
  const [step, setStep] = useState<Step>("welcome");
  const idx = STEPS.indexOf(step);
  const next = () => setStep(STEPS[Math.min(idx + 1, STEPS.length - 1)]);
  const back = () => setStep(STEPS[Math.max(idx - 1, 0)]);
  const enrolledSteps = daemon.state.voiceSteps;
  const allVoiceStepsRecorded = ENROLLMENT_STEPS.every((s) => enrolledSteps.includes(s.key));

  useEffect(() => {
    daemon.setupMode(true);
    return () => daemon.setupMode(false);
    // The daemon command sender is stable in behavior but recreated with the API object.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (daemon.state.isDaemonConnected) daemon.getVoiceProfileStatus();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [daemon.state.isDaemonConnected]);

  useEffect(() => {
    if (allVoiceStepsRecorded && !daemon.state.voiceEnabled) daemon.setVoiceProfileEnabled(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [allVoiceStepsRecorded]);

  return (
    <div className="min-h-screen bg-surface-950 flex flex-col items-center justify-center p-6">
      <div className="w-full max-w-lg">
        <div className="text-center mb-8">
          <div className="inline-flex items-center justify-center w-16 h-16 rounded-2xl bg-primary-600 mb-4">
            <svg className="w-8 h-8 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19 11a7 7 0 01-7 7m0 0a7 7 0 01-7-7m7 7v4m0 0H8m4 0h4m-4-8a3 3 0 01-3-3V5a3 3 0 116 0v6a3 3 0 01-3 3z" />
            </svg>
          </div>
          <h1 className="text-2xl font-bold text-white">Always</h1>
        </div>

        <div className="flex justify-center gap-2 mb-8">
          {STEPS.map((s, i) => (
            <div
              key={s}
              className={`h-1.5 rounded-full transition-all duration-300 ${
                i <= idx ? "w-8 bg-primary-500" : "w-4 bg-surface-700"
              }`}
            />
          ))}
        </div>

        <div className="bg-surface-900 rounded-2xl p-6 border border-white/10 animate-fade-in min-h-[300px]">
          {step === "welcome" && <StepWelcome />}
          {step === "permissions" && <StepPermissions daemon={daemon} />}
          {step === "voice" && <StepVoice daemon={daemon} />}
          {step === "tips" && <StepTips />}
          {step === "ready" && <StepReady daemon={daemon} />}
        </div>

        <div className="flex gap-3 mt-6">
          {idx > 0 && step !== "ready" && (
            <button
              onClick={back}
              className="px-4 py-2.5 text-sm font-medium text-surface-300 hover:text-white bg-surface-800 hover:bg-surface-700 rounded-lg transition-colors"
            >
              Back
            </button>
          )}
          {step !== "ready" && (
            <button
              onClick={next}
              className="flex-1 px-4 py-2.5 text-sm font-semibold text-white bg-primary-600 hover:bg-primary-500 rounded-lg transition-colors"
            >
              {step === "welcome" ? "Get Started" : step === "voice" && !allVoiceStepsRecorded ? "Continue without it" : "Continue"}
            </button>
          )}
          {step === "ready" && (
            <button
              onClick={onComplete}
              className="flex-1 px-4 py-2.5 text-sm font-semibold text-white bg-primary-600 hover:bg-primary-500 rounded-lg transition-colors"
            >
              Launch Always
            </button>
          )}
        </div>

        {step === "ready" && (
          <button
            onClick={onOpenSettings}
            className="mt-3 w-full px-4 py-2 text-sm font-medium text-surface-400 hover:text-white transition-colors"
          >
            Open Settings instead
          </button>
        )}
        {step === "permissions" && (
          <button
            onClick={next}
            className="mt-3 w-full px-4 py-2 text-xs text-surface-500 hover:text-surface-300 transition-colors"
          >
            Skip for now — I'll do it later in Settings
          </button>
        )}
        {step === "voice" && !allVoiceStepsRecorded && (
          <button
            onClick={() => {
              if (window.confirm("Voice enrollment helps Always ignore other people, videos, and calls. Skip voice setup for now?")) next();
            }}
            className="mt-3 w-full px-4 py-2 text-xs text-surface-500 hover:text-surface-300 transition-colors"
          >
            Skip voice setup for now
          </button>
        )}
        {step === "ready" && (
          <button
            onClick={onComplete}
            className="mt-3 w-full px-4 py-2 text-xs text-surface-500 hover:text-surface-300 transition-colors"
          >
            Keep me paused everywhere for now
          </button>
        )}
      </div>
    </div>
  );
}

function StepWelcome() {
  return (
    <div className="animate-slide-up">
      <h2 className="text-xl font-semibold text-white mb-3">Welcome to Always</h2>
      <p className="text-surface-300 text-sm leading-relaxed mb-4">
        Always is a speech-to-text assistant that listens, transcribes, and types your words
        wherever your cursor is. You stay in control: it only listens in the apps you allow.
      </p>
      <div className="space-y-2">
        {[
          "Dictate in any app — transcribed text is pasted at the cursor",
          "Paused by default — you choose which apps can listen",
          "Local or Groq cloud transcription, optional post-processing",
          "Optional “My Voice” speaker verification",
        ].map((t) => (
          <div key={t} className="flex items-center gap-3 text-sm text-surface-300">
            <CheckIcon />
            <span>{t}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function StepPermissions({ daemon }: { daemon: DaemonApi }) {
  const [os, setOs] = useState("windows");
  const [checking, setChecking] = useState(false);
  useEffect(() => {
    platform().then(setOs).catch(() => {});
  }, []);

  const micOk = daemon.state.isDaemonConnected;
  const shortcutsOk = daemon.state.inputMonitoringGranted;

  const recheck = () => {
    setChecking(true);
    setTimeout(() => setChecking(false), 800);
  };

  return (
    <div className="animate-slide-up">
      <h2 className="text-xl font-semibold text-white mb-3">Permissions</h2>
      <p className="text-surface-300 text-sm leading-relaxed mb-4">
        Always needs access to your microphone. {os === "macos" ? "It also needs Input Monitoring for global shortcuts." : os === "linux" ? "Global shortcuts are available in X11 sessions." : ""}
      </p>
      <div className="space-y-2.5">
        <PermissionCard
          ok={micOk}
          title="Microphone"
          detail={
            micOk
              ? "Granted — the daemon is hearing audio."
              : os === "windows"
                ? "If dictation is silent, allow desktop apps under Settings → Privacy & security → Microphone."
                : "Required to hear you speak."
          }
          actionTitle={micOk ? undefined : os === "windows" ? "Open Microphone Settings" : os === "macos" ? "Open Settings" : undefined}
          onAction={
            micOk
              ? undefined
              : () =>
                  openUrl(
                    os === "windows"
                      ? "ms-settings:privacy-microphone"
                      : os === "macos"
                        ? "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
                        : "",
                  )
          }
        />
        <PermissionCard
          ok={shortcutsOk}
          title={os === "macos" ? "Input Monitoring" : "Global Shortcuts"}
          detail={
            shortcutsOk
              ? "Active — global hotkeys are working."
              : os === "linux"
                ? "X11 is required for global shortcuts. On Wayland, use the CLI to toggle pause and auto-enter."
                : os === "windows"
                  ? "Another app may already own one of the global shortcut chords."
                  : "Required for the global hotkeys to work."
          }
          actionTitle={
            !shortcutsOk && os === "macos" ? "Open Settings" : undefined
          }
          onAction={
            !shortcutsOk && os === "macos"
              ? () => openUrl("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
              : undefined
          }
        />
      </div>
      <div className="mt-4">
        <Btn small onClick={recheck} disabled={checking}>
          {checking ? <Spinner /> : null}
          Re-check
        </Btn>
      </div>
    </div>
  );
}

function PermissionCard({ ok, title, detail, actionTitle, onAction }: {
  ok: boolean;
  title: string;
  detail: string;
  actionTitle?: string;
  onAction?: () => void;
}) {
  return (
    <div className="flex items-start gap-3 rounded-lg bg-white/[0.06] p-3">
      <span className={`mt-0.5 ${ok ? "text-green-500" : "text-orange-400"}`}>
        {ok ? <CheckIcon /> : (
          <svg className="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path strokeWidth={2} strokeLinecap="round" d="M12 8v5m0 3h.01M10.3 3.9L1.8 18a2 2 0 001.7 3h17a2 2 0 001.7-3L13.7 3.9a2 2 0 00-3.4 0z" />
          </svg>
        )}
      </span>
      <div className="flex-1 min-w-0">
        <div className="text-sm font-medium text-surface-100">{title}</div>
        <Caption>{detail}</Caption>
      </div>
      {actionTitle && onAction && <Btn small onClick={onAction}>{actionTitle}</Btn>}
    </div>
  );
}

/// Candidate apps = whatever is focused now plus anything already resumed —
/// the Windows analogue of OnboardingAppCandidates' "running apps" list.
function StepApps({ daemon, compact = false }: { daemon: DaemonApi; compact?: boolean }) {
  const s = daemon.state;
  const focused = s.focusedApp;
  const candidates = [...new Set([...(focused ? [focused] : []), ...s.resumedApps])];

  const toggle = (bundleId: string) => {
    daemon.setAppPaused(bundleId, s.resumedApps.includes(bundleId) ? null : false);
  };

  return (
    <div className="animate-slide-up">
      {!compact && (
        <>
          <h2 className="text-xl font-semibold text-white mb-3">Choose your apps</h2>
          <p className="text-surface-300 text-sm leading-relaxed mb-4">
            Always stays paused except in apps you allow. Tick an app to resume voice typing while it's focused —
            you can change this any time in Settings → General.
          </p>
        </>
      )}
      {candidates.length === 0 ? (
        <div className="rounded-lg bg-white/[0.06] p-5 text-center">
          <Caption className="italic">
            No apps detected yet. Focus the app you want to dictate in, then come back — it'll show up here.
          </Caption>
        </div>
      ) : (
        <div className="space-y-1.5">
          {candidates.map((bundleId) => {
            const on = s.resumedApps.includes(bundleId);
            return (
              <button
                key={bundleId}
                type="button"
                onClick={() => toggle(bundleId)}
                className="w-full flex items-center gap-3 rounded-lg bg-white/[0.06] px-3 py-2.5 text-left hover:bg-surface-800"
              >
                <span
                  className={`w-4 h-4 rounded border flex items-center justify-center ${
                    on ? "bg-primary-500 border-primary-500" : "border-surface-600"
                  }`}
                >
                  {on && (
                    <svg className="w-3 h-3 text-white" viewBox="0 0 24 24" fill="none" stroke="currentColor">
                      <path strokeWidth={3} strokeLinecap="round" d="M5 13l4 4L19 7" />
                    </svg>
                  )}
                </span>
                <span className="text-sm text-surface-100 flex-1 truncate">
                  {(bundleId.split(/[\\/]/).pop() ?? bundleId).replace(/\.exe$/i, "")}
                </span>
                {bundleId === focused && (
                  <Caption className="text-primary-400">focused now</Caption>
                )}
              </button>
            );
          })}
        </div>
      )}
      <Caption className="mt-3">
        {s.resumedApps.length} {s.resumedApps.length === 1 ? "app" : "apps"} will listen.
      </Caption>
    </div>
  );
}

function StepVoice({ daemon }: { daemon: DaemonApi }) {
  const s = daemon.state;
  const enrolledSteps = s.voiceSteps;
  const recording = s.enrollment.recordingStep;
  const allEnrolled = ENROLLMENT_STEPS.every((step) => enrolledSteps.includes(step.key));

  return (
    <div className="animate-slide-up space-y-2.5">
      <h2 className="text-xl font-semibold text-white mb-2">Teach Always your voice</h2>
      <Caption>Optional, but it helps Always ignore other people, videos, and calls. Record three short samples; you can also do this later in Settings.</Caption>
      {!s.isDaemonConnected && (
        <div className="rounded-md bg-orange-500/10 border border-orange-500/30 px-3 py-2 text-xs text-orange-400">
          Daemon not connected — voice enrollment needs the running daemon.
        </div>
      )}
      {s.enrollment.lastError && (
        <div className="rounded-md bg-red-500/10 border border-red-500/30 px-3 py-2 text-xs text-red-400">{s.enrollment.lastError}</div>
      )}
      <div className="space-y-2">
        {ENROLLMENT_STEPS.map((sample) => {
          const done = enrolledSteps.includes(sample.key);
          const active = recording === sample.key;
          return (
            <div key={sample.key} className={`rounded-lg p-3 ${active ? "bg-primary-500/10 ring-1 ring-primary-500/30" : "bg-white/[0.06]"}`}>
              <div className="flex items-center gap-3">
                <span className={`text-sm ${done ? "text-green-500" : "text-surface-500"}`}>{done ? "✓" : "○"}</span>
                <div className="flex-1 min-w-0">
                  <div className="text-sm font-medium text-surface-100">{sample.title}</div>
                  <Caption>{active ? (s.enrollment.isPreparing ? "Preparing voice model…" : "Listening — keep talking…") : sample.prompt}</Caption>
                </div>
                {active ? (
                  <Btn onClick={daemon.cancelVoiceEnrollment}>Cancel</Btn>
                ) : (
                  <Btn disabled={!s.isDaemonConnected || recording !== null || s.enrollment.isPreparing} onClick={() => daemon.startVoiceEnrollment(sample.key)}>
                    {done ? "Re-record" : "Record"}
                  </Btn>
                )}
              </div>
              {active && (
                <div className="mt-2 flex items-center gap-2 pl-6">
                  {s.enrollment.isPreparing ? <><Spinner /><Caption>Starting microphone…</Caption></> : <><LevelMeter level={s.enrollment.level} /><Caption>{(s.enrollment.voicedMs / 1000).toFixed(1)}s{s.enrollment.targetMs > 0 ? ` / ${(s.enrollment.targetMs / 1000).toFixed(0)}s` : ""}</Caption></>}
                </div>
              )}
            </div>
          );
        })}
      </div>
      {allEnrolled && <Caption className="text-green-400">All three recorded — Always will only listen to your voice.</Caption>}
    </div>
  );
}

function StepTips() {
  return (
    <div className="animate-slide-up space-y-3">
      <h2 className="text-xl font-semibold text-white mb-2">Tips & shortcuts</h2>
      <Caption>Auto-enter is on by default. You can adjust it and remap shortcuts any time in Settings.</Caption>
      {[
        ["Ctrl+Alt+P", "Pause or resume for this app", "Pause dictation in the focused app without changing the rest of your allowlist."],
        ["Ctrl+Alt+Shift+P", "Pause everywhere", "Use the master pause when you want Always silent in every app."],
        ["Ctrl+Alt+A", "Toggle auto-enter", "When enabled, Always presses Enter after typing your words."],
        ["Ctrl+Alt+V", "Paste filtered text anyway", "Use this when you want the raw transcript instead of filtered text."],
        ["Ctrl+Alt+W", "Correct & improve your vocabulary", "Fix the last transcript; Always can learn the correction for next time."],
      ].map(([shortcut, title, detail]) => (
        <div key={shortcut} className="flex items-start gap-3 rounded-lg bg-white/[0.06] p-3">
          <kbd className="shrink-0 text-primary-200 bg-primary-500/20 px-2 py-1 rounded text-xs">{shortcut}</kbd>
          <div><div className="text-sm font-medium text-surface-100">{title}</div><Caption>{detail}</Caption></div>
        </div>
      ))}
    </div>
  );
}

function StepReady({ daemon }: { daemon: DaemonApi }) {
  return (
    <div className="animate-slide-up">
      <h2 className="text-xl font-semibold text-white mb-3">You're all set!</h2>
      <p className="text-surface-300 text-sm leading-relaxed mb-4">
        Choose the apps where Always should type. Every other app stays paused. You can change this any time in Settings.
      </p>
      <StepApps daemon={daemon} compact />
    </div>
  );
}

function CheckIcon() {
  return (
    <svg className="w-4 h-4 text-primary-500 flex-shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor">
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M5 13l4 4L19 7" />
    </svg>
  );
}
