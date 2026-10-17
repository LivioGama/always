import { useEffect, useMemo, useState } from "react";
import type { CatalogModel, DaemonApi } from "../../hooks/useDaemonState";
import { resolvedPostprocessMode, type Config } from "../../lib/config";
import { configDelete, configSet } from "../../lib/tauri";
import { Btn, Caption, Divider, PanelTitle, Segmented, Spinner, Toggle } from "../controls";

/// Settings → Models: post-processing mode + local model catalog.
/// Mirrors ModelsPanel.swift + ModelsSection.swift.
export function ModelsPanel({ daemon, config, onConfigChange }: {
  daemon: DaemonApi;
  config: Config;
  onConfigChange: (patch: Partial<Config>) => void;
}) {
  const s = daemon.state;
  const mode = resolvedPostprocessMode(config);

  useEffect(() => {
    if (s.isDaemonConnected) daemon.listModels();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [s.isDaemonConnected]);

  const setMode = (m: string) => {
    onConfigChange({
      postprocessMode: m,
      postprocessEnabled: m !== "off",
      postprocessProvider: m === "apple" ? "apple" : m === "groq" ? "groq" : config.postprocessProvider,
    });
  };

  return (
    <div className="p-5 space-y-4">
      <GroqKeyCard config={config} onConfigChange={onConfigChange} />

      {/* Post-processing mode */}
      <div>
        <PanelTitle>Post-Processing</PanelTitle>
        <Caption className="mt-0.5">
          Optionally fix punctuation, casing, and false starts before pasting.
        </Caption>
        <div className="mt-2">
          <Segmented
            width={360}
            value={mode}
            onChange={setMode}
            options={[
              { label: "Always Fast", value: "fast" },
              { label: "Apple Intelligence", value: "apple" },
              { label: "Groq Cloud", value: "groq" },
              { label: "Off", value: "off" },
            ]}
          />
        </div>
        {mode === "apple" && !config.appleIntelligenceAvailable && (
          <Caption className="mt-1.5 text-orange-400">
            ⚠ Apple Intelligence isn't available on this platform — transcripts will pass through unedited.
          </Caption>
        )}
        {mode === "groq" && !config.groqKeySaved && (
          <Caption className="mt-1.5 text-orange-400">
            ⚠ Groq post-processing needs an API key — add one above or transcripts pass through unedited.
          </Caption>
        )}
      </div>

      <Divider />

      <ModelsSection daemon={daemon} config={config} onConfigChange={onConfigChange} />
    </div>
  );
}

// ── Groq API key card ────────────────────────────────────────────────────────

function GroqKeyCard({ config, onConfigChange }: {
  config: Config;
  onConfigChange: (patch: Partial<Config>) => void;
}) {
  const [draft, setDraft] = useState("");
  const [revealed, setRevealed] = useState(false);
  const [saving, setSaving] = useState(false);

  const save = async () => {
    const key = draft.trim();
    if (!key) return;
    setSaving(true);
    try {
      await configSet("groq_api_key", key);
      onConfigChange({ groqKeySaved: true, groqApiKey: key });
      setDraft("");
    } finally {
      setSaving(false);
    }
  };

  const remove = async () => {
    setSaving(true);
    try {
      await configDelete("groq_api_key");
      onConfigChange({ groqKeySaved: false, groqApiKey: null });
      setRevealed(false);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="rounded-lg bg-white/[0.05] p-3 space-y-2">
      <div className="flex items-center gap-2">
        <PanelTitle>Groq API Key</PanelTitle>
        <div className="flex-1" />
        {config.groqKeySaved && (
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-green-500/15 text-green-400">saved</span>
        )}
      </div>
      <Caption>
        Used for cloud transcription and Groq post-processing. Stored in your local config only.
      </Caption>
      <div className="flex items-center gap-2">
        <div className="relative flex-1">
          <input
            type={revealed ? "text" : "password"}
            value={config.groqKeySaved && !draft ? "••••••••••••••••" : draft}
            placeholder={config.groqKeySaved ? "Replace saved key" : "gsk_…"}
            onChange={(e) => setDraft(e.target.value)}
            className="w-full bg-surface-800 border border-surface-700 rounded px-2.5 py-1.5 pr-8 text-xs text-white font-mono focus:outline-none focus:ring-1 focus:ring-primary-500"
          />
          <button
            type="button"
            title={revealed ? "Hide" : "Show"}
            onClick={() => setRevealed((r) => !r)}
            className="absolute right-2 top-1/2 -translate-y-1/2 text-surface-500 hover:text-surface-300"
          >
            <svg className="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor">
              {revealed ? (
                <path strokeWidth={2} strokeLinecap="round" d="M3 3l18 18M10.5 10.7a2 2 0 002.8 2.8M7.4 7.6A9.8 9.8 0 002 12c2.2 4.4 6 7 10 7 1.7 0 3.2-.4 4.6-1M9.9 5.4A9.4 9.4 0 0112 5c4 0 7.8 2.6 10 7-.7 1.4-1.7 2.7-2.9 3.7" />
              ) : (
                <>
                  <path strokeWidth={2} strokeLinecap="round" d="M2 12c2.2-4.4 6-7 10-7s7.8 2.6 10 7c-2.2 4.4-6 7-10 7s-7.8-2.6-10-7z" />
                  <circle cx="12" cy="12" r="3" strokeWidth={2} />
                </>
              )}
            </svg>
          </button>
        </div>
        <Btn prominent onClick={save} disabled={!draft.trim() || saving}>
          {saving ? <Spinner /> : null}
          Save
        </Btn>
        {config.groqKeySaved && (
          <Btn destructive onClick={remove} disabled={saving}>Delete</Btn>
        )}
      </div>
    </div>
  );
}

// ── Local model catalog (ModelsSection.swift) ────────────────────────────────

type SortKey = "recommended" | "name" | "size" | "accuracy" | "speed";

function ModelsSection({ daemon, config, onConfigChange }: {
  daemon: DaemonApi;
  config: Config;
  onConfigChange: (patch: Partial<Config>) => void;
}) {
  const s = daemon.state;
  const [langFilter, setLangFilter] = useState<string>("all");
  const [onlyTranslation, setOnlyTranslation] = useState(false);
  const [onlyStreaming, setOnlyStreaming] = useState(false);
  const [onlyFast, setOnlyFast] = useState(false);
  const [sort, setSort] = useState<SortKey>("recommended");
  const [showAll, setShowAll] = useState(false);

  const languages = useMemo(() => {
    const set = new Set<string>();
    for (const m of s.models) for (const l of m.supported_languages) set.add(l);
    return [...set].sort();
  }, [s.models]);

  const filtered = useMemo(() => {
    let list = [...s.models];
    if (langFilter !== "all") list = list.filter((m) => m.supported_languages.includes(langFilter));
    if (onlyTranslation) list = list.filter((m) => m.supports_translation);
    if (onlyStreaming) list = list.filter((m) => m.supports_streaming);
    if (onlyFast) list = list.filter((m) => m.speed_score >= 8);
    switch (sort) {
      case "name": list.sort((a, b) => a.name.localeCompare(b.name)); break;
      case "size": list.sort((a, b) => a.size_mb - b.size_mb); break;
      case "accuracy": list.sort((a, b) => b.accuracy_score - a.accuracy_score); break;
      case "speed": list.sort((a, b) => b.speed_score - a.speed_score); break;
      default:
        list.sort((a, b) => Number(b.is_recommended) - Number(a.is_recommended) || b.accuracy_score - a.accuracy_score);
    }
    return list;
  }, [s.models, langFilter, onlyTranslation, onlyStreaming, onlyFast, sort]);

  const downloaded = filtered.filter((m) => m.is_downloaded);
  const available = filtered.filter((m) => !m.is_downloaded);
  const visibleAvailable = showAll ? available : available.slice(0, 8);
  const activeId = s.backend.startsWith("local:") ? s.backend.slice(6) : null;
  const cloudActive = s.backend === "groq" || s.backend === "apple";

  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2">
        <PanelTitle>Speech Recognition</PanelTitle>
        <div className="flex-1" />
        <Btn small onClick={daemon.listModels} disabled={!s.isDaemonConnected}>Refresh</Btn>
      </div>

      {/* Cloud option */}
      <div className="rounded-lg bg-white/[0.05] p-2.5 flex items-center gap-2.5">
        <svg className={`w-5 h-5 ${cloudActive ? "text-primary-400" : "text-surface-500"}`} viewBox="0 0 24 24" fill="none" stroke="currentColor">
          <path strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" d="M17.5 18a4.5 4.5 0 100-9 6 6 0 00-11.5 1.5A4 4 0 006.5 18h11z" />
        </svg>
        <div className="flex-1 min-w-0">
          <div className="text-[13px] font-medium text-surface-100">Groq Cloud</div>
          <Caption>Fastest and lightest — audio is transcribed on Groq's servers. Needs an API key.</Caption>
        </div>
        {cloudActive ? (
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-primary-500/15 text-primary-300">active</span>
        ) : (
          <Btn onClick={() => daemon.setActiveTranscriber("groq")} disabled={!s.isDaemonConnected || s.loadingBackend !== null}>
            {s.loadingBackend === "groq" ? <Spinner /> : null}Use
          </Btn>
        )}
      </div>

      {/* Language filter */}
      <div className="flex flex-wrap items-center gap-2">
        <Caption>Language</Caption>
        <select
          value={config.lang ?? "auto"}
          onChange={(e) => {
            const v = e.target.value;
            onConfigChange({ lang: v === "auto" ? null : v });
            daemon.setLanguage(v === "auto" ? null : v);
          }}
          className="bg-surface-800 border border-surface-700 rounded px-2 py-1 text-xs text-surface-200"
        >
          <option value="auto">Auto-detect</option>
          {languages.map((l) => <option key={l} value={l}>{l.toUpperCase()}</option>)}
        </select>
        <Caption>Catalog</Caption>
        <select
          value={langFilter}
          onChange={(e) => setLangFilter(e.target.value)}
          className="bg-surface-800 border border-surface-700 rounded px-2 py-1 text-xs text-surface-200"
        >
          <option value="all">All languages</option>
          {languages.map((l) => <option key={l} value={l}>{l.toUpperCase()}</option>)}
        </select>
        <select
          value={sort}
          onChange={(e) => setSort(e.target.value as SortKey)}
          className="bg-surface-800 border border-surface-700 rounded px-2 py-1 text-xs text-surface-200"
        >
          <option value="recommended">Recommended</option>
          <option value="name">Name</option>
          <option value="size">Size</option>
          <option value="accuracy">Accuracy</option>
          <option value="speed">Speed</option>
        </select>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <FilterChip on={onlyTranslation} onClick={() => setOnlyTranslation((v) => !v)}>Translation</FilterChip>
        <FilterChip on={onlyFast} onClick={() => setOnlyFast((v) => !v)}>Fast</FilterChip>
        <FilterChip on={onlyStreaming} onClick={() => setOnlyStreaming((v) => !v)}>Streaming</FilterChip>
        {onlyTranslation && (
          <Caption className="text-orange-400/80">Translation support is limited on this backend.</Caption>
        )}
      </div>

      {/* Downloaded */}
      {downloaded.length > 0 && (
        <div className="space-y-1.5">
          <Caption>Downloaded</Caption>
          {downloaded.map((m) => (
            <ModelCard key={m.id} m={m} daemon={daemon} active={activeId === m.id} />
          ))}
        </div>
      )}

      {/* Available */}
      <div className="space-y-1.5">
        <Caption>Available to download</Caption>
        {visibleAvailable.map((m) => (
          <ModelCard key={m.id} m={m} daemon={daemon} active={false} />
        ))}
        {available.length > 8 && (
          <button
            type="button"
            onClick={() => setShowAll((v) => !v)}
            className="text-xs text-primary-400 hover:text-primary-300"
          >
            {showAll ? "Show fewer" : `Show all ${available.length} models`}
          </button>
        )}
        {filtered.length === 0 && (
          <Caption className="italic">
            {s.isDaemonConnected ? "No models match the current filters." : "Connect the daemon to list models."}
          </Caption>
        )}
      </div>
    </div>
  );
}

function FilterChip({ on, onClick, children }: { on: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`px-2 py-0.5 rounded-full text-[10px] border ${
        on ? "bg-primary-500/15 border-primary-500/40 text-primary-300" : "border-surface-700 text-surface-400 hover:text-surface-200"
      }`}
    >
      {children}
    </button>
  );
}

