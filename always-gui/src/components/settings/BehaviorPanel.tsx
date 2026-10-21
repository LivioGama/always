import { useState } from "react";
import type { DaemonApi } from "../../hooks/useDaemonState";
import type { Config } from "../../lib/config";
import { Caption, Divider, NumericSettingRow, PanelTitle, Segmented, ToggleRow } from "../controls";

// Presets — mirror SensitivityPreset.swift / PauseTolerancePreset.swift.
const SENSITIVITY_PRESETS = [
  { key: "high", label: "High", stt: 0.005, hear: 0.0005 },
  { key: "normal", label: "Normal", stt: 0.012, hear: 0.001 },
  { key: "low", label: "Low", stt: 0.025, hear: 0.002 },
];

const PAUSE_TOLERANCE_PRESETS = [
  { key: "fast", label: "Fast", secs: 0.6 },
  { key: "balanced", label: "Balanced", secs: 0.9 },
  { key: "relaxed", label: "Relaxed", secs: 1.4 },
];

function sensitivityOf(c: Config) {
  return SENSITIVITY_PRESETS.find(
    (p) => Math.abs(p.stt - c.sttEnergyThreshold) < 1e-6 && Math.abs(p.hear - c.hearEnergyThreshold) < 1e-6,
  )?.key ?? "custom";
}

function toleranceOf(c: Config) {
  return PAUSE_TOLERANCE_PRESETS.find((p) => Math.abs(p.secs - c.sttSilence) < 1e-6)?.key ?? "custom";
}

