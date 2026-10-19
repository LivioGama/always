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