function ScorePips({ score, title }: { score: number; title: string }) {
  const filled = Math.round((score / 10) * 5);
  return (
    <span className="inline-flex items-center gap-0.5" title={`${title}: ${score}/10`}>
      {[0, 1, 2, 3, 4].map((i) => (
        <span key={i} className={`w-1.5 h-1.5 rounded-full ${i < filled ? "bg-primary-400" : "bg-surface-700"}`} />
      ))}
    </span>
  );
}

function ModelCard({ m, daemon, active }: { m: CatalogModel; daemon: DaemonApi; active: boolean }) {
  const s = daemon.state;
  const pct = s.downloadProgress[m.id];
  const err = s.modelErrors[m.id];
  const verifying = m.is_verifying || s.verifying.includes(m.id);
  const extracting = s.extracting.includes(m.id);
  const installing = s.installing.includes(m.id);
  const busy = m.is_downloading || verifying || extracting || installing;

  return (
    <div className={`rounded-lg p-2.5 ${active ? "bg-primary-500/10 ring-1 ring-primary-500/30" : "bg-white/[0.05]"}`}>
      <div className="flex items-start gap-2.5">
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-1.5">
            <span className="text-[13px] font-medium text-surface-100 truncate">{m.name}</span>
            {m.is_recommended && (
              <span className="text-[9px] px-1 py-px rounded bg-primary-500/15 text-primary-300">recommended</span>
            )}
            {m.supportsMultipleLanguages && (
              <span className="text-[9px] px-1 py-px rounded bg-surface-700/60 text-surface-300">multi</span>
            )}
          </div>
          {m.description && <Caption className="line-clamp-2">{m.description}</Caption>}
          <div className="flex items-center gap-3 mt-1">
            <Caption>{m.size_mb >= 1024 ? `${(m.size_mb / 1024).toFixed(1)} GB` : `${m.size_mb} MB`}</Caption>
            <span className="flex items-center gap-1"><Caption>Acc</Caption><ScorePips score={m.accuracy_score} title="Accuracy" /></span>
            <span className="flex items-center gap-1"><Caption>Spd</Caption><ScorePips score={m.speed_score} title="Speed" /></span>
          </div>
          {err && <Caption className="text-red-400 mt-1">{err}</Caption>}
          {busy && (
            <div className="mt-1.5">
              {m.is_downloading && pct !== undefined ? (
                <div className="flex items-center gap-2">
                  <div className="flex-1 h-1.5 rounded-full bg-surface-700/50 overflow-hidden">
                    <div className="h-full bg-primary-500 transition-[width]" style={{ width: `${pct}%` }} />
                  </div>
                  <span className="text-[10px] text-surface-400 tabular-nums">{pct.toFixed(0)}%</span>
                </div>
              ) : (
                <div className="flex items-center gap-1.5">
                  <Spinner />
                  <Caption>
                    {verifying ? "Verifying…" : extracting ? "Extracting…" : installing ? "Installing…" : "Downloading…"}
                  </Caption>
                </div>
              )}
            </div>
          )}
        </div>
        <div className="flex flex-col items-end gap-1 shrink-0">
          {m.is_downloaded ? (
            <>
              {active ? (
                <span className="text-[10px] px-1.5 py-0.5 rounded bg-primary-500/15 text-primary-300">active</span>
              ) : (
                <Btn small disabled={!s.isDaemonConnected || busy} onClick={() => daemon.setActiveTranscriber(`local:${m.id}`)}>
                  {s.loadingBackend === `local:${m.id}` ? <Spinner /> : null}Use
                </Btn>
              )}
              <Btn
                small destructive
                disabled={busy}
                onClick={() => {
                  if (window.confirm(`Delete "${m.name}"? You'll need to re-download it to use it again.`)) {
                    daemon.deleteModel(m.id);
                  }
                }}
              >
                Delete
              </Btn>
            </>
          ) : m.is_downloading ? (
            <Btn small onClick={() => daemon.cancelModelDownload(m.id)}>Cancel</Btn>
          ) : (
            <Btn small disabled={!s.isDaemonConnected || busy} onClick={() => daemon.downloadModel(m.id)}>
              Download
            </Btn>
          )}
        </div>
      </div>
    </div>
  );
}
