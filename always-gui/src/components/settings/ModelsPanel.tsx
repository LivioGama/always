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

type SortKey = "default" | "best" | "accuracy" | "speed";
type LanguageFilter = "all" | "english" | "multilingual";

function ModelsSection({ daemon, config, onConfigChange }: {
  daemon: DaemonApi;
  config: Config;
  onConfigChange: (patch: Partial<Config>) => void;
}) {
  const s = daemon.state;
  const [langFilter, setLangFilter] = useState<LanguageFilter>("all");
  const [onlyTranslation, setOnlyTranslation] = useState(false);
  const [onlyStreaming, setOnlyStreaming] = useState(false);
  const [onlyFast, setOnlyFast] = useState(false);
  const [sort, setSort] = useState<SortKey>("default");
  const [showAll, setShowAll] = useState(false);

  const filtered = useMemo(() => {
    let list = [...s.models];
    if (langFilter === "english") list = list.filter((m) => !m.supportsMultipleLanguages);
    if (langFilter === "multilingual") list = list.filter((m) => m.supportsMultipleLanguages);
    if (onlyTranslation) list = list.filter((m) => m.supports_translation);
    if (onlyStreaming) list = list.filter((m) => m.supports_streaming);
    if (onlyFast) list = list.filter((m) => m.speed_score >= 0.75);
    switch (sort) {
      case "best": list.sort((a, b) => (b.accuracy_score + b.speed_score) - (a.accuracy_score + a.speed_score)); break;
      case "accuracy": list.sort((a, b) => b.accuracy_score - a.accuracy_score); break;
      case "speed": list.sort((a, b) => b.speed_score - a.speed_score); break;
      default: break;
    }
    return list;
  }, [s.models, langFilter, onlyTranslation, onlyStreaming, onlyFast, sort]);

  const filtersActive = langFilter !== "all" || onlyTranslation || onlyFast || onlyStreaming || sort !== "default";
  const downloaded = filtered.filter((m) => m.is_downloaded).sort((a, b) =>
    Number(b.is_recommended) - Number(a.is_recommended) || a.name.localeCompare(b.name));
  const available = filtered.filter((m) => !m.is_downloaded).sort((a, b) =>
    Number(b.is_recommended) - Number(a.is_recommended) || a.size_mb - b.size_mb);
  const visibleAvailable = showAll ? available : available.slice(0, 8);
  const activeId = s.backend.startsWith("local:") ? s.backend.slice(6) : null;

  return (
    <div className="space-y-3">
      <div>
        <PanelTitle>Transcription Models</PanelTitle>
        <Caption className="mt-0.5">Select a transcription model or download additional models. Different models offer varying levels of accuracy and speed.</Caption>
      </div>

      <div className="flex items-center gap-2.5 rounded-md bg-white/[0.05] px-2.5 py-2">
        <span className="text-xs text-surface-400">Active:</span>
        <span className="text-xs font-semibold text-surface-100">
          {activeId ? s.models.find((m) => m.id === activeId)?.name ?? activeId : s.backend === "groq" ? "Groq Whisper (Remote)" : s.backend === "none" ? "No model selected" : s.backend}
        </span>
        <div className="flex-1" />
        {s.backend !== "groq" && (
          s.loadingBackend === "groq" ? <LoadingLabel /> : (
            <Btn small disabled={!s.isDaemonConnected || s.loadingBackend !== null} onClick={() => daemon.setActiveTranscriber("groq")}>Use Groq (Remote)</Btn>
          )
        )}
      </div>

      {s.backend === "none" && <Caption>Choose a downloaded local model or add a Groq API key to enable cloud transcription.</Caption>}
      {s.modelErrors.groq && <Caption className="!text-orange-400">{s.modelErrors.groq}</Caption>}

      {(() => {
        const activeModel = s.models.find((m) => m.id === activeId);
        if (!activeModel?.supports_language_selection || activeModel.supported_languages.length < 2) return null;
        return (
          <div className="flex items-center gap-2">
            <Caption>Language:</Caption>
            <select
              value={config.lang && config.lang !== "auto" ? config.lang : activeModel.supported_languages[0]}
              onChange={(e) => {
                onConfigChange({ lang: e.target.value });
                daemon.setLanguage(e.target.value);
              }}
              className="bg-surface-800 border border-surface-700 rounded px-2 py-1 text-xs text-surface-200"
            >
              {activeModel.supported_languages.map((l) => <option key={l} value={l}>{l.toUpperCase()}</option>)}
            </select>
          </div>
        );
      })()}
      <div className="flex items-center gap-1 overflow-x-auto">
        <Segmented
          value={langFilter}
          onChange={setLangFilter}
          options={[{ label: "All", value: "all" }, { label: "English", value: "english" }, { label: "Multilingual", value: "multilingual" }]}
        />
        <FilterChip on={onlyTranslation} onClick={() => setOnlyTranslation((v) => !v)}>Translation</FilterChip>
        <FilterChip on={onlyFast} onClick={() => setOnlyFast((v) => !v)}>Fast</FilterChip>
        <FilterChip on={onlyStreaming} onClick={() => setOnlyStreaming((v) => !v)}>Streaming</FilterChip>
        <select
          value={sort}
          onChange={(e) => setSort(e.target.value as SortKey)}
          className="ml-auto shrink-0 bg-surface-800 border border-surface-700 rounded px-2 py-1 text-xs text-surface-200"
        >
          <option value="default">Sort</option>
          <option value="best">↓ Best</option>
          <option value="accuracy">↓ Accuracy</option>
          <option value="speed">↓ Speed</option>
        </select>
      </div>
      <div className="flex items-center gap-2">
        {filtersActive && (
          <button
            type="button"
            onClick={() => { setLangFilter("all"); setOnlyTranslation(false); setOnlyFast(false); setOnlyStreaming(false); setSort("default"); }}
            className="text-xs text-surface-400 hover:text-surface-200"
          >Clear</button>
        )}
        {onlyTranslation && (
          <Caption className="text-orange-400/80">Translation support is limited on this backend.</Caption>
        )}
      </div>

      {/* Downloaded */}
      {downloaded.length > 0 && (
        <div className="space-y-1.5">
          <Caption>Downloaded Models</Caption>
          {downloaded.map((m) => (
            <ModelCard key={m.id} m={m} daemon={daemon} active={activeId === m.id} />
          ))}
        </div>
      )}

      {/* Available */}
      <div className="space-y-1.5">
          <Caption>Available to Download</Caption>
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
      className={`px-2 py-1 rounded-md text-xs border transition-colors ${
        on ? "bg-primary-600 border-primary-500 text-white" : "bg-surface-800 border-surface-700 text-surface-300 hover:bg-surface-700"
      }`}
    >
      {children}
    </button>
  );
}

