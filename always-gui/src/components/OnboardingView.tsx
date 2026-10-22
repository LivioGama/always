import { useEffect, useState } from "react";
import { ENROLLMENT_STEPS, type DaemonApi } from "../hooks/useDaemonState";
import { EngineStep } from "./onboarding/EngineStep";
import { Icon, type IconName } from "./onboarding/icons";
import { SOLID, type Tone } from "./onboarding/rows";
import { PermissionsStep, ReadyStep, TipsStep, VoiceStep, WelcomeStep } from "./onboarding/steps";

type Step = "welcome" | "engine" | "permissions" | "voice" | "tips" | "ready";
// Engine before permissions: a model download runs while the user grants them.
const STEPS: Step[] = ["welcome", "engine", "permissions", "voice", "tips", "ready"];

const META: Record<Step, { icon: IconName; tone: Tone; title: string; subtitle: string }> = {
  welcome: { icon: "waveform", tone: "accent", title: "Welcome to Always", subtitle: "Always listens to your voice and types what you say — in any app. Let's get set up in a few quick steps." },
  permissions: { icon: "lock", tone: "orange", title: "Grant Permissions", subtitle: "Always needs access to your microphone. This window updates automatically once you grant it." },
  engine: { icon: "cpu", tone: "teal", title: "Choose Your Engine", subtitle: "Pick how your speech becomes text. You can change this any time." },
  voice: { icon: "micOff", tone: "purple", title: "Teach It Your Voice", subtitle: "Optional, but it stops Always transcribing other people, videos and calls." },
  tips: { icon: "bulb", tone: "yellow", title: "Tips & Shortcuts", subtitle: "A few shortcuts that make Always feel invisible. Memorize these — you'll use them daily." },
  ready: { icon: "seal", tone: "green", title: "You're All Set", subtitle: "Pick the apps Always types into, then start speaking. You can revisit any of this in Settings." },
};

const bigBtn = "min-w-[120px] px-5 py-2.5 text-sm rounded-lg transition-colors";
const primaryBtn = `${bigBtn} font-semibold text-white bg-primary-600 hover:bg-primary-500`;
const plainBtn = `${bigBtn} font-medium text-surface-200 bg-surface-800 hover:bg-surface-700 border border-surface-700`;

