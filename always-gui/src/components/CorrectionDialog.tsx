import { useState } from "react";
import { CorrectionPendingData } from "../types/uds";

interface CorrectionDialogProps {
  pending: CorrectionPendingData;
  onApprove: () => void;
  onReject: () => void;
}

export function CorrectionDialog({
  pending,
  onApprove,
  onReject,
}: CorrectionDialogProps) {
  const [intended, setIntended] = useState("");

  // Auto-fill the suspected correction
  const guessedCorrection = findGuessedCorrection(pending);

  if (!guessedCorrection) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm animate-fade-in">
      <div className="bg-surface-900 rounded-2xl border border-surface-700 shadow-2xl p-6 w-full max-w-md animate-slide-up">
        <div className="flex items-center gap-2 mb-4">
          <svg className="w-5 h-5 text-primary-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M11 5H6a2 2 0 00-2 2v11a2 2 0 002 2h11a2 2 0 002-2v-5m-1.414-9.414a2 2 0 112.828 2.828L11.828 15H9v-2.828l8.586-8.586z" />
          </svg>
          <h3 className="text-lg font-semibold text-white">Correct transcription</h3>
        </div>

        {/* Last transcript */}
        <div className="mb-4">
          <p className="text-xs text-surface-400 mb-1">Last transcript</p>
          <p className="text-sm text-surface-200 bg-surface-800 rounded-lg p-3">
            {pending.lastTranscript}
          </p>
        </div>

        {/* Correction input */}
        <div className="mb-4">
          <p className="text-xs text-surface-400 mb-1">
            Replace <span className="text-red-400 font-medium line-through">{pending.wrong}</span> with
          </p>
          <input
            type="text"
            value={intended || guessedCorrection}
            onChange={(e) => setIntended(e.target.value)}
            placeholder="Type correct spelling…"
            className="w-full bg-surface-800 border border-surface-600 rounded-lg px-3 py-2 text-white text-sm placeholder-surface-500 focus:outline-none focus:ring-2 focus:ring-primary-500"
            autoFocus
            onKeyDown={(e) => {
              if (e.key === "Enter" && intended.trim()) {
                onApprove();
              }
            }}
          />
        </div>

        {/* Quick actions */}
        <div className="flex gap-2">
          <button
            onClick={onReject}
            className="flex-1 px-3 py-2 text-sm font-medium text-surface-300 bg-surface-800 hover:bg-surface-700 rounded-lg transition-colors"
          >
            Reject
          </button>
          <button
            onClick={() => {
              if (guessedCorrection.trim()) {
                setIntended(guessedCorrection);
              }
            }}
            className="flex-1 px-3 py-2 text-sm font-medium text-primary-300 bg-primary-600/20 hover:bg-primary-600/30 rounded-lg transition-colors"
          >
            Use "{guessedCorrection}"
          </button>
          <button
            onClick={() => {
              if (intended.trim()) onApprove();
            }}
            disabled={!intended.trim()}
            className="flex-1 px-3 py-2 text-sm font-semibold text-white bg-primary-600 hover:bg-primary-500 rounded-lg transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
          >
            Accept
          </button>
        </div>
      </div>
    </div>
  );
}

function findGuessedCorrection(pending: CorrectionPendingData): string | null {
  // Simple heuristic: suggest the last word of the last transcript as
  // a starting point for correction, since users typically fix the
  // most recently pasted word.
  const words = pending.lastTranscript.trim().split(/\s+/);
  const lastWord = words[words.length - 1];
  if (lastWord && lastWord !== pending.wrong) {
    return lastWord;
  }
  return null;
}