import { useEffect, useState } from "react";
import { clearHistory, historyFilePath, readHistory, revealPath, type HistoryEntry } from "../../lib/tauri";
import { Btn, Caption, PanelTitle, Spinner } from "../controls";

/// Settings → History: today's transcripts. Mirrors HistoryPanel.swift —
/// loads the history file, shows entries newest-first, refresh / export-all /
/// delete-all (confirm) / copy-entry.
export function HistoryPanel() {
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState<number | null>(null);
  const [path, setPath] = useState("");

  const load = async () => {
    setLoading(true);
    setError(null);
    try {
      const [rows, p] = await Promise.all([readHistory(), historyFilePath()]);
      setPath(p);
      const dayStart = new Date();
      dayStart.setHours(0, 0, 0, 0);
      const today = rows.filter((e) => e.timestamp_ms >= dayStart.getTime());
      setEntries(today.reverse());
    } catch (e: any) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  const copy = async (text: string, i: number) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(i);
      setTimeout(() => setCopied(null), 1200);
    } catch {
      /* clipboard unavailable */
    }
  };

  const exportAll = async () => {
    try {
      const rows = await readHistory();
      const blob = new Blob([rows.map((r) => r.text).join("\n\n")], { type: "text/plain" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `always-transcripts-${new Date().toISOString().slice(0, 10)}.txt`;
      a.click();
      URL.revokeObjectURL(url);
    } catch {
      /* ignore */
    }
  };

  const deleteAll = async () => {
    if (!window.confirm("Delete all transcript history? This can't be undone.")) return;
    await clearHistory().catch(() => {});
    setEntries([]);
  };

  return (
    <div className="p-5 space-y-3">
      <div className="flex items-center gap-2">
        <PanelTitle>Transcript History</PanelTitle>
        <div className="flex-1" />
        <Btn small onClick={load} disabled={loading}>{loading ? <Spinner /> : null}Refresh</Btn>
        <Btn small onClick={exportAll}>Export All</Btn>
        <Btn small destructive onClick={deleteAll}>Delete All</Btn>
      </div>
      <Caption>Today · {entries.length} {entries.length === 1 ? "entry" : "entries"}</Caption>

      {error && (
        <div className="rounded-md bg-red-500/10 border border-red-500/30 px-3 py-2 text-xs text-red-400">{error}</div>
      )}

      {!loading && !error && entries.length === 0 && (
        <div className="rounded-lg bg-surface-500/[0.08] p-6 text-center">
          <Caption className="italic">No transcripts yet today — say something.</Caption>
        </div>
      )}

      <div className="space-y-1.5">
        {entries.map((e, i) => (
          <div key={`${e.timestamp_ms}-${i}`} className="rounded-lg bg-surface-500/[0.08] p-2.5">
            <div className="flex items-start gap-2">
              <div className="flex-1 min-w-0">
                <div className="text-sm text-surface-100 whitespace-pre-wrap">{e.text}</div>
                <Caption className="mt-0.5">
                  {new Date(e.timestamp_ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}
                </Caption>
              </div>
              <Btn small onClick={() => copy(e.text, i)}>{copied === i ? "Copied" : "Copy"}</Btn>
            </div>
          </div>
        ))}
      </div>

      {path && (
        <button
          type="button"
          onClick={() => revealPath(path)}
          className="text-[10px] text-surface-500 hover:text-surface-300 truncate w-full text-left"
          title={path}
        >
          {path}
        </button>
      )}
    </div>
  );
}
