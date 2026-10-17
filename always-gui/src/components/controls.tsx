import { useEffect, useRef, useState, type ReactNode } from "react";
import { parseShortcutParts, partSymbol, shortcutFromKeyEvent } from "../lib/shortcuts";

// ── Typography / layout primitives (SwiftUI-ish) ────────────────────────────

export function PanelTitle({ children }: { children: ReactNode }) {
  return <h3 className="text-sm font-semibold text-white">{children}</h3>;
}

export function Caption({ children, className = "" }: { children: ReactNode; className?: string }) {
  return <p className={`text-xs text-surface-400 ${className}`}>{children}</p>;
}

export function Divider() {
  return <div className="border-t border-surface-800 my-1" />;
}

/// SwiftUI `Color.secondary.opacity(0.08)` card.
export function Card({ children, accent = false, className = "" }: {
  children: ReactNode;
  accent?: boolean;
  className?: string;
}) {
  return (
    <div
      className={`rounded-md p-2.5 ${
        accent ? "bg-primary-500/10" : "bg-surface-500/[0.08]"
      } ${className}`}
    >
      {children}
    </div>
  );
}

// ── Toggle (switch style) ───────────────────────────────────────────────────

export function Toggle({ on, onChange, disabled = false }: {
  on: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      disabled={disabled}
      onClick={() => onChange(!on)}
      className={`relative w-10 h-[22px] rounded-full transition-colors shrink-0 ${
        on ? "bg-primary-500" : "bg-surface-600"
      } ${disabled ? "opacity-40 cursor-not-allowed" : ""}`}
    >
      <span
        className={`absolute top-[2px] w-[18px] h-[18px] bg-white rounded-full shadow transition-transform ${
          on ? "translate-x-[20px]" : "translate-x-[2px]"
        }`}
      />
    </button>
  );
}

/// Label + caption + toggle row, like a SwiftUI `Toggle("…", isOn:)`.
export function ToggleRow({ label, help, on, onChange, disabled }: {
  label: string;
  help?: string;
  on: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <div className="flex items-start gap-3" title={help}>
      <div className="flex-1">
        <div className="text-xs text-surface-200">{label}</div>
      </div>
      <Toggle on={on} onChange={onChange} disabled={disabled} />
    </div>
  );
}

// ── Segmented picker (Picker .segmented) ────────────────────────────────────

