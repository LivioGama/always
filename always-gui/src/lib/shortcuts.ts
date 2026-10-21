import type { Config } from "./config";

// Shortcut parsing/formatting — mirror of KeyCaptureButton.swift helpers.
// Wire format: "ctrl+alt+shift+p" (lowercase, "+"-separated).

export function parseShortcutParts(s: string): string[] {
  if (!s) return [];
  return s.toLowerCase().split("+").map((p) => p.trim()).filter(Boolean);
}

const SYMBOLS: Record<string, string> = {
  ctrl: "Ctrl",
  control: "Ctrl",
  alt: "Alt",
  option: "Alt",
  shift: "Shift",
  meta: "Win",
  cmd: "Win",
  command: "Win",
  fn: "Fn",
  space: "Space",
  return: "Enter",
  enter: "Enter",
};

export function partSymbol(part: string): string {
  return SYMBOLS[part] ?? part.toUpperCase();
}

/// Map a DOM KeyboardEvent to the daemon's wire format, or null when the
/// press is modifier-only or has no modifiers (same rule as the macOS
/// recorder: a bare key can't be a global shortcut).
export function shortcutFromKeyEvent(e: KeyboardEvent): string | null {
  const parts: string[] = [];
  if (e.ctrlKey) parts.push("ctrl");
  if (e.altKey) parts.push("alt");
  if (e.shiftKey) parts.push("shift");
  if (e.metaKey) parts.push("meta");

  let key = e.key.toLowerCase();
  if (key === " ") key = "space";
  if (key === "arrowup" || key === "arrowdown" || key === "arrowleft" || key === "arrowright") {
    key = key.replace("arrow", "");
  }
  if (["control", "alt", "shift", "meta", "capslock", "tab", "escape"].includes(key)) return null;
  if (parts.length === 0) return null;
  return [...parts, key].join("+");
}

/// The global shortcut actions, shared by onboarding's tips and Settings →
/// Shortcuts so both show what is actually configured.
export type ShortcutAction = {
  key: keyof Config;
  label: string;
  detail: string;
  /// CLI equivalent a desktop can bind where Always cannot listen for keys
  /// (Linux on Wayland). No command = not available there.
  command?: string;
  /// Per-app pause needs a focused-app monitor, which only macOS has.
  perApp?: boolean;
};

export const SHORTCUT_ACTIONS: ShortcutAction[] = [
  {
    key: "shortcutMasterPause",
    label: "Pause everywhere",
    detail: "Turn Always off or back on, in every app.",
    command: "always toggle-pause",
  },
  {
    key: "shortcutPause",
    label: "Pause or resume for this app",
    detail: "Pause dictation in the focused app without changing the rest of your allowlist.",
    perApp: true,
  },
  {
    key: "shortcutAutoEnter",
    label: "Toggle auto-enter",
    detail: "When enabled, Always presses Enter after typing your words.",
    command: "always toggle-auto-enter",
  },
  {
    key: "shortcutForcePaste",
    label: "Paste filtered text anyway",
    detail: "Use this when you want the raw transcript instead of filtered text.",
  },
  {
    key: "shortcutCorrectionDialog",
    label: "Correct & improve your vocabulary",
    detail: "Fix the last transcript; Always can learn the correction for next time.",
  },
];

/// Configured combo for display ("ctrl+alt+shift+p" → "Ctrl+Alt+Shift+P"); unset → null.
export function formatShortcut(combo: string | undefined): string | null {
  const parts = parseShortcutParts(combo === "(none)" ? "" : (combo ?? ""));
  return parts.length ? parts.map(partSymbol).join("+") : null;
}

/// Linux on Wayland: the daemon reports it cannot listen for global keys
/// (`ShortcutListenerStatus` false), so the desktop has to bind them.
export function shortcutsNeedDesktop(os: string, listenerOk: boolean): boolean {
  return os === "linux" && !listenerOk;
}

/// The actions to show for this platform, with their configured keys.
export function visibleShortcuts(config: Config, os: string, needDesktop: boolean) {
  return SHORTCUT_ACTIONS.filter((a) => !(a.perApp && os === "linux"))
    .filter((a) => !needDesktop || a.command)
    .map((a) => ({ ...a, combo: formatShortcut(config[a.key] as string) }))
    .filter((a) => a.combo !== null);
}