/// First-run onboarding. Mirrors OnboardingView.swift (fixed 580x640 window).
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
  const [confirmSkip, setConfirmSkip] = useState(false);
  const idx = STEPS.indexOf(step);
  const next = () => setStep(STEPS[Math.min(idx + 1, STEPS.length - 1)]);
  const back = () => setStep(STEPS[Math.max(idx - 1, 0)]);
  const allVoiceStepsRecorded = ENROLLMENT_STEPS.every((s) => daemon.state.voiceSteps.includes(s.key));
  const m = META[step];

  // Engine-step choice that can't apply yet: a model still downloading, or
  // Groq waiting for the restart that loads its key. Re-applied on every
  // catalogue refresh (polled every 5 s and on reconnect) until the daemon
  // confirms it, so neither leaving the step nor a daemon restart (which
  // kills an in-flight download) loses it. Dropped when it fails.
  const [pending, setPending] = useState<string | null>(null);
  const s = daemon.state;
  const pendingError = pending ? s.modelErrors[pending.startsWith("local:") ? pending.slice(6) : pending] : undefined;
  const pick = (backend: string | null) => {
    // Clears a stale error in the same render, so it can't cancel the pick.
    if (backend === "groq") daemon.setActiveTranscriber("groq");
    setPending(backend);
  };
  useEffect(() => {
    if (pendingError) setPending(null);
  }, [pendingError]);
  useEffect(() => {
    if (!pending || !s.isDaemonConnected) return;
    if (s.backend === pending) return setPending(null);
    if (pending === "groq") return daemon.setActiveTranscriber("groq");
    const model = s.models.find((m) => `local:${m.id}` === pending);
    if (!model) return;
    if (model.is_downloaded && !model.is_downloading) daemon.setActiveTranscriber(pending);
    // A restarted daemon dropped the download: resume it from the partial
    // file. A duplicate request is a no-op in the daemon.
    else if (!model.is_downloading) daemon.downloadModel(model.id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pending, s.models, s.backend, s.isDaemonConnected]);

  useEffect(() => {
    daemon.setupMode(true);
    return () => daemon.setupMode(false);
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
    <div className="min-h-screen bg-surface-950 flex items-center justify-center">
      <div className="w-[580px] h-[640px] flex flex-col bg-surface-950 text-white overflow-hidden">
        <div className="flex-1 min-h-0 flex flex-col items-center gap-5 pt-9 px-8">
          {step === "welcome" ? (
            <span className="inline-flex items-center justify-center w-[88px] h-[88px] rounded-[20px] bg-primary-600 text-white shadow-lg shadow-primary-500/30 shrink-0">
              <Icon name="mic" className="w-11 h-11" />
            </span>
          ) : step !== "ready" ? (
            <span className={`inline-flex items-center justify-center w-[72px] h-[72px] rounded-full text-white shadow-lg shrink-0 ${SOLID[m.tone]}`}>
              <Icon name={m.icon} className="w-8 h-8" stroke={1.5} />
            </span>
          ) : null}
          <h1 className="text-[28px] leading-tight font-bold text-center">{m.title}</h1>
          <p className="text-[13px] text-surface-400 text-center px-6">{m.subtitle}</p>
          <div className="w-full flex-1 min-h-0 flex flex-col overflow-y-auto">
            {step === "welcome" && <WelcomeStep />}
            {step === "permissions" && <PermissionsStep daemon={daemon} />}
            {step === "engine" && <EngineStep daemon={daemon} onPick={pick} />}
            {step === "voice" && <VoiceStep daemon={daemon} />}
            {step === "tips" && <TipsStep daemon={daemon} />}
            {step === "ready" && <ReadyStep daemon={daemon} />}
          </div>
        </div>

        <div className="flex justify-center items-center gap-2 pb-4 pt-2">
          {STEPS.map((s) => (
            <span key={s} title={s} className={`h-2 rounded-full transition-all duration-300 ${s === step ? "w-6 bg-primary-500" : "w-2 bg-white/25"}`} />
          ))}
        </div>

        <div className="flex items-center gap-3 px-6 pb-6">
          {step !== "welcome" && step !== "ready" && <button onClick={back} className={plainBtn}>Back</button>}
          {step === "ready" && (
            <button onClick={onOpenSettings} className="px-3 py-2 text-sm text-surface-400 hover:text-white transition-colors">
              Open Settings
            </button>
          )}
          <div className="flex-1" />
          {step === "voice" && (
            <div className="flex gap-2">
              <button onClick={() => setConfirmSkip(true)} className={plainBtn}>Skip for now</button>
              <button onClick={next} className={primaryBtn}>{allVoiceStepsRecorded ? "Continue" : "Continue without it"}</button>
            </div>
          )}
          {step === "ready" && <button onClick={onComplete} className={primaryBtn}>Start Using Always</button>}
          {step !== "voice" && step !== "ready" && <button onClick={next} className={primaryBtn}>Continue</button>}
        </div>

        {confirmSkip && (
          <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm animate-fade-in">
            <div className="bg-surface-900 rounded-2xl border border-surface-700 shadow-2xl p-6 w-full max-w-sm animate-slide-up">
              <h3 className="text-lg font-semibold text-white mb-3">Are you sure?</h3>
              <p className="text-sm text-surface-300 mb-4">
                Voice enrollment makes a big difference. It lets Always ignore other people, videos, and calls — so it only types what you say. We really recommend it.
              </p>
              <div className="flex gap-2">
                <button onClick={() => setConfirmSkip(false)} className={`flex-1 ${plainBtn}`}>Set up my voice</button>
                <button onClick={() => { setConfirmSkip(false); next(); }} className={`flex-1 ${primaryBtn}`}>Skip voice setup</button>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