/// Settings → Behavior: sensitivity presets, pause tolerance, toggles,
/// advanced thresholds, auto-enter, idle auto-pause. Mirrors BehaviorPanel.swift.
export function BehaviorPanel({
  daemon,
  config,
  onConfigChange,
}: {
  daemon: DaemonApi;
  config: Config;
  onConfigChange: (patch: Partial<Config>) => void;
}) {
  const [showAdvanced, setShowAdvanced] = useState(false);
  const sens = sensitivityOf(config);
  const tol = toleranceOf(config);
  const s = daemon.state;

  const closestSensHint = () => {
    let best = SENSITIVITY_PRESETS[1];
    let bestDist = Infinity;
    for (const p of SENSITIVITY_PRESETS) {
      const dist = Math.abs(p.stt - config.sttEnergyThreshold) + Math.abs(p.hear - config.hearEnergyThreshold);
      if (dist < bestDist) { bestDist = dist; best = p; }
    }
    return `Custom — closest to ${best.label} (STT ${best.stt.toFixed(4)}, Hear ${best.hear.toFixed(4)}). Yours: STT ${config.sttEnergyThreshold.toFixed(4)}, Hear ${config.hearEnergyThreshold.toFixed(4)}.`;
  };

  const closestTolHint = () => {
    let best = PAUSE_TOLERANCE_PRESETS[1];
    let bestDist = Infinity;
    for (const p of PAUSE_TOLERANCE_PRESETS) {
      const dist = Math.abs(p.secs - config.sttSilence);
      if (dist < bestDist) { bestDist = dist; best = p; }
    }
    return `Custom — closest to ${best.label} (${best.secs.toFixed(1)}s). Yours: ${config.sttSilence.toFixed(2)}s.`;
  };

  const changeRuntimeSetting = (patch: Partial<Config>) => {
    onConfigChange(patch);
    const next = { ...config, ...patch };
    daemon.applyRuntimePreferences({
      auto_enter_delay_ms: next.autoEnterDelayMs,
      energy_threshold: next.sttEnergyThreshold,
      silence_secs: next.sttSilence,
      cooldown_ms: next.sttCooldownMs,
      silero_threshold: next.sileroThreshold,
      adaptive_silence: next.sttAdaptiveSilence,
      stt_live_preview: next.sttLivePreview,
      live_typing: next.liveTyping,
      spoken_enter: next.spokenEnter,
      spoken_enter_phrases: next.spokenEnterPhrases,
      spoken_enter_at_end: next.spokenEnterAtEnd,
    });
  };

  return (
    <div className="p-5 space-y-3.5">
      {/* Sensitivity */}
      <div className="space-y-2">
        <PanelTitle>Sensitivity</PanelTitle>
        <div className="flex items-center gap-3">
          <span className="text-[13px] text-surface-200">Mic Sensitivity</span>
          <div className="flex-1" />
          <Segmented
            width={280}
            value={sens}
            onChange={(key) => {
              const p = SENSITIVITY_PRESETS.find((x) => x.key === key);
              if (p) onConfigChange({ sttEnergyThreshold: p.stt, hearEnergyThreshold: p.hear });
            }}
            options={[
              ...SENSITIVITY_PRESETS.map((p) => ({ label: p.label, value: p.key })),
              ...(sens === "custom" ? [{ label: "Custom", value: "custom" }] : []),
            ]}
          />
        </div>
        {sens === "custom" && <Caption className="text-orange-400/90">✦ {closestSensHint()}</Caption>}

        <div className="flex items-center gap-3">
          <span
            className="text-[13px] text-surface-200"
            title="How long a mid-sentence pause can be before the utterance is finalized and pasted. Higher = fewer split sentences, slightly later paste."
          >
            Pause Tolerance
          </span>
          <div className="flex-1" />
          <Segmented
            width={280}
            value={tol}
            onChange={(key) => {
              const p = PAUSE_TOLERANCE_PRESETS.find((x) => x.key === key);
              if (p) onConfigChange({ sttSilence: p.secs });
            }}
            options={[
              ...PAUSE_TOLERANCE_PRESETS.map((p) => ({ label: p.label, value: p.key })),
              ...(tol === "custom" ? [{ label: "Custom", value: "custom" }] : []),
            ]}
          />
        </div>
        {tol === "custom" && <Caption className="text-orange-400/90">✦ {closestTolHint()}</Caption>}

        <ToggleRow
          label="Extend automatically when a sentence sounds unfinished"
          help="When the transcript-so-far ends mid-thought (no punctuation, trailing 'and'/'which'/…), the pause window is temporarily doubled so the sentence isn't split."
          on={config.sttAdaptiveSilence}
          onChange={(v) => changeRuntimeSetting({ sttAdaptiveSilence: v })}
        />
        <ToggleRow
          label="Show live transcript while you speak"
          help="Periodically transcribes what you've said so far and shows the provisional text in the overlay while you're still talking."
          on={config.sttLivePreview}
          onChange={(v) => changeRuntimeSetting({ sttLivePreview: v })}
        />
        <ToggleRow
          label="Type words while you speak"
          help="Types into the focused app as you talk instead of pasting when you stop, then corrects the words to the final transcript. Typing stops if you pause, switch window, or another app takes the microphone."
          on={config.liveTyping}
          onChange={(v) => changeRuntimeSetting({ liveTyping: v })}
        />

        {/* Advanced thresholds disclosure */}
        <button
          type="button"
          onClick={() => setShowAdvanced((v) => !v)}
          className="flex items-center gap-1.5 text-xs text-surface-400 hover:text-surface-200"
        >
          <svg
            className={`w-3 h-3 transition-transform ${showAdvanced ? "rotate-90" : ""}`}
            viewBox="0 0 24 24" fill="currentColor"
          >
            <path d="M8 5v14l11-7z" />
          </svg>
          Advanced thresholds
        </button>
        {showAdvanced && (
          <div className="space-y-2 pt-1.5">
            <NumericSettingRow
              title="STT Energy Threshold"
              help="How loud your voice needs to be for Always to start transcribing."
              explanation="How loud your voice needs to be for Always to start transcribing. Think of it as the 'volume gate' — below this level, Always stays silent. Lower if you speak softly, raise it if it's transcribing background noise."
              value={config.sttEnergyThreshold}
              onChange={(v) => onConfigChange({ sttEnergyThreshold: v })}
              defaultValue={0.012}
              min={0.0001}
              max={0.5}
              decimals={4}
            />
            <NumericSettingRow
              title="Hear Energy Threshold"
              help="How quiet a sound can be for Always to 'hear' that someone is speaking."
              explanation="A pre-check that decides 'is anyone speaking at all?' before running the full transcriber. Lower = catches quiet or distant voices. Higher = ignores background chatter and only reacts to clear, nearby speech."
              value={config.hearEnergyThreshold}
              onChange={(v) => onConfigChange({ hearEnergyThreshold: v })}
              defaultValue={0.001}
              min={0.0001}
              max={0.5}
              decimals={4}
            />
            <NumericSettingRow
              title="Silence Timeout"
              help="How many seconds of silence before Always finalizes what you said and pastes it."
              explanation="How many seconds of silence before Always finalizes what you said and pastes it. Short = snappy but may cut you off mid-sentence. Long = patient, keeps sentences together, but slower to paste."
              unit="s"
              value={config.sttSilence}
              onChange={(v) => onConfigChange({ sttSilence: v })}
              defaultValue={0.9}
              min={0.3}
              max={15}
              decimals={2}
            />
            <NumericSettingRow
              title="Cooldown"
              help="Minimum seconds between consecutive pastes."
              explanation="A safety pause between consecutive pastes so two sentences don't get mashed together. Usually leave at default. Lower = more responsive, higher = more deliberate."
              unit="s"
              value={config.sttCooldownMs / 1000}
              onChange={(v) => onConfigChange({ sttCooldownMs: Math.round(v * 1000) })}
              defaultValue={0.15}
              min={0}
              max={60}
              decimals={3}
            />
            <NumericSettingRow
              title="Silero VAD Threshold"
              help="How confident the voice activity detector must be that it hears actual speech."
              explanation="How confident the AI voice detector must be that it hears actual speech (not a door slam, keyboard, or fan). 0 = always on, 1 = only crystal-clear speech. 0.5 is a good middle ground."
              value={config.sileroThreshold}
              onChange={(v) => onConfigChange({ sileroThreshold: v })}
              defaultValue={0.5}
              min={0}
              max={1}
              decimals={2}
            />
          </div>
        )}
      </div>

      <Divider />

      {/* Auto-Enter */}
      <div className="space-y-1.5">
        <PanelTitle>Auto-Enter</PanelTitle>
        <ToggleRow
          label="Press Return after each paste"
          on={s.isAutoEnter}
          onChange={(v) => {
            onConfigChange({ sttAutoEnter: v });
            daemon.setAutoEnter(v);
          }}
          disabled={!s.isDaemonConnected}
        />
        <NumericSettingRow
          title="Delay"
          help="Seconds before Return is pressed. Set to 0 for immediate Return."
          unit="s"
          value={config.autoEnterDelayMs / 1000}
          onChange={(v) => onConfigChange({ autoEnterDelayMs: Math.round(v * 1000) })}
          defaultValue={4}
          min={0}
          max={60}
          decimals={0}
        />
        <ToggleRow
          label={'Say "press enter" to send'}
          help={'Saying "press enter" or "send it" on its own, right after dictating, presses Return. Off: those words are typed like any others.'}
          on={config.spokenEnter}
          onChange={(v) => changeRuntimeSetting({ spokenEnter: v })}
        />
        {config.spokenEnter && (
          <div className="space-y-1">
            <input
              value={config.spokenEnterPhrases}
              onChange={(e) => onConfigChange({ spokenEnterPhrases: e.target.value })}
              onBlur={() => changeRuntimeSetting({})}
              placeholder="press enter, send it"
              aria-label="Phrases that press Return"
              className="w-full bg-surface-800 border border-surface-700 rounded px-2.5 py-1.5 text-xs text-white focus:outline-none focus:ring-1 focus:ring-primary-500"
            />
            <Caption>Phrases that press Return, separated by commas. Leave empty for the defaults.</Caption>
            <ToggleRow
              label="Also at the end of a sentence"
              help={'"…and run the tests, go go" types the sentence without the phrase, then presses Return — no pause needed. Pick a phrase you would never dictate: with "send it", a sentence ending in "send it" is sent.'}
              on={config.spokenEnterAtEnd}
              onChange={(v) => changeRuntimeSetting({ spokenEnterAtEnd: v })}
            />
          </div>
        )}
      </div>

      <Divider />

      {/* Idle Auto-Pause */}
      <div className="space-y-1.5">
        <PanelTitle>Idle Auto-Pause</PanelTitle>
        <Caption>
          Pauses listening after a stretch with no speech. Does not flip the global master switch — switch to a resumed app or speak to wake. Changes apply on next daemon restart.
        </Caption>
        <NumericSettingRow
          title="Pause After Inactivity"
          help="Seconds of no voice before idle auto-pause. Default 600 (10 min). Set to 0 to disable."
          unit="s"
          value={config.idlePauseSecs}
          onChange={(v) => onConfigChange({ idlePauseSecs: Math.round(v) })}
          defaultValue={600}
          min={0}
          max={86400}
          decimals={0}
        />
      </div>
    </div>
  );
}

