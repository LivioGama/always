import { useEffect, useState } from "react";
import { useDaemonState } from "./hooks/useDaemonState";
import { listen, udsStatus, windowLabel } from "./lib/tauri";
import { OnboardingView } from "./components/OnboardingView";
import { SettingsWindow } from "./components/SettingsWindow";
import { StatusOverlay } from "./components/StatusOverlay";
import { CorrectionDialog } from "./components/CorrectionDialog";

type Route = "settings" | "onboarding" | "overlay" | "loading";

/// Top-level router. Which surface to show is decided by the Tauri window
/// label (the Rust window manager creates "settings" / "onboarding" /
/// "overlay" windows), falling back to hash for dev, then to first-run state.
export default function App() {
  const daemon = useDaemonState();
  const [route, setRoute] = useState<Route>("loading");
  const [onboardingDone, setOnboardingDone] = useState(
    () => localStorage.getItem("always_onboarding_done") === "true",
  );

  // Wire daemon events: Rust backend forwards raw JSON lines as "uds_event",
  // plus connection lifecycle events.
  useEffect(() => {
    const un1 = listen("uds_event", (payload) => {
      if (typeof payload === "string") daemon.processEvent(payload);
      else daemon.processEvent(JSON.stringify(payload));
    });
    const un2 = listen("uds_connected", () => {
      daemon.setConnected(true);
      daemon.getVoiceProfileStatus();
      daemon.listModels();
    });
    const un3 = listen("uds_disconnected", () => daemon.setConnected(false));

    // The one-shot `uds_connected` event can fire before the webview's
    // listeners attach — poll the backend until it confirms the pipe.
    let cancelled = false;
    const poll = () => {
      udsStatus()
        .then((ok) => {
          if (cancelled) return;
          if (ok) {
            daemon.setConnected(true);
            daemon.getVoiceProfileStatus();
            daemon.listModels();
          }
          setTimeout(poll, ok ? 5000 : 1500);
        })
        .catch(() => !cancelled && setTimeout(poll, 3000));
    };
    poll();

    return () => {
      cancelled = true;
      un1();
      un2();
      un3();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Resolve which view this window should show.
  useEffect(() => {
    (async () => {
      let label = "";
      try {
        label = await windowLabel();
      } catch {
        label = "";
      }
      const hash = window.location.hash.replace(/^#\/?/, "");
      const pick = label || hash || (onboardingDone ? "settings" : "onboarding");
      if (pick.includes("overlay")) setRoute("overlay");
      else if (pick.includes("onboarding")) setRoute("onboarding");
      else if (pick.includes("settings") || pick.includes("main")) {
        setRoute(onboardingDone || pick.includes("settings") ? "settings" : "onboarding");
      } else setRoute(onboardingDone ? "settings" : "onboarding");
    })();
  }, [onboardingDone]);

  if (route === "loading") {
    return <div className="min-h-screen bg-surface-950" />;
  }

  if (route === "overlay") {
    return (
      <StatusOverlay
        state={daemon.state}
        onTogglePause={daemon.togglePause}
        onOpenSettings={() => {}}
      />
    );
  }

  if (route === "onboarding") {
    return (
      <>
        <OnboardingView
          daemon={daemon}
          onComplete={() => {
            localStorage.setItem("always_onboarding_done", "true");
            setOnboardingDone(true);
            setRoute("settings");
          }}
          onOpenSettings={() => {
            localStorage.setItem("always_onboarding_done", "true");
            setOnboardingDone(true);
            setRoute("settings");
          }}
        />
        {daemon.state.correctionPending && (
          <CorrectionDialog
            pending={daemon.state.correctionPending}
            onApprove={() => daemon.approveCorrection(daemon.state.correctionPending!.id)}
            onReject={() => daemon.rejectCorrection(daemon.state.correctionPending!.id)}
          />
        )}
      </>
    );
  }

  return (
    <>
      <SettingsWindow daemon={daemon} />
      {daemon.state.correctionPending && (
        <CorrectionDialog
          pending={daemon.state.correctionPending}
          onApprove={() => daemon.approveCorrection(daemon.state.correctionPending!.id)}
          onReject={() => daemon.rejectCorrection(daemon.state.correctionPending!.id)}
        />
      )}
    </>
  );
}
