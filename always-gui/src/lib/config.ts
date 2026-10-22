// Mirror of the Swift app's `Config` model + `Config.fromCLI` parser —
// `always config show` emits `key: value` lines the GUI binds to.

export interface Config {
  sttEnergyThreshold: number;
  hearEnergyThreshold: number;
  sttCooldownMs: number;
  sttSilence: number;
  sttAdaptiveSilence: boolean;
  sttLivePreview: boolean;
  liveTyping: boolean;
  spokenEnter: boolean;
  spokenEnterPhrases: string;
  spokenEnterAtEnd: boolean;
  sttAutoEnter: boolean;
  autoEnterDelayMs: number;
  groqApiKey: string | null;
  groqKeySaved: boolean;
  sileroThreshold: number;
  shortcutPause: string;
  shortcutAutoEnter: string;
  shortcutForcePaste: string;
  shortcutCorrectionDialog: string;
  shortcutMasterPause: string;
  postprocessEnabled: boolean;
  postprocessProvider: string; // "groq" | "apple"
  postprocessMode: string | null; // "fast" | "apple" | "groq" | "off"
  appleIntelligenceAvailable: boolean;
  perAppSettingsJson: string | null;
  idlePauseSecs: number;
  audibleStatusSound: string; // off | low | medium | high
  lang: string | null;
  micConflictExclusionBundles: string;
}

export const DEFAULT_CONFIG: Config = {
  sttEnergyThreshold: 0.012,
  hearEnergyThreshold: 0.001,
  sttCooldownMs: 150,
  sttSilence: 0.9,
  sttAdaptiveSilence: true,
  sttLivePreview: true,
  liveTyping: false,
  spokenEnter: false,
  spokenEnterPhrases: "press enter, hit enter, press return, hit return, send it, send that, submit it, submit that",
  spokenEnterAtEnd: false,
  sttAutoEnter: true,
  autoEnterDelayMs: 4000,
  groqApiKey: null,
  groqKeySaved: false,
  sileroThreshold: 0.5,
  shortcutPause: "ctrl+alt+p",
  shortcutAutoEnter: "ctrl+alt+a",
  shortcutForcePaste: "ctrl+alt+v",
  shortcutCorrectionDialog: "ctrl+alt+w",
  shortcutMasterPause: "ctrl+alt+shift+p",
  postprocessEnabled: true,
  postprocessProvider: "groq",
  postprocessMode: null,
  appleIntelligenceAvailable: true,
  perAppSettingsJson: null,
  idlePauseSecs: 600,
  audibleStatusSound: "off",
  lang: null,
  micConflictExclusionBundles: '["net.telestream.screenflow10","pl.maketheweb.cleanshotx"]',
};

/// Mirrors `resolvedPostprocessMode`: explicit mode wins; enabled → its
/// provider; off → "fast".
export function resolvedPostprocessMode(c: Config): string {
  if (c.postprocessMode) return c.postprocessMode;
  if (!c.postprocessEnabled) return "fast";
  return c.postprocessProvider === "apple" ? "apple" : "groq";
}

function isMaskedApiKey(value: string): boolean {
  return value.includes("***") || value.includes("(saved)");
}

export function parseConfig(output: string): Config {
  const c = { ...DEFAULT_CONFIG };
  for (const line of output.split("\n")) {
    const idx = line.indexOf(":");
    if (idx < 0) continue;
    const key = line.slice(0, idx).trim();
    const value = line.slice(idx + 1).trim();
    const num = (d: number) => {
      const n = parseFloat(value.replace("s", ""));
      return Number.isFinite(n) ? n : d;
    };
    const int = (d: number) => {
      const n = parseInt(value, 10);
      return Number.isFinite(n) ? n : d;
    };
    const bool = () => value === "true" || value === "1";
    const notSet = value.includes("(not set)");

    switch (key) {
      case "stt_energy_threshold": c.sttEnergyThreshold = num(c.sttEnergyThreshold); break;
      case "hear_energy_threshold": c.hearEnergyThreshold = num(c.hearEnergyThreshold); break;
      case "stt_cooldown_ms": c.sttCooldownMs = int(c.sttCooldownMs); break;
      case "stt_cooldown_secs": {
        const s = parseFloat(value);
        if (Number.isFinite(s)) c.sttCooldownMs = Math.round(s * 1000);
        break;
      }
      case "stt_silence":
      case "stt_silence_secs": c.sttSilence = num(c.sttSilence); break;
      case "stt_adaptive_silence": c.sttAdaptiveSilence = bool(); break;
      case "stt_live_preview": c.sttLivePreview = bool(); break;
      case "live_typing": c.liveTyping = bool(); break;
      case "spoken_enter": c.spokenEnter = bool(); break;
      case "spoken_enter_phrases": c.spokenEnterPhrases = value; break;
      case "spoken_enter_at_end": c.spokenEnterAtEnd = bool(); break;
      case "stt_auto_enter": c.sttAutoEnter = bool(); break;
      case "auto_enter_delay_ms": c.autoEnterDelayMs = int(c.autoEnterDelayMs); break;
      case "auto_enter_delay_secs":
      case "stt_auto_enter_delay_secs": {
        const s = parseFloat(value);
        if (Number.isFinite(s)) c.autoEnterDelayMs = Math.round(s * 1000);
        break;
      }
      case "groq_api_key":
        if (!notSet) {
          if (isMaskedApiKey(value)) c.groqKeySaved = true;
          else { c.groqApiKey = value; c.groqKeySaved = true; }
        }
        break;
      case "silero_threshold": c.sileroThreshold = num(c.sileroThreshold); break;
      case "shortcut_pause": if (!notSet) c.shortcutPause = value; break;
      case "shortcut_auto_enter": if (!notSet) c.shortcutAutoEnter = value; break;
      case "shortcut_force_paste": if (!notSet) c.shortcutForcePaste = value; break;
      case "shortcut_correction_dialog": if (!notSet) c.shortcutCorrectionDialog = value; break;
      case "shortcut_master_pause": if (!notSet) c.shortcutMasterPause = value; break;
      case "postprocess_enabled": c.postprocessEnabled = bool(); break;
      case "postprocess_provider": if (["groq", "apple"].includes(value)) c.postprocessProvider = value; break;
      case "postprocess_mode": if (["fast", "apple", "groq", "off"].includes(value)) c.postprocessMode = value; break;
      case "apple_intelligence_available": c.appleIntelligenceAvailable = bool(); break;
      case "per_app_settings_json": c.perAppSettingsJson = value === "{}" ? null : value; break;
      case "idle_pause_secs": c.idlePauseSecs = int(c.idlePauseSecs); break;
      case "audible_status_sound": if (["off", "low", "medium", "high"].includes(value)) c.audibleStatusSound = value; break;
      case "lang": c.lang = value === "" || notSet ? null : value; break;
      case "mic_conflict_exclusion_bundles": c.micConflictExclusionBundles = value; break;
    }
  }
  return c;
}
