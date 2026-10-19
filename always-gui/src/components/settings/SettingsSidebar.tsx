import type { ReactNode } from "react";
import type { DaemonUiState } from "../../hooks/useDaemonState";

export type SettingsPanelKey =
  | "general"
  | "models"
  | "myVoice"
  | "permissions"
  | "behavior"
  | "shortcuts"
  | "library"
  | "history"
  | "about";

const PANELS: { key: SettingsPanelKey; label: string; icon: ReactNode }[] = [
  {
    key: "general", label: "General",
    icon: <svg className="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor"><rect x="3" y="3" width="18" height="18" rx="4" strokeWidth={2} /><circle cx="17" cy="7" r="4" fill="currentColor" stroke="none" /><path stroke="white" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round" d="m15.5 7 1 1 2-2" /></svg>,
  },
  {
    key: "models", label: "Models",
    icon: <svg className="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor"><rect x="5" y="5" width="14" height="14" rx="2" strokeWidth={2} /><path strokeWidth={2} d="M9 2v3M15 2v3M9 19v3M15 19v3M2 9h3M2 15h3M19 9h3M19 15h3" /></svg>,
  },
  {
    key: "myVoice", label: "My Voice",
    icon: <svg className="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor"><circle cx="12" cy="8" r="3.5" strokeWidth={2} /><path strokeWidth={2} strokeLinecap="round" d="M4 20c1.5-3.5 4.5-5 8-5s6.5 1.5 8 5M2 10c.8-2 1.8-3 3-3m17 3c-.8-2-1.8-3-3-3" /></svg>,
  },
  {
    key: "permissions", label: "Permissions",
    icon: <svg className="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor"><path strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" d="M12 3l7 3v5c0 4.5-3 8.5-7 10-4-1.5-7-5.5-7-10V6l7-3z" /><path strokeWidth={2} strokeLinecap="round" d="M9 12l2 2 4-4" /></svg>,
  },
  {
    key: "behavior", label: "Behavior",
    icon: <svg className="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor"><path strokeWidth={2} strokeLinecap="round" d="M4 8h10M18 8h2M4 16h2M10 16h10" /><circle cx="16" cy="8" r="2" strokeWidth={2} /><circle cx="8" cy="16" r="2" strokeWidth={2} /></svg>,
  },
  {
    key: "shortcuts", label: "Shortcuts",
    icon: <svg className="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor"><path strokeWidth={2} strokeLinecap="round" d="M9 9V4.5M9 9L4.5 4M9 9H4.5M15 15v4.5m0-4.5l4.5 5m-4.5-5h4.5" /></svg>,
  },
  {
    key: "library", label: "Library",
    icon: <svg className="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor"><path strokeWidth={2} strokeLinecap="round" d="M5 4v16M9 4v16M14 5l5 14" /></svg>,
  },
  {
    key: "history", label: "History",
    icon: <svg className="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor"><circle cx="12" cy="12" r="9" strokeWidth={2} /><path strokeWidth={2} strokeLinecap="round" d="M12 7v5l3 3" /></svg>,
  },
  {
    key: "about", label: "About",
    icon: <svg className="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor"><circle cx="12" cy="12" r="9" strokeWidth={2} /><path strokeWidth={2} strokeLinecap="round" d="M12 16v-4m0-4h.01" /></svg>,
  },
];

export function SettingsSidebar({
  selected,
  onSelect,
  state,
}: {
  selected: SettingsPanelKey;
  onSelect: (p: SettingsPanelKey) => void;
  state: DaemonUiState;
}) {
  return (
    <div className="w-[200px] shrink-0 bg-surface-900 border-r border-white/10 py-5 flex flex-col">
      <div className="flex flex-col gap-1 px-2">
        {PANELS.map((p) => {
          const active = selected === p.key;
          return (
            <button
              key={p.key}
              type="button"
              onClick={() => onSelect(p.key)}
              className={`flex items-center gap-2.5 px-4 py-2 rounded-md text-[13px] text-left w-full transition-colors ${
                active ? "bg-primary-500 text-white" : "text-white/90 hover:bg-white/5"
              }`}
            >
              <span className={`w-[18px] flex items-center justify-center ${active ? "text-white" : "text-surface-400"}`}>
                {p.icon}
              </span>
              {p.label}
            </button>
          );
        })}
      </div>
      <div className="flex-1" />
      <div className="border-t border-white/10 mx-3" />
      <StatusFooter state={state} />
    </div>
  );
}

function StatusFooter({ state }: { state: DaemonUiState }) {
  const running = state.isDaemonConnected;
  const label = running ? "Running" : "Disconnected";
  const color = running ? "text-green-500" : "text-orange-500";
  return (
    <div className={`flex items-center gap-2 px-4 pt-3 ${color}`}>
      {running ? (
        <svg className="w-3.5 h-3.5" viewBox="0 0 24 24" fill="currentColor"><path d="M12 2a10 10 0 100 20 10 10 0 000-20zm-1.5 14.5l-4-4 1.4-1.4 2.6 2.6 5.6-5.6 1.4 1.4-7 7z" /></svg>
      ) : (
        <svg className="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor"><path strokeWidth={2} strokeLinecap="round" d="M12 8v5m0 3h.01M10.3 3.9L1.8 18a2 2 0 001.7 3h17a2 2 0 001.7-3L13.7 3.9a2 2 0 00-3.4 0z" /></svg>
      )}
      <span className="text-xs">{label}</span>
    </div>
  );
}