export function Segmented<T extends string>({
  options,
  value,
  onChange,
  width,
}: {
  options: { label: string; value: T }[];
  value: T;
  onChange: (v: T) => void;
  width?: number;
}) {
  return (
    <div
      className="inline-flex rounded-lg bg-surface-800 p-0.5 border border-surface-700"
      style={width ? { width } : undefined}
    >
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          onClick={() => onChange(o.value)}
          className={`flex-1 px-3 py-1 text-xs rounded-md transition-colors whitespace-nowrap ${
            value === o.value
              ? "bg-surface-600 text-white shadow"
              : "text-surface-400 hover:text-surface-200"
          }`}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

// ── Buttons ─────────────────────────────────────────────────────────────────

export function Btn({ children, onClick, disabled, destructive, prominent, small = true, title }: {
  children: ReactNode;
  onClick?: () => void;
  disabled?: boolean;
  destructive?: boolean;
  prominent?: boolean;
  small?: boolean;
  title?: string;
}) {
  const base = `inline-flex items-center gap-1 rounded-md font-medium transition-colors ${
    small ? "px-2.5 py-1 text-xs" : "px-3 py-1.5 text-sm"
  }`;
  const tone = destructive
    ? "bg-red-500/10 text-red-400 hover:bg-red-500/20"
    : prominent
      ? "bg-primary-600 text-white hover:bg-primary-500"
      : "bg-surface-800 text-surface-300 hover:bg-surface-700 hover:text-white border border-surface-700";
  return (
    <button
      type="button"
      title={title}
      onClick={onClick}
      disabled={disabled}
      className={`${base} ${tone} ${disabled ? "opacity-40 cursor-not-allowed" : ""}`}
    >
      {children}
    </button>
  );
}

// ── NumericSettingRow ───────────────────────────────────────────────────────

export function NumericSettingRow({
  title,
  help,
  explanation,
  unit = "",
  value,
  onChange,
  defaultValue,
  min,
  max,
  decimals = 4,
  disabled,
}: {
  title: string;
  help?: string;
  explanation?: string;
  unit?: string;
  value: number;
  onChange: (v: number) => void;
  defaultValue: number;
  min?: number;
  max?: number;
  decimals?: number;
  disabled?: boolean;
}) {
  const [showInfo, setShowInfo] = useState(false);
  const [text, setText] = useState(String(value));
  useEffect(() => setText(String(value)), [value]);

  const commit = (raw: string) => {
    let n = parseFloat(raw);
    if (!Number.isFinite(n)) return;
    if (min !== undefined) n = Math.max(min, n);
    if (max !== undefined) n = Math.min(max, n);
    onChange(n);
  };

  return (
    <div className="relative">
      <div className="flex items-baseline gap-3">
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-1">
            <span className="text-sm text-surface-200" title={help}>{title}</span>
            {explanation && (
              <button
                type="button"
                onClick={() => setShowInfo((s) => !s)}
                className="text-primary-400 hover:text-primary-300"
                title="What does this mean?"
              >
                <svg className="w-3 h-3" viewBox="0 0 24 24" fill="none" stroke="currentColor">
                  <circle cx="12" cy="12" r="10" strokeWidth={2} />
                  <path strokeWidth={2} d="M12 16v-4m0-4h.01" />
                </svg>
              </button>
            )}
          </div>
          {help && <div className="text-[10px] text-surface-500 leading-snug">{help}</div>}
        </div>
        <div className="flex items-center gap-1 shrink-0">
          <input
            value={text}
            disabled={disabled}
            onChange={(e) => {
              setText(e.target.value);
              commit(e.target.value);
            }}
            onBlur={() => setText(String(value))}
            className="w-20 bg-surface-800 border border-surface-700 rounded px-2 py-1 text-xs text-right text-white focus:outline-none focus:ring-1 focus:ring-primary-500 disabled:opacity-40"
          />
          {unit && <span className="text-xs text-surface-500 w-5">{unit}</span>}
        </div>
        <span className="text-[10px] text-surface-500 w-24 text-right shrink-0">
          Default: {defaultValue.toFixed(decimals)}{unit ? ` ${unit}` : ""}
        </span>
        <button
          type="button"
          title="Reset to recommended default"
          onClick={() => onChange(defaultValue)}
          className="text-surface-500 hover:text-surface-200 shrink-0"
        >
          <svg className="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path strokeWidth={2} strokeLinecap="round" d="M3 12a9 9 0 109-9M3 3v6h6" />
          </svg>
        </button>
      </div>
      {showInfo && explanation && (
        <div className="mt-1 ml-4 p-3 w-72 bg-surface-800 border border-surface-700 rounded-lg shadow-xl z-10 relative">
          <div className="text-sm font-medium text-white mb-1">{title}</div>
          <div className="text-xs text-surface-400">{explanation}</div>
        </div>
      )}
    </div>
  );
}

// ── KeyCaptureButton ────────────────────────────────────────────────────────

export function KeyCaptureButton({
  label,
  description,
  shortcut,
  onSave,
}: {
  label: string;
  description?: string;
  shortcut: string;
  onSave: (value: string) => void;
}) {
  const [recording, setRecording] = useState(false);
  const recordingRef = useRef(false);
  recordingRef.current = recording;
  const saveRef = useRef(onSave);
  saveRef.current = onSave;

  useEffect(() => {
    if (!recording) return;
    const handler = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      const combo = shortcutFromKeyEvent(e);
      if (combo) {
        saveRef.current(combo);
        setRecording(false);
      }
    };
    window.addEventListener("keydown", handler, true);
    return () => window.removeEventListener("keydown", handler, true);
  }, [recording]);

  const parts = parseShortcutParts(shortcut);

  return (
    <div className="flex items-center gap-3.5 py-1.5">
      <div className="flex-1 min-w-0">
        <div className="text-[13px] font-medium text-surface-100">{label}</div>
        {description && <div className="text-[10.5px] text-surface-500">{description}</div>}
      </div>
      <div className="flex items-center gap-1.5">
        <button
          type="button"
          onClick={() => setRecording((r) => !r)}
          className={`min-w-[120px] min-h-[30px] px-2.5 py-1.5 rounded-[7px] border flex items-center justify-center gap-1 ${
            recording
              ? "border-orange-500 bg-orange-500/10"
              : "border-surface-600 bg-surface-800 hover:bg-surface-700"
          }`}
        >
          {recording ? (
            <span className="text-xs text-orange-400">Press keys…</span>
          ) : parts.length === 0 ? (
            <span className="text-xs text-surface-500">Not set</span>
          ) : (
            parts.map((p, i) => (
              <kbd
                key={i}
                className="min-w-[24px] min-h-[22px] px-1.5 text-[11px] font-medium text-surface-100 bg-surface-900/80 border border-surface-600 rounded-[5px] inline-flex items-center justify-center"
              >
                {partSymbol(p)}
              </kbd>
            ))
          )}
        </button>
        <button
          type="button"
          title="Clear shortcut"
          disabled={!shortcut}
          onClick={() => onSave("")}
          className={`w-7 h-7 rounded-[7px] border border-surface-600 text-surface-400 text-[15px] leading-none flex items-center justify-center ${
            shortcut ? "opacity-100 hover:bg-surface-700" : "opacity-0 pointer-events-none"
          }`}
        >
          ×
        </button>
      </div>
    </div>
  );
}

// ── LevelMeter (My Voice enrollment) ────────────────────────────────────────

export function LevelMeter({ level }: { level: number }) {
  const frac = Math.sqrt(Math.min(1, level / 0.05));
  return (
    <div className="flex-1 h-1.5 rounded-full bg-surface-500/15 overflow-hidden">
      <div
        className="h-full bg-primary-500 rounded-full transition-[width] duration-100"
        style={{ width: `${frac * 100}%` }}
      />
    </div>
  );
}

// ── Spinner ─────────────────────────────────────────────────────────────────

export function Spinner() {
  return (
    <svg className="w-3.5 h-3.5 animate-spin text-surface-400" viewBox="0 0 24 24" fill="none">
      <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
      <path className="opacity-90" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.4 0 0 5.4 0 12h4z" />
    </svg>
  );
}
