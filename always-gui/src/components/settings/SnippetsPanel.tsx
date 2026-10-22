import { useCallback, useEffect, useRef, useState } from "react";
import { appFilePath, readAppFile, revealPath, writeAppFile } from "../../lib/tauri";
import { Btn, Caption, Card, Divider, PanelTitle, Spinner } from "../controls";

/// One voice snippet: saying `trigger` as a whole utterance (or anywhere in
/// a sentence) pastes `expansion` verbatim. Mirrors the daemon's
/// `snippets.rs` `Entry` shape — only `trigger` and `expansion` are
/// serialized; `id` is client-side only, like the Swift panel's UUID.
interface VoiceSnippet {
  id: string;
  trigger: string;
  expansion: string;
}

/// `~/.always/snippets.json`. The daemon re-reads the file on every paste,
/// so a save here is live for the very next utterance — no restart, no UDS
/// round-trip. Never clobber a hand-edited file we failed to parse: surface
/// the error and leave editing/saving disabled until the user fixes or
/// reloads (same contract as `SnippetsStore` in SnippetsPanel.swift).
const FILE = "snippets.json";

const newId = () =>
  typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random().toString(36).slice(2)}`;

/// Read the file (absent ⇒ empty list, same as the Tauri command), keep ids
/// stable, and remember where each snippet's raw text came from.
function parse(raw: string): VoiceSnippet[] {
  const parsed = JSON.parse(raw || "[]");
  if (!Array.isArray(parsed)) throw new Error("expected a JSON array");
  return parsed.map((e: any) => ({
    id: newId(),
    trigger: typeof e?.trigger === "string" ? e.trigger : "",
    expansion: typeof e?.expansion === "string" ? e.expansion : "",
  }));
}

/// Serialize back to the daemon's shape (no `id` on disk), pretty-printed
/// like the Swift `JSONEncoder` ([.prettyPrinted, .withoutEscapingSlashes]).
function serialize(snippets: VoiceSnippet[]): string {
  return `${JSON.stringify(
    snippets.map(({ trigger, expansion }) => ({ trigger, expansion })),
    null,
    2,
  )}\n`;
}

export function SnippetsPanel() {
  const [snippets, setSnippets] = useState<VoiceSnippet[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [path, setPath] = useState("");
  const pendingSave = useRef(0);

  /// Debounced save: the Swift panel writes on every field change, which
  /// over IPC would mean a write per keystroke — coalesce to one trailing
  /// write instead.
  const save = useCallback(async (next: VoiceSnippet[]) => {
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
        setError(`Failed to save snippets.json: ${String(e)}`);
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
      setSnippets(parse(raw));
    } catch (e: any) {
      setError(`snippets.json could not be read: ${String(e)}`);
      setSnippets([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const update = (id: string, patch: Partial<VoiceSnippet>) => {
    if (error) return;
    setSnippets((prev) => {
      const next = prev.map((s) => (s.id === id ? { ...s, ...patch } : s));
      void save(next);
      return next;
    });
  };

  const add = () => {
    if (error) return;
    setSnippets((prev) => {
      const next = [...prev, { id: newId(), trigger: "", expansion: "" }];
      void save(next);
      return next;
    });
  };

  const remove = (snippet: VoiceSnippet) => {
    if (!window.confirm(`Delete snippet "${snippet.trigger}"?`)) return;
    setSnippets((prev) => {
      const next = prev.filter((s) => s.id !== snippet.id);
      void save(next);
      return next;
    });
  };

  return (
    <div className="p-5 space-y-3">
      <div className="flex items-center gap-2">
        <PanelTitle>Voice Snippets</PanelTitle>
        {saving && <Spinner />}
        <div className="flex-1" />
        {snippets.length > 0 && (
          <Caption>
            {snippets.length} snippet{snippets.length === 1 ? "" : "s"}
          </Caption>
        )}
      </div>
      <Caption>
        Say a trigger phrase — alone or anywhere in a sentence — and Always replaces it with the
        expansion: exact text, no AI rewriting. Matching ignores case and punctuation. Changes apply
        to the very next thing you say.
      </Caption>

      {error && (
        <div className="flex items-center gap-2 rounded-md bg-orange-500/10 border border-orange-500/30 px-3 py-2">
          <svg className="w-3.5 h-3.5 text-orange-400 shrink-0" viewBox="0 0 24 24" fill="currentColor">
            <path d="M12 2 1 21h22L12 2zm1 14h-2v-2h2v2zm0-4h-2V8h2v4z" />
          </svg>
          <div className="flex-1 text-xs text-orange-300">{error}</div>
          <Btn small onClick={load}>Reload</Btn>
        </div>
      )}

      {loading && <div className="flex items-center gap-2"><Spinner /><Caption>Loading snippets…</Caption></div>}

      {!loading && !error && snippets.length === 0 && (
        <div className="rounded-lg bg-white/[0.08] p-6 text-center">
          <Caption className="italic">No snippets yet. Add one to paste boilerplate with your voice.</Caption>
        </div>
      )}

      <div className="space-y-2">
        {snippets.map((s) => (
          <Card key={s.id} className="space-y-2">
            <div className="flex items-center gap-2">
              <svg className="w-3.5 h-3.5 text-primary-400 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor">
                <path strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" d="M21 15a2 2 0 01-2 2H7l-4 4V5a2 2 0 012-2h14a2 2 0 012 2v10z" />
              </svg>
              <input
                value={s.trigger}
                disabled={!!error}
                placeholder="Trigger phrase — e.g. grill me"
                onChange={(e) => update(s.id, { trigger: e.target.value })}
                className="flex-1 min-w-0 bg-surface-800 border border-surface-700 rounded px-2 py-1 text-[13px] text-white placeholder:text-surface-500 focus:outline-none focus:ring-1 focus:ring-primary-500 disabled:opacity-40"
              />
              <Btn small destructive title="Delete this snippet" onClick={() => remove(s)}>✕</Btn>
            </div>
            <textarea
              value={s.expansion}
              disabled={!!error}
              placeholder="Expansion — pasted verbatim when the trigger is said"
              onChange={(e) => update(s.id, { expansion: e.target.value })}
              className="w-full min-h-[72px] max-h-[180px] resize-y bg-surface-900 border border-surface-700 rounded p-1.5 text-[12px] font-mono text-white placeholder:text-surface-500 focus:outline-none focus:ring-1 focus:ring-primary-500 disabled:opacity-40"
            />
            {!s.expansion && (
              <Caption className="text-orange-400">
                Expansion is empty — saying the trigger will paste nothing.
              </Caption>
            )}
          </Card>
        ))}
      </div>

      <Btn small prominent disabled={!!error} onClick={add}>+ Add Snippet</Btn>

      <Divider />

      <div className="flex items-center gap-2">
        <Btn small onClick={load} title="Reload from disk (picks up hand-edits)" disabled={loading}>
          Reload
        </Btn>
        {path && (
          <button
            type="button"
            onClick={() => revealPath(path)}
            className="text-[10px] font-mono text-surface-500 hover:text-surface-300 truncate text-left"
            title="Show in Finder"
          >
            {path}
          </button>
        )}
      </div>
    </div>
  );
}