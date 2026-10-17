import { useCallback, useEffect, useState } from "react";
import { appFilePath, openPath, readAppFile, writeAppFile } from "../../lib/tauri";
import { Btn, Caption, PanelTitle, Segmented, Spinner } from "../controls";

/// Settings → Library: Vocabulary (glossary.json) + Snippets (snippets.json).
/// Mirrors LibraryPanel.swift hosting VocabularyPanel / SnippetsPanel —
/// same on-disk shapes the daemon reads.

export function LibraryPanel() {
  const [tab, setTab] = useState<"vocabulary" | "snippets">("vocabulary");
  return (
    <div className="p-5 space-y-4">
      <div className="flex items-center gap-2">
        <PanelTitle>Library</PanelTitle>
        <div className="flex-1" />
        <Segmented
          width={220}
          value={tab}
          onChange={setTab}
          options={[
            { label: "Vocabulary", value: "vocabulary" },
            { label: "Snippets", value: "snippets" },
          ]}
        />
      </div>
      {tab === "vocabulary" ? <VocabularyPanel /> : <SnippetsPanel />}
    </div>
  );
}

// ── Generic JSON file plumbing ──────────────────────────────────────────────

function useJsonFile<T>(name: string) {
  const [data, setData] = useState<T | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoadError(null);
    try {
      const raw = await readAppFile(name);
      setData(raw.trim() ? (JSON.parse(raw) as T) : ([] as unknown as T));
    } catch (e: any) {
      // Never clobber a hand-edited file we failed to parse.
      setLoadError(`${name} could not be read: ${e?.message ?? e}`);
      setData(null);
    }
  }, [name]);

  useEffect(() => {
    load();
  }, [load]);

  const save = useCallback(
    async (next: T) => {
      if (loadError) return;
      setSaveError(null);
      try {
        await writeAppFile(name, JSON.stringify(next, null, 2) + "\n");
        setData(next);
      } catch (e: any) {
        setSaveError(`Failed to save ${name}: ${e?.message ?? e}`);
      }
    },
    [name, loadError],
  );

  return { data, save, reload: load, loading: data === null && !loadError, loadError, saveError };
}

// ── Vocabulary — glossary.json, daemon's `glossary.rs` Entry shape ──────────

interface GlossaryEntry {
  term: string;
  mistranscriptions: string[];
  frequency: number;
  weight: number;
}

