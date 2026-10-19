import { useEffect } from "react";
import { ENROLLMENT_STEPS, type DaemonApi } from "../../hooks/useDaemonState";
import { Btn, Caption, Divider, LevelMeter, PanelTitle, Spinner, Toggle } from "../controls";

/// Settings → My Voice: speaker-recognition enrollment. Mirrors
/// MyVoicePanel.swift — per-step Record/Re-record buttons, live level +
/// voiced progress, enable toggle, delete voiceprint.
export function MyVoicePanel({ daemon }: { daemon: DaemonApi }) {
  const s = daemon.state;
  const enrolledSteps = s.voiceSteps;
  const allEnrolled = ENROLLMENT_STEPS.every((st) => enrolledSteps.includes(st.key));
  const recording = s.enrollment.recordingStep;

  useEffect(() => {
    if (s.isDaemonConnected) daemon.getVoiceProfileStatus();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [s.isDaemonConnected]);

  return (
    <div className="p-5 space-y-3.5">
      <div>
        <PanelTitle>My Voice</PanelTitle>
        <Caption>
          Teach Always your voice. Once enrolled and enabled, dictation responds only to you — voices from movies, music, videos, and other people are ignored.
        </Caption>
      </div>
      <div className="flex items-center gap-3 rounded-md bg-primary-500/10 px-3 py-2.5">
        <svg
          className={`w-5 h-5 shrink-0 ${allEnrolled && s.voiceEnabled ? "text-primary-400" : "text-surface-500"}`}
          viewBox="0 0 24 24" fill="none" stroke="currentColor"
        >
          <circle cx="12" cy="8" r="3.5" strokeWidth={2} />
          <path strokeWidth={2} strokeLinecap="round" d="M4 20c1.5-3.5 4.5-5 8-5s6.5 1.5 8 5M2 10c.8-2 1.8-3 3-3m17 3c-.8-2-1.8-3-3-3" />
        </svg>
        <div className="flex-1">
          <div className="text-[13px] font-medium text-surface-100">Only listen to my voice</div>
          <Caption>
            {allEnrolled && s.voiceEnabled
              ? "Active — Always ignores every voice except yours."
              : allEnrolled
                ? "Voiceprint ready — enable My Voice to ignore other voices."
                : "Voice gating is off. Record all three samples before you can enable My Voice."}
          </Caption>
        </div>
        <Toggle
          on={s.voiceEnabled}
          onChange={(v) => daemon.setVoiceProfileEnabled(v)}
          disabled={!s.isDaemonConnected || (!allEnrolled && !s.voiceEnabled)}
        />
      </div>

      {!s.isDaemonConnected && (
        <div className="rounded-md bg-orange-500/10 border border-orange-500/30 px-3 py-2 text-xs text-orange-400">
          Daemon not connected — enrollment and voice gating require the running daemon.
        </div>
      )}

      {s.enrollment.lastError && (
        <div className="rounded-md bg-red-500/10 border border-red-500/30 px-3 py-2 text-xs text-red-400">
          {s.enrollment.lastError}
        </div>
      )}

      <Divider />

      <div className="space-y-2">
        <Caption>
          Record three short samples — normal, quieter, louder. Speak for a few seconds each until the bar fills.
        </Caption>
        {ENROLLMENT_STEPS.map((step) => {
          const done = enrolledSteps.includes(step.key);
          const isRecording = recording === step.key;
          const preparing = s.enrollment.isPreparing && isRecording;
          return (
            <div
              key={step.key}
              className={`rounded-md p-2.5 ${isRecording ? "bg-primary-500/10 ring-1 ring-primary-500/30" : "bg-white/[0.08]"}`}
            >
              <div className="flex items-center gap-2.5">
                <span className={`w-4 h-4 flex items-center justify-center ${done ? "text-green-500" : "text-surface-500"}`}>
                  {done ? (
                    <svg className="w-4 h-4" viewBox="0 0 24 24" fill="currentColor">
                      <path d="M12 2a10 10 0 100 20 10 10 0 000-20zm-1.5 14.5l-4-4 1.4-1.4 2.6 2.6 5.6-5.6 1.4 1.4-7 7z" />
                    </svg>
                  ) : (
                    <svg className="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor">
                      <circle cx="12" cy="12" r="9" strokeWidth={2} />
                    </svg>
                  )}
                </span>
                <div className="flex-1 min-w-0">
                  <div className="text-[13px] font-medium text-surface-100">{step.title}</div>
                  <Caption>{step.prompt}</Caption>
                </div>
                {isRecording ? (
                  <Btn onClick={daemon.cancelVoiceEnrollment}>Cancel</Btn>
                ) : (
                  <Btn
                    disabled={!s.isDaemonConnected || recording !== null || s.enrollment.isPreparing}
                    onClick={() => daemon.startVoiceEnrollment(step.key)}
                  >
                    {done ? "Re-record" : "Record"}
                  </Btn>
                )}
              </div>

              {isRecording && (
                <div className="mt-2 flex items-center gap-2 pl-6">
                  {preparing ? (
                    <>
                      <Spinner />
                      <Caption>Starting microphone…</Caption>
                    </>
                  ) : (
                    <>
                      <LevelMeter level={s.enrollment.level} />
                      <span className="text-[10px] text-surface-400 tabular-nums w-16 text-right">
                        {(s.enrollment.voicedMs / 1000).toFixed(1)}s
                        {s.enrollment.targetMs > 0 ? ` / ${(s.enrollment.targetMs / 1000).toFixed(0)}s` : ""}
                      </span>
                    </>
                  )}
                </div>
              )}
            </div>
          );
        })}
      </div>

      {(allEnrolled || s.enrolled) && (
        <>
          <Divider />
          <div className="flex items-center gap-2.5">
            <Caption className="flex-1">
              Voiceprint stored on this device only. Deleting it turns voice gating off.
            </Caption>
            <Btn
              destructive
              disabled={!s.isDaemonConnected}
              onClick={() => {
                if (window.confirm("Delete your voiceprint? Voice gating will be disabled.")) {
                  daemon.deleteVoiceProfile();
                }
              }}
            >
              Delete Voiceprint
            </Btn>
          </div>
        </>
      )}
    </div>
  );
}
