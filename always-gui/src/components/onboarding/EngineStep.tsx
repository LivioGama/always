import { useEffect, useState, type ReactNode } from "react";
import type { CatalogModel, DaemonApi } from "../../hooks/useDaemonState";
import { configSet, openUrl, restartDaemon } from "../../lib/tauri";
import { Btn, Caption, Spinner } from "../controls";
import { Icon } from "./icons";

// Recommended first, then speed + accuracy (same ranking as GroqQuotaDialog).
// shortcut: CatalogModel has no is_custom field, so custom models are not excluded.
const best = (models: CatalogModel[]) =>
  [...models].sort(
    (a, b) =>
      Number(b.is_recommended) - Number(a.is_recommended) ||
      b.speed_score + b.accuracy_score - (a.speed_score + a.accuracy_score),
  )[0];

function EngineCard({ title, detail, selected, children }: { title: string; detail: string; selected: boolean; children: ReactNode }) {
  return (
    <div className={`flex flex-col gap-1.5 rounded-xl border px-4 py-2.5 bg-white/[0.06] ${selected ? "border-green-500/30" : "border-white/[0.12]"}`}>
      <div className="flex items-center">
        <span className="flex-1 text-[15px] font-medium text-white">{title}</span>
        {selected && <Icon name="checkCircle" className="w-[18px] h-[18px] text-green-500" />}
      </div>
      <Caption>{detail}</Caption>
      {children}
    </div>
  );
}

// `onPick` hands a choice that can't apply yet to OnboardingView, which
// keeps applying it after the user has left this step (null = cancel it).
export function EngineStep({ daemon, onPick }: { daemon: DaemonApi; onPick: (backend: string | null) => void }) {
  const s = daemon.state;
  const [key, setKey] = useState("");
  const [saving, setSaving] = useState(false);
  const [keyError, setKeyError] = useState<string | null>(null);
  // Saved: the key stays in the field, masked and read-only, until "Change".
  const [keySaved, setKeySaved] = useState(false);

  useEffect(() => {
    daemon.listModels();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const ready = best(s.models.filter((m) => m.is_downloaded && !m.is_downloading));
  const target = best(s.models);

  const saveKey = async () => {
    const k = key.trim();
    if (!k || saving || keySaved) return;
    setSaving(true);
    setKeyError(null);
    try {
      await configSet("groq_api_key", k);
      // The daemon reads the key only at start.
      await restartDaemon();
      onPick("groq");
      setKeySaved(true);
    } catch (e) {
      setKeyError(`Could not save the key: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setSaving(false);
    }
  };

  const progress = target ? s.downloadProgress[target.id] : undefined;
  const err = target ? s.modelErrors[target.id] : undefined;

  return (
    <div className="flex flex-col gap-2.5 px-5 overflow-y-auto">
      <EngineCard title="Groq (cloud)" detail="Best accuracy and speed. Needs a free API key." selected={s.backend === "groq"}>
        <div className="flex items-center gap-2">
          <input
            type="password"
            value={key}
            placeholder="gsk_…"
            onChange={(e) => setKey(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && saveKey()}
            disabled={keySaved || saving}
            className="flex-1 min-w-0 rounded-md bg-surface-800 border border-surface-700 px-2.5 py-1 text-xs text-white placeholder:text-surface-500 outline-none focus:border-primary-500 disabled:opacity-60"
          />
          {keySaved ? (
            <Btn onClick={() => { setKeySaved(false); setKey(""); }}>Change</Btn>
          ) : (
            <Btn prominent disabled={saving || !key.trim()} onClick={saveKey}>Save &amp; Use</Btn>
          )}
        </div>
        <div className="flex items-center gap-2">
          <button type="button" onClick={() => openUrl("https://console.groq.com/keys")} className="text-xs text-primary-400 hover:underline">
            Get a free key
          </button>
          {saving && <Spinner />}
        </div>
        {keyError && <p className="text-xs text-red-400">{keyError}</p>}
        <p className="text-[11px] text-surface-500">Free tier: about 2,000 requests a day. Live transcript uses more of it.</p>
      </EngineCard>

      {ready ? (
        <EngineCard title="Local model" detail={`Private and offline. ${ready.name} is ready.`} selected={s.backend.startsWith("local:")}>
          <div><Btn prominent onClick={() => { onPick(null); daemon.setActiveTranscriber(`local:${ready.id}`); }}>Use {ready.name}</Btn></div>
        </EngineCard>
      ) : target ? (
        <EngineCard title="Local model" detail={`Private and offline. One-time download of ${target.name} (${target.size_mb} MB).`} selected={false}>
          {progress !== undefined ? (
            <div className="flex items-center gap-2">
              <div className="flex-1 h-1.5 rounded-full bg-white/10 overflow-hidden">
                <div className="h-full bg-primary-500 transition-all" style={{ width: `${Math.min(100, progress)}%` }} />
              </div>
              <Caption>{Math.round(progress)}%</Caption>
            </div>
          ) : s.installing.includes(target.id) ? (
            <div className="flex items-center gap-2"><Spinner /><Caption>Installing…</Caption></div>
          ) : (
            <div>
              <Btn
                prominent
                onClick={() => {
                  daemon.downloadModel(target.id);
                  onPick(`local:${target.id}`);
                }}
              >
                {err ? "Retry" : "Download & Use"}
              </Btn>
            </div>
          )}
          {progress !== undefined && <p className="text-[11px] text-surface-500">Keeps downloading while you continue.</p>}
          {err && <p className="text-xs text-red-400">{err}</p>}
        </EngineCard>
      ) : (
        <EngineCard title="Local model" detail="Private and offline. Loading the model list…" selected={false}>
          <div><Spinner /></div>
        </EngineCard>
      )}

      <Caption className="pt-0.5">Set up later in Settings → Models</Caption>
    </div>
  );
}
