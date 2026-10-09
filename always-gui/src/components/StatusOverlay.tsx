import { DaemonState } from "../types/uds";

interface StatusOverlayProps {
  state: DaemonState;
  onTogglePause: () => void;
  onOpenSettings: () => void;
  compact?: boolean;
}

export function StatusOverlay({
  state,
  onTogglePause,
  onOpenSettings,
  compact = false,
}: StatusOverlayProps) {
  const {
    state: daemonState,
    isPaused,
    masterPaused,
    isAutoEnter,
    currentTranscript,
    interimText,
    backend,
    focusedApp,
    enrolled,
    voiceEnabled,
    pauseSource,
  } = state;

  // Determine if we show the full overlay
  const showOverlay =
    daemonState === "Listening" ||
    daemonState === "Processing" ||
    daemonState === "Transcribing" ||
    daemonState === "Paused";

  if (!showOverlay && !compact) return null;

  const stateColor = isPaused
    ? "text-surface-400"
    : daemonState === "Processing"
      ? "text-yellow-400"
      : daemonState === "Transcribing"
        ? "text-primary-400"
        : "text-green-400";

  const stateLabel = isPaused
    ? "Paused"
    : daemonState === "Processing"
      ? "Processing…"
      : daemonState === "Transcribing"
        ? "Transcribing…"
        : "Listening";

  return (
    <div className="flex flex-col gap-2">
      {/* HUD Bar */}
      <div
        className={`bg-surface-900/95 backdrop-blur-sm rounded-xl border border-surface-800/50 shadow-xl overflow-hidden ${
          compact ? "p-2" : "p-3"
        }`}
      >
        <div className="flex items-center gap-3">
          {/* Mic icon */}
          <button
            onClick={onTogglePause}
            className={`flex items-center gap-2 text-sm font-medium ${stateColor} hover:opacity-80 transition-opacity`}
          >
            <MicIcon active={daemonState !== "Paused" && daemonState !== "Idle"} />
            <span>{stateLabel}</span>
          </button>

          {/* Energy bar (visual only during listening) */}
          {!isPaused && daemonState !== "Idle" && (
            <div className="flex-1 h-1.5 bg-surface-800 rounded-full overflow-hidden">
              <div
                className={`h-full rounded-full transition-all duration-150 ${
                  daemonState === "Processing"
                    ? "bg-yellow-500"
                    : daemonState === "Transcribing"
                      ? "bg-primary-500"
                      : "bg-green-500"
                }`}
                style={{ width: daemonState === "Processing" ? "60%" : "40%" }}
              />
            </div>
          )}

          {/* Badges */}
          <div className="flex items-center gap-1.5">
            {pauseSource && (
              <span className="text-[10px] px-1.5 py-0.5 bg-red-500/20 text-red-400 rounded-full">
                {pauseSource}
              </span>
            )}
            {voiceEnabled && enrolled && (
              <span className="text-[10px] px-1.5 py-0.5 bg-primary-500/20 text-primary-300 rounded-full flex items-center gap-1">
                <VoiceBadge />
                My Voice
              </span>
            )}
            {isAutoEnter && (
              <span className="text-[10px] px-1.5 py-0.5 bg-green-500/20 text-green-400 rounded-full">
                Auto
              </span>
            )}
            <span className="text-[10px] px-1.5 py-0.5 bg-surface-800 text-surface-400 rounded-full">
              {backend}
            </span>
          </div>

          {/* Settings */}
          <button
            onClick={onOpenSettings}
            className="p-1.5 text-surface-500 hover:text-white transition-colors"
            title="Settings"
          >
            <SettingsIcon />
          </button>
        </div>
      </div>

      {/* Transcript display (only when not compact) */}
      {!compact && (currentTranscript || interimText) && (
        <div className="bg-surface-900/90 backdrop-blur-sm rounded-xl border border-surface-800/50 p-3 animate-slide-up">
          {interimText && (
            <p className="text-surface-400 text-sm italic">{interimText}</p>
          )}
          {currentTranscript && (
            <p className="text-white text-sm font-medium">{currentTranscript}</p>
          )}
        </div>
      )}
    </div>
  );
}

function MicIcon({ active }: { active: boolean }) {
  if (active) {
    return (
      <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19 11a7 7 0 01-7 7m0 0a7 7 0 01-7-7m7 7v4m0 0H8m4 0h4m-4-8a3 3 0 01-3-3V5a3 3 0 116 0v6a3 3 0 01-3 3z" />
      </svg>
    );
  }
  return (
    <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M5.586 15H4a1 1 0 01-1-1v-4a1 1 0 011-1h1.586l4.707-4.707C10.923 3.663 12 4.109 12 5v14c0 .891-1.077 1.337-1.707.707L5.586 15z" />
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M17 14l2-2m0 0l2-2m-2 2l-2-2m2 2l2 2" />
    </svg>
  );
}

function VoiceBadge() {
  return (
    <svg className="w-2.5 h-2.5" fill="currentColor" viewBox="0 0 24 24">
      <circle cx="12" cy="12" r="6" />
    </svg>
  );
}

function SettingsIcon() {
  return (
    <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 002.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 001.066 2.573c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 00-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 00-2.573 1.066c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 00-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 00-1.066-2.573c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 001.066-2.573c-.94-1.543.826-3.31 2.37-2.37.996.608 2.296.07 2.572-1.065z" />
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M15 12a3 3 0 11-6 0 3 3 0 016 0z" />
    </svg>
  );
}