function ScoreBar({ score, title }: { score: number; title: string }) {
  return (
    <span className="flex items-center gap-1.5" title={`${title}: ${Math.round(score * 100)}%`}>
      <span className="w-14 text-right text-[10px] text-surface-400">{title}</span>
      <span className="w-[70px] h-1.5 overflow-hidden rounded-full bg-surface-700">
        <span className="block h-full rounded-full bg-primary-500" style={{ width: `${Math.max(0, Math.min(1, score)) * 100}%` }} />
      </span>
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
    <div className={`rounded-lg border p-2.5 ${active ? "border-primary-500/60" : "border-white/[0.12]"}`}>
      <div className="flex items-start gap-2.5">
        <div className="flex-1 min-w-0">
          <div className="flex items-start gap-3">
            <div className="flex-1 min-w-0">
              <div className="flex items-center gap-1.5">
                <span className="text-[13px] font-medium text-surface-100 truncate">{m.name}</span>
                {active && <ModelBadge>Active</ModelBadge>}
                {!active && m.is_recommended && <ModelBadge outline>Recommended</ModelBadge>}
              </div>
              {m.description && <Caption className="line-clamp-2">{m.description}</Caption>}
            </div>
            {(m.accuracy_score !== 0 || m.speed_score !== 0) && (
              <div className="shrink-0 flex flex-col items-end gap-0.5">
                <ScoreBar score={m.accuracy_score} title="accuracy" />
                <ScoreBar score={m.speed_score} title="speed" />
              </div>
            )}
          </div>
          <div className="mt-1.5 flex items-center gap-2.5 flex-wrap">
            <ModelMetadata icon="globe">{languageLabel(m)}</ModelMetadata>
            {m.supports_translation && <ModelMetadata icon="translate">Translate to English</ModelMetadata>}
            {m.supports_streaming && <ModelMetadata icon="stream">Live streaming</ModelMetadata>}
            <ModelMetadata icon="drive">{sizeLabel(m.size_mb)}</ModelMetadata>
            <div className="flex-1" />
            {m.is_downloaded && (active ? <span className="text-[10px] text-surface-400">Active</span> : (
              s.loadingBackend === `local:${m.id}` ? <LoadingLabel /> : (
                <Btn small disabled={!s.isDaemonConnected || busy || s.loadingBackend !== null} onClick={() => daemon.setActiveTranscriber(`local:${m.id}`)}>Use</Btn>
              )
            ))}
            {m.is_downloaded ? (
              <button
                type="button"
                className="inline-flex items-center justify-center w-7 h-7 rounded-md bg-red-500/10 text-red-400 hover:bg-red-500/20 disabled:opacity-40 disabled:cursor-not-allowed"
                aria-label={`Delete ${m.name}`}
                disabled={active || busy || s.loadingBackend !== null}
                title={active ? `Switch to another model before deleting ${m.name}` : `Delete ${m.name}`}
                onClick={() => {
                if (window.confirm(`Delete "${m.name}"? You'll need to re-download it to use it again.`)) daemon.deleteModel(m.id);
                }}
              >
                <TrashIcon />
              </button>
            ) : null}
            {!m.is_downloaded ? (
              m.is_downloading ? (
                <Btn small onClick={() => daemon.cancelModelDownload(m.id)}>Cancel</Btn>
              ) : installing ? <Spinner /> : (
                <Btn small disabled={!s.isDaemonConnected || busy} onClick={() => daemon.downloadModel(m.id)}>
                  <DownloadIcon />{sizeLabel(m.size_mb)}
                </Btn>
              )
            ) : null}
          </div>
          {err && <Caption className="text-red-400 mt-1">{err}</Caption>}
          {busy && (
            <div className="mt-1.5">
              {m.is_downloading && pct !== undefined ? (
                <div className="flex items-center gap-2">
                  <div className="flex-1 h-1.5 rounded-full bg-surface-700/50 overflow-hidden">
                    <div className="h-full bg-primary-500 transition-[width]" style={{ width: `${pct}%` }} />
                  </div>
                  <div className="flex gap-2 text-[10px] text-surface-400 tabular-nums">
                    <span>{pct.toFixed(0)}%</span>
                    {s.downloadBytes[m.id]?.total ? <span>{byteString(s.downloadBytes[m.id].downloaded)} / {byteString(s.downloadBytes[m.id].total)}</span> : null}
                  </div>
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
      </div>
    </div>
  );
}

function ModelBadge({ children, outline = false }: { children: React.ReactNode; outline?: boolean }) {
  return <span className={`text-[10px] px-1.5 py-0.5 rounded ${outline ? "border border-surface-600 text-surface-300" : "bg-primary-500/20 text-primary-300"}`}>{children}</span>;
}

function ModelMetadata({ icon, children }: { icon: "globe" | "translate" | "stream" | "drive"; children: React.ReactNode }) {
  return <span className="inline-flex items-center gap-1 text-[10px] text-surface-400"><ModelIcon kind={icon} />{children}</span>;
}

function ModelIcon({ kind }: { kind: "globe" | "translate" | "stream" | "drive" }) {
  const common = { className: "w-3 h-3 shrink-0", viewBox: "0 0 24 24", fill: "none", stroke: "currentColor", strokeWidth: 1.8, strokeLinecap: "round" as const, strokeLinejoin: "round" as const };
  if (kind === "globe") return <svg {...common}><circle cx="12" cy="12" r="9" /><path d="M3 12h18M12 3a15 15 0 010 18M12 3a15 15 0 000 18" /></svg>;
  if (kind === "translate") return <svg {...common}><path d="M4 5h10M9 3v2m5 0c-.7 4-3 7-7 9m0-6c1.5 2 3.5 3.5 6 4.5M14 20l4-10 4 10m-6.5-3h5" /></svg>;
  if (kind === "stream") return <svg {...common}><path d="M3 9a13 13 0 000 6m4-9a8 8 0 000 12m4-15a3 3 0 000 18m4-12a3 3 0 000 6m5-4a1 1 0 000 2" /></svg>;
  return <svg {...common}><rect x="3" y="5" width="18" height="14" rx="2" /><path d="M7 9h10M7 13h6" /></svg>;
}

function LoadingLabel() {
  return <span className="inline-flex items-center gap-1 text-xs text-surface-400"><Spinner />Loading…</span>;
}

function DownloadIcon() {
  return <svg className="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor"><circle cx="12" cy="12" r="9" strokeWidth={1.8} /><path d="M12 7v10m-4-4 4 4 4-4" strokeWidth={1.8} strokeLinecap="round" strokeLinejoin="round" /></svg>;
}

function TrashIcon() {
  return <svg aria-hidden="true" className="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.8} strokeLinecap="round" strokeLinejoin="round"><path d="M3 6h18M8 6V4h8v2m3 0-1 14H6L5 6m4 4v6m6-6v6" /></svg>;
}

function languageLabel(m: CatalogModel): string {
  if (m.supports_language_selection && m.supported_languages.length > 1) return "Multi-language";
  if (m.supported_languages.length === 1) return m.supported_languages[0].toUpperCase() === "EN" ? "English Only" : `${m.supported_languages[0].toUpperCase()} Only`;
  return "Multi-language";
}

function sizeLabel(mb: number): string {
  return mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${mb} MB`;
}

function byteString(bytes: number): string {
  const units = ["bytes", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) { value /= 1000; unit += 1; }
  return `${unit === 0 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}