function VocabularyPanel() {
  const { data, save, loading, loadError, saveError } = useJsonFile<GlossaryEntry[]>("glossary.json");
  const [path, setPath] = useState("");
  const [pendingDelete, setPendingDelete] = useState<GlossaryEntry | null>(null);

  useEffect(() => {
    appFilePath("glossary.json").then(setPath).catch(() => {});
  }, []);

  if (loading) return <Spinner />;

  const entries = data ?? [];

  return (
    <div className="space-y-3">
      <Caption>
        Glossary terms bias transcription toward your preferred spellings and substitutions. Delete entries
        here or edit glossary.json directly to add them.
      </Caption>
      {(loadError || saveError) && (
        <div className="rounded-md bg-red-500/10 border border-red-500/30 px-3 py-2 text-xs text-red-400">
          {loadError ?? saveError}
        </div>
      )}

      {entries.length === 0 ? (
        <div className="rounded-lg bg-white/[0.08] p-5 text-center">
          <Caption className="italic">No glossary entries yet. Open glossary.json to add terms.</Caption>
        </div>
      ) : (
        <div className="space-y-1.5">
          {[...entries].sort((a, b) => a.term.localeCompare(b.term)).map((e) => (
            <div key={e.term} className="rounded-lg bg-white/[0.08] px-3 py-2">
              <div className="flex items-start gap-2">
                <div className="flex-1 min-w-0">
                  <div className="text-[13px] font-medium text-surface-100">{e.term}</div>
                  {e.mistranscriptions.length > 0 && (
                    <Caption className="truncate">
                      heard as: {e.mistranscriptions.join(", ")}
                    </Caption>
                  )}
                </div>
                <Caption className="shrink-0">×{e.frequency}</Caption>
                <button
                  type="button"
                  title="Delete entry"
                  onClick={() => setPendingDelete(e)}
                  className="text-surface-500 hover:text-red-400 shrink-0"
                >
                  <svg className="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor">
                    <path strokeWidth={2} strokeLinecap="round" d="M3 6h18M8 6V4h8v2m-9 0l1 14h8l1-14" />
                  </svg>
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      <div className="flex items-center gap-2">
        <Caption>{entries.length} {entries.length === 1 ? "entry" : "entries"}</Caption>
        <div className="flex-1" />
        <Btn small onClick={() => openPath(path)}>Open glossary.json</Btn>
      </div>
      {path && <Caption className="truncate">{path}</Caption>}

      {pendingDelete && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="bg-surface-900 border border-surface-700 rounded-xl p-5 w-80 shadow-2xl">
            <div className="text-[13px] font-semibold text-white mb-1">Delete "{pendingDelete.term}"?</div>
            <Caption className="mb-4">This removes the term and its learned mistranscriptions.</Caption>
            <div className="flex gap-2 justify-end">
              <Btn small onClick={() => setPendingDelete(null)}>Cancel</Btn>
              <Btn
                small destructive
                onClick={() => {
                  save(entries.filter((x) => x !== pendingDelete));
                  setPendingDelete(null);
                }}
              >
                Delete
              </Btn>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

// ── Snippets — snippets.json, daemon's entry shape {trigger, expansion} ─────

interface VoiceSnippet {
  trigger: string;
  expansion: string;
}

function SnippetsPanel() {
  const { data, save, loading, loadError, saveError } = useJsonFile<VoiceSnippet[]>("snippets.json");
  const [path, setPath] = useState("");
  const [trigger, setTrigger] = useState("");
  const [expansion, setExpansion] = useState("");
  const [pendingDelete, setPendingDelete] = useState<VoiceSnippet | null>(null);

  useEffect(() => {
    appFilePath("snippets.json").then(setPath).catch(() => {});
  }, []);

  if (loading) return <Spinner />;

  const snippets = data ?? [];

  const add = () => {
    const t = trigger.trim();
    const x = expansion.trim();
    if (!t || !x) return;
    save([...snippets.filter((s) => s.trigger !== t), { trigger: t, expansion: x }]);
    setTrigger("");
    setExpansion("");
  };

  return (
    <div className="space-y-3">
      <Caption>
        Say a trigger phrase as a whole utterance to paste the expansion verbatim — signatures, addresses,
        links. The file is re-read on every paste, so changes are live immediately.
      </Caption>
      {(loadError || saveError) && (
        <div className="rounded-md bg-red-500/10 border border-red-500/30 px-3 py-2 text-xs text-red-400">
          {loadError ?? saveError}
        </div>
      )}

      <div className="space-y-2">
        <input
          value={trigger}
          onChange={(e) => setTrigger(e.target.value)}
          placeholder='Trigger phrase — e.g. "my signature"'
          className="w-full bg-surface-800 border border-surface-700 rounded px-2.5 py-1.5 text-xs text-white focus:outline-none focus:ring-1 focus:ring-primary-500"
        />
        <textarea
          value={expansion}
          onChange={(e) => setExpansion(e.target.value)}
          placeholder="Text to paste…"
          rows={2}
          className="w-full bg-surface-800 border border-surface-700 rounded px-2.5 py-1.5 text-xs text-white focus:outline-none focus:ring-1 focus:ring-primary-500 resize-y"
        />
        <div className="flex justify-end">
          <Btn prominent onClick={add} disabled={!trigger.trim() || !expansion.trim() || !!loadError}>
            Add Snippet
          </Btn>
        </div>
      </div>

      {snippets.length === 0 ? (
        <div className="rounded-lg bg-white/[0.08] p-5 text-center">
          <Caption className="italic">No snippets yet.</Caption>
        </div>
      ) : (
        <div className="divide-y divide-white/[0.08] rounded-lg bg-white/[0.08]">
          {[...snippets].sort((a, b) => a.trigger.localeCompare(b.trigger)).map((s) => (
            <div key={s.trigger} className="flex items-start gap-2.5 px-3 py-2">
              <div className="flex-1 min-w-0">
                <div className="text-[13px] text-surface-100 truncate">{s.trigger}</div>
                <Caption className="line-clamp-2">{s.expansion}</Caption>
              </div>
              <button
                type="button"
                title="Delete"
                onClick={() => setPendingDelete(s)}
                className="text-surface-500 hover:text-red-400 mt-0.5"
              >
                <svg className="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor">
                  <path strokeWidth={2} strokeLinecap="round" d="M3 6h18M8 6V4h8v2m-9 0l1 14h8l1-14" />
                </svg>
              </button>
            </div>
          ))}
        </div>
      )}
      <div className="flex items-center gap-2">
        <Caption>{snippets.length} {snippets.length === 1 ? "snippet" : "snippets"}</Caption>
        <div className="flex-1" />
        <Btn small onClick={() => openPath(path)}>Open snippets.json</Btn>
      </div>

      {pendingDelete && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="bg-surface-900 border border-surface-700 rounded-xl p-5 w-80 shadow-2xl">
            <div className="text-[13px] font-semibold text-white mb-1">Delete "{pendingDelete.trigger}"?</div>
            <Caption className="mb-4">This snippet will no longer expand.</Caption>
            <div className="flex gap-2 justify-end">
              <Btn small onClick={() => setPendingDelete(null)}>Cancel</Btn>
              <Btn
                small destructive
                onClick={() => {
                  save(snippets.filter((x) => x !== pendingDelete));
                  setPendingDelete(null);
                }}
              >
                Delete
              </Btn>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
