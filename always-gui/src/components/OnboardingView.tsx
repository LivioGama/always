import { useState, useCallback } from "react";

interface OnboardingViewProps {
  onComplete: () => void;
  onStartSettings: () => void;
  onStartEnrollment: () => void;
  hasVoiceProfile: boolean;
  shortcutListening: boolean;
  onReloadShortcuts?: () => void;
}

type Step = 0 | 1 | 2 | 3 | 4;

const NEXT_LABELS = ["Get Started", "Record: normal", "Record: lower", "Record: louder", "Launch Always"];

export function OnboardingView({
  onComplete,
  onStartSettings,
  onStartEnrollment,
  hasVoiceProfile,
  shortcutListening,
  onReloadShortcuts,
}: OnboardingViewProps) {
  const [step, setStep] = useState<Step>(0);
  const [hotkeys, setHotkeys] = useState({
    pause: "⌥ Space",
    resume: "⌥⇧ Space",
    capture: "⌃⌥X",
    grammarCorrect: "⌥G",
    globalPause: "⌃⌥P",
    globalResume: "⌃⌥⇧P",
  });

  const handleNext = () => {
    if (step < 4) {
      if (step === 1 || step === 2) {
        onStartEnrollment();
      }
      setStep((s) => Math.min(s + 1, 4) as Step);
    } else {
      if (onReloadShortcuts) onReloadShortcuts();
      onComplete();
    }
  };

  return (
    <div className="min-h-screen bg-surface-950 flex flex-col items-center justify-center p-6">
      <div className="w-full max-w-lg">
        {/* Logo */}
        <div className="text-center mb-8">
          <div className="inline-flex items-center justify-center w-16 h-16 rounded-2xl bg-primary-600 mb-4">
            <svg className="w-8 h-8 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19 11a7 7 0 01-7 7m0 0a7 7 0 01-7-7m7 7v4m0 0H8m4 0h4m-4-8a3 3 0 01-3-3V5a3 3 0 116 0v6a3 3 0 01-3 3z" />
            </svg>
          </div>
          <h1 className="text-2xl font-bold text-white">Always</h1>
        </div>

        {/* Progress dots */}
        <div className="flex justify-center gap-2 mb-8">
          {[0, 1, 2, 3, 4].map((i) => (
            <div
              key={i}
              className={`h-1.5 rounded-full transition-all duration-300 ${
                i <= step ? "w-8 bg-primary-500" : "w-4 bg-surface-700"
              }`}
            />
          ))}
        </div>

        {/* Step content */}
        <div className="bg-surface-900 rounded-2xl p-6 border border-surface-800 animate-fade-in">
          {step === 0 && <StepWelcome />}
          {step === 1 && <StepEnrollNormal />}
          {step === 2 && <StepEnrollLower />}
          {step === 3 && <StepEnrollLouder />}
          {step === 4 && <StepDone />}
        </div>

        {/* Actions */}
        <div className="flex gap-3 mt-6">
          {step >= 2 && step <= 3 && (
            <button
              onClick={() => setStep((s) => Math.max(s - 1, 0) as Step)}
              className="px-4 py-2.5 text-sm font-medium text-surface-300 hover:text-white bg-surface-800 hover:bg-surface-700 rounded-lg transition-colors"
            >
              Back
            </button>
          )}
          <button
            onClick={handleNext}
            className="flex-1 px-4 py-2.5 text-sm font-semibold text-white bg-primary-600 hover:bg-primary-500 rounded-lg transition-colors"
          >
            {NEXT_LABELS[step]}
          </button>
        </div>

        {step === 4 && (
          <button
            onClick={onStartSettings}
            className="mt-3 w-full px-4 py-2 text-sm font-medium text-surface-400 hover:text-white transition-colors"
          >
            Open Settings instead
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
        Always is an on-device speech-to-text assistant that listens, transcribes,
        and types your words wherever you need them. Everything runs locally on your Mac.
      </p>
      <div className="space-y-2">
        <div className="flex items-center gap-3 text-sm text-surface-300">
          <CheckIcon />
          <span>Privacy-first: your audio never leaves your Mac</span>
        </div>
        <div className="flex items-center gap-3 text-sm text-surface-300">
          <CheckIcon />
          <span>Transcribes in any app with global hotkey control</span>
        </div>
        <div className="flex items-center gap-3 text-sm text-surface-300">
          <CheckIcon />
          <span>Optional "My Voice" speaker verification</span>
        </div>
      </div>
    </div>
  );
}

function StepEnrollNormal() {
  return (
    <div className="animate-slide-up">
      <h2 className="text-xl font-semibold text-white mb-3">My Voice — Step 1 of 3</h2>
      <p className="text-surface-300 text-sm leading-relaxed mb-4">
        "Record: normal"
      </p>
      <p className="text-surface-400 text-xs leading-relaxed mb-4">
        Speak this sentence at a normal volume:
      </p>
      <div className="bg-surface-800 rounded-lg p-4 text-center">
        <p className="text-white text-base font-medium">
          The quick brown fox jumps over the lazy dog
        </p>
      </div>
    </div>
  );
}

function StepEnrollLower() {
  return (
    <div className="animate-slide-up">
      <h2 className="text-xl font-semibold text-white mb-3">My Voice — Step 2 of 3</h2>
      <p className="text-surface-300 text-sm leading-relaxed mb-4">
        "Record: lower"
      </p>
      <p className="text-surface-400 text-xs leading-relaxed mb-4">
        Now speak the same sentence at a slightly lower volume:
      </p>
      <div className="bg-surface-800 rounded-lg p-4 text-center">
        <p className="text-white text-base font-medium">
          The quick brown fox jumps over the lazy dog
        </p>
      </div>
    </div>
  );
}

function StepEnrollLouder() {
  return (
    <div className="animate-slide-up">
      <h2 className="text-xl font-semibold text-white mb-3">My Voice — Step 3 of 3</h2>
      <p className="text-surface-300 text-sm leading-relaxed mb-4">
        "Record: louder"
      </p>
      <p className="text-surface-400 text-xs leading-relaxed mb-4">
        Finally, speak the same sentence at a slightly higher volume:
      </p>
      <div className="bg-surface-800 rounded-lg p-4 text-center">
        <p className="text-white text-base font-medium">
          The quick brown fox jumps over the lazy dog
        </p>
      </div>
    </div>
  );
}

function StepDone() {
  return (
    <div className="animate-slide-up">
      <h2 className="text-xl font-semibold text-white mb-3">You're all set!</h2>
      <p className="text-surface-300 text-sm leading-relaxed mb-4">
        Always is ready to transcribe your speech. Use the hotkeys in the Settings
        to control listening, pausing, and grammar correction.
      </p>
      <div className="bg-surface-800 rounded-lg p-4 space-y-2 text-sm">
        <div className="flex justify-between text-surface-300">
          <span>Pause listening</span>
          <kbd className="text-surface-100 bg-surface-700 px-2 py-0.5 rounded text-xs">Alt+Space</kbd>
        </div>
        <div className="flex justify-between text-surface-300">
          <span>Resume listening</span>
          <kbd className="text-surface-100 bg-surface-700 px-2 py-0.5 rounded text-xs">Alt+Shift+Space</kbd>
        </div>
        <div className="flex justify-between text-surface-300">
          <span>Capture correction</span>
          <kbd className="text-surface-100 bg-surface-700 px-2 py-0.5 rounded text-xs">Ctrl+Alt+X</kbd>
        </div>
      </div>
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