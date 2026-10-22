import { useEffect, useState } from "react";
import type { CatalogModel, DaemonApi } from "../hooks/useDaemonState";

// Same pick as the daemon's fallback model: recommended first, then speed + accuracy.
const best = (models: CatalogModel[]) =>
  [...models].sort(
    (a, b) =>
      Number(b.is_recommended) - Number(a.is_recommended) ||
      b.speed_score + b.accuracy_score - (a.speed_score + a.accuracy_score),
  )[0];

/// Groq's daily (free-tier) quota is used up: offer a one-click switch to an
/// on-device model, downloading the recommended one first when none is on disk.
/// Mirrors the macOS GroqQuotaPrompt (SPEC § Models, Groq daily quota).
export function GroqQuotaDialog({ daemon }: { daemon: DaemonApi }) {
  const s = daemon.state;
  const [downloadingId, setDownloadingId] = useState<string | null>(null);
  const local = best(s.models.filter((m) => m.is_downloaded && !m.is_downloading));
  const target = local ?? best(s.models);
  const pending = downloadingId ? s.models.find((m) => m.id === downloadingId) : undefined;
  const error = downloadingId ? s.modelErrors[downloadingId] : undefined;

  // Download finished → switch, as if the user had clicked "Use".
  useEffect(() => {
    if (!pending?.is_downloaded) return;
    daemon.setActiveTranscriber(`local:${pending.id}`);
    daemon.dismissGroqQuota();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pending?.is_downloaded]);

  const confirm = () => {
    if (!target) return;
    if (local) {
      daemon.setActiveTranscriber(`local:${local.id}`);
      daemon.dismissGroqQuota();
    } else {
      setDownloadingId(target.id);
      daemon.downloadModel(target.id);
    }
  };

  const progress = downloadingId ? s.downloadProgress[downloadingId] : undefined;
  const status = error
    ? `Download failed: ${error}`
    : downloadingId
      ? progress === undefined
        ? `Installing ${target?.name}…`
        : `Downloading ${target?.name}… ${Math.round(progress)}%`
      : null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm animate-fade-in">
      <div className="bg-surface-900 rounded-2xl border border-surface-700 shadow-2xl p-6 w-full max-w-md animate-slide-up">
        <h3 className="text-lg font-semibold text-white mb-3">Groq's free quota is used up for today</h3>
        <p className="text-sm text-surface-300 mb-4">
          Dictation through Groq will fail until the quota resets.{" "}
          {!target
            ? "Download an on-device model in Settings → Models to keep dictating."
            : local
              ? `Switch to ${local.name}? It runs on this computer, is free, and keeps live preview.`
              : `Download ${target.name} (${target.size_mb} MB) and switch to it? It runs on this computer, is free, and keeps live preview.`}{" "}
          You can switch back in Settings → Models.
        </p>
        {status && <p className={`text-xs mb-4 ${error ? "text-red-400" : "text-surface-400"}`}>{status}</p>}
        <div className="flex gap-2">
          <button
            onClick={daemon.dismissGroqQuota}
            className="flex-1 px-3 py-2 text-sm font-medium text-surface-300 bg-surface-800 hover:bg-surface-700 rounded-lg transition-colors"
          >
            Keep Groq
          </button>
          {target && (
            <button
              onClick={confirm}
              disabled={!!downloadingId && !error}
              className="flex-1 px-3 py-2 text-sm font-semibold text-white bg-primary-600 hover:bg-primary-500 rounded-lg transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {local ? `Switch to ${local.name}` : error ? "Retry download" : "Download & switch"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
