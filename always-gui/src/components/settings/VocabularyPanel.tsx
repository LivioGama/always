import { useCallback, useEffect, useRef, useState } from "react";
import { appFilePath, openPath, readAppFile, revealPath, writeAppFile } from "../../lib/tauri";
import { Btn, Caption, PanelTitle, Spinner } from "../controls";

/// One glossary entry. Mirrors the daemon's `glossary.rs` `Entry` shape —
/// only `term`, `mistranscriptions`, `frequency`, and `weight` are
/// serialized; `frequency` defaults to 0 and `weight` to 1 when absent.
/// `id` is client-side only, like the Swift panel's UUID.
interface GlossaryEntry {
  id: string;
  term: string;
  mistranscriptions: string[];
  frequency: number;
  weight: number;
}

/// `~/.always/glossary.json`. The daemon re-reads the file on every
/// transcription, so a save here is live for the next utterance — no
/// restart, no UDS round-trip. Never clobber a hand-edited file we failed
/// to parse: surface the error and leave deleting/saving disabled until
/// the user fixes or reloads (same contract as `VocabularyPanel.swift`).
const FILE = "glossary.json";

const newId = () =>
  typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random().toString(36).slice(2)}`;

/// Read the file (absent ⇒ empty list, same as the Tauri command) and
/// apply the daemon's defaults for omitted fields.
function parse(raw: string): GlossaryEntry[] {
  const parsed = JSON.parse(raw || "[]");
  if (!Array.isArray(parsed)) throw new Error("expected a JSON array");
  return parsed.map((e: any) => ({
    id: newId(),
    term: typeof e?.term === "string" ? e.term : "",
    mistranscriptions: Array.isArray(e?.mistranscriptions)
      ? e.mistranscriptions.filter((m: any) => typeof m === "string")
      : [],
    frequency: typeof e?.frequency === "number" ? e.frequency : 0,
    weight: typeof e?.weight === "number" ? e.weight : 1,
  }));
}

/// Serialize back to the daemon's shape (no `id` on disk), pretty-printed
/// like the Swift `JSONEncoder` ([.prettyPrinted, .withoutEscapingSlashes]).
function serialize(entries: GlossaryEntry[]): string {
  return `${JSON.stringify(
    entries.map(({ term, mistranscriptions, frequency, weight }) => ({
      term,
      mistranscriptions,
      frequency,
      weight,
    })),
    null,
    2,
  )}\n`;
}

/// Settings → Vocabulary: glossary terms that bias transcription toward
/// your preferred spellings and substitutions. Mirrors VocabularyPanel.swift.
export function VocabularyPanel() {
  const [entries, setEntries] = useState<GlossaryEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [path, setPath] = useState("");
  const [pendingDelete, setPendingDelete] = useState<GlossaryEntry | null>(null);
  const pendingSave = useRef(0);

  /// Debounced save: coalesce rapid deletes into one trailing write.
  const save = useCallback(async (next: GlossaryEntry[]) => {
    const token = ++pendingSave.current;
    setSaving(true);
    try {
      await writeAppFile(FILE, serialize(next));
      if (pendingSave.current === token) {
        setError(null);
        setSaving(false);
      }
    } catch (e: any) {
      if (pendingSave.current === token) {
        setError(`Failed to save glossary.json: ${String(e)}`);
        setSaving(false);
      }
    }
  }, []);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [raw, p] = await Promise.all([readAppFile(FILE), appFilePath(FILE)]);
      setPath(p);
      setEntries(parse(raw));
    } catch (e: any) {
      setError(`glossary.json could not be read: ${String(e)}`);
      setEntries([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const confirmDelete = () => {
    const victim = pendingDelete;
    setPendingDelete(null);
    if (!victim || error) return;
    setEntries((prev) => {
      const next = prev.filter((e) => e.id !== victim.id);
      void save(next);
      return next;
    });
  };

  const badge = (text: string, className: string) => (
    <span
      className={`text-[10px] font-medium px-1.5 py-0.5 rounded shrink-0 ${className}`}
    >
      {text}
    </span>
  );

  return (
    <div className="p-5 space-y-3">
      <div className="flex items-center gap-2">
        <PanelTitle>Vocabulary</PanelTitle>
        {saving && <Spinner />}
        <div className="flex-1" />
        {entries.length > 0 && (
          <Caption>
            {entries.length} term{entries.length === 1 ? "" : "s"}
          </Caption>
        )}
      </div>
      <Caption>
        Glossary terms bias transcription toward your preferred spellings and substitutions. Delete
        entries here or edit glossary.json directly to add them.
      </Caption>

      {/* Toolbar */}
      <div className="flex items-center gap-2">
        <Btn small onClick={() => openPath(path)} disabled={!path}>
          Open glossary.json
        </Btn>
        <Btn small onClick={() => revealPath(path)} disabled={!path} title="Show in Finder">
          Show in Finder
        </Btn>
        <Btn small onClick={load} title="Reload from disk (picks up hand-edits)" disabled={loading}>
          Reload
        </Btn>
      </div>
      {path && (
        <button
          type="button"
          onClick={() => revealPath(path)}
          className="block text-[10px] font-mono text-surface-500 hover:text-surface-300 truncate text-left w-full"
          title="Show in Finder"
        >
          {path}
        </button>
      )}

      {error && (
        <div className="flex items-center gap-2 rounded-md bg-orange-500/10 border border-orange-500/30 px-3 py-2">
          <svg className="w-3.5 h-3.5 text-orange-400 shrink-0" viewBox="0 0 24 24" fill="currentColor">
            <path d="M12 2 1 21h22L12 2zm1 14h-2v-2h2v2zm0-4h-2V8h2v4z" />
          </svg>
          <div className="flex-1 text-xs text-orange-300">{error}</div>
          <Btn small onClick={load}>Reload</Btn>
        </div>
      )}

      {loading && <div className="flex items-center gap-2"><Spinner /><Caption>Loading vocabulary…</Caption></div>}

      {!loading && !error && entries.length === 0 && (
        <div className="rounded-lg bg-white/[0.08] p-6 text-center">
          <Caption className="italic">No glossary entries yet. Open glossary.json to add terms.</Caption>
        </div>
      )}

      <div className="space-y-2">
        {entries.map((e) => (
          <div key={e.id} className="rounded-lg bg-white/[0.08] px-3 py-2">
            <div className="flex items-center gap-2">
              <svg className="w-3.5 h-3.5 text-primary-400 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor">
                <path strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" d="M7.5 8.5h9M7.5 12h6M4 5h16v11l-4 4H4V5z" />
              </svg>
              <div className="flex-1 min-w-0 text-[13px] font-medium text-surface-100 truncate">{e.term}</div>
              {e.weight > 1 && badge(`${e.weight}×`, "text-primary-300 bg-primary-500/15")}
              {e.frequency > 0 && badge(`${e.frequency} freq`, "text-surface-300 bg-white/10")}
              <button
                type="button"
                title="Delete this entry"
                onClick={() => setPendingDelete(e)}
                disabled={!!error}
                className="text-surface-500 hover:text-red-400 shrink-0 disabled:opacity-40"
              >
                <svg className="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor">
                  <path strokeWidth={2} strokeLinecap="round" d="M3 6h18M8 6V4h8v2m-9 0l1 14h8l1-14" />
                </svg>
              </button>
            </div>
            {e.mistranscriptions.length > 0 && (
              <div className="flex items-start gap-1.5 pt-1.5 pl-5.5">
                <svg className="w-3 h-3 mt-0.5 text-surface-500 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor">
                  <path strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" d="M5 12h4m0 0c0 3-2 5-4 5m4-5c0 3 2 5 4 5M19 12h-4m0 0c0-3 2-5 4-5m-4 5c0-3-2-5-4-5" />
                </svg>
                <Caption className="truncate">heard as: {e.mistranscriptions.join(", ")}</Caption>
              </div>
            )}
          </div>
        ))}
      </div>

      {pendingDelete && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="bg-surface-900 border border-surface-700 rounded-xl p-5 w-80 shadow-2xl">
            <div className="text-[13px] font-semibold text-white mb-1">Delete "{pendingDelete.term}"?</div>
            <Caption className="mb-4">This removes the term and its learned mistranscriptions.</Caption>
            <div className="flex gap-2 justify-end">
              <Btn small onClick={() => setPendingDelete(null)}>Cancel</Btn>
              <Btn small destructive onClick={confirmDelete}>Delete</Btn>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}