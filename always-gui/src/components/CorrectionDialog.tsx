import { useState } from "react";

export interface CorrectionPending {
  id: string;
  wrong: string;
  right: string;
}

/// "You misheard me" dialog — mirrors the daemon's CorrectionPending event:
/// it proposes `wrong` → `right`; the user can edit `right`, accept, or reject.
export function CorrectionDialog({
  pending,
  onApprove,
  onReject,
}: {
  pending: CorrectionPending;
  onApprove: () => void;
  onReject: () => void;
}) {
  const [intended, setIntended] = useState(pending.right);
  const value = intended.trim();

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm animate-fade-in">
      <div className="bg-surface-900 rounded-2xl border border-surface-700 shadow-2xl p-6 w-full max-w-md animate-slide-up">
        <div className="flex items-center gap-2 mb-4">
          <svg className="w-5 h-5 text-primary-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M11 5H6a2 2 0 00-2 2v11a2 2 0 002 2h11a2 2 0 002-2v-5m-1.414-9.414a2 2 0 112.828 2.828L11.828 15H9v-2.828l8.586-8.586z" />
          </svg>
          <h3 className="text-lg font-semibold text-white">Correct transcription</h3>
        </div>

        <div className="mb-4">
          <p className="text-xs text-surface-400 mb-1">
            Always heard <span className="text-red-400 font-medium line-through">{pending.wrong}</span> — it should be:
          </p>
          <input
            type="text"
            value={intended}
            onChange={(e) => setIntended(e.target.value)}
            placeholder="Type the correct word…"
            className="w-full bg-surface-800 border border-surface-600 rounded-lg px-3 py-2 text-white text-sm placeholder-surface-500 focus:outline-none focus:ring-2 focus:ring-primary-500"
            autoFocus
            onKeyDown={(e) => {
              if (e.key === "Enter" && value) onApprove();
              if (e.key === "Escape") onReject();
            }}
          />
        </div>

        <div className="flex gap-2">
          <button
            onClick={onReject}
            className="flex-1 px-3 py-2 text-sm font-medium text-surface-300 bg-surface-800 hover:bg-surface-700 rounded-lg transition-colors"
          >
            Reject
          </button>
          <button
            onClick={() => value && onApprove()}
            disabled={!value}
            className="flex-1 px-3 py-2 text-sm font-semibold text-white bg-primary-600 hover:bg-primary-500 rounded-lg transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
          >
            Accept
          </button>
        </div>
      </div>
    </div>
  );
}
