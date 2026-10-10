import { useState, useEffect, useCallback, useRef } from "react";
import { useDaemonState } from "./hooks/useDaemonState";
import { useUDSConnection } from "./hooks/useUDSConnection";
import { OnboardingView } from "./components/OnboardingView";
import { StatusOverlay } from "./components/StatusOverlay";
import { SettingsWindow } from "./components/SettingsWindow";
import { CorrectionDialog } from "./components/CorrectionDialog";
import type { Event } from "@tauri-apps/api/event";

type AppView = "onboarding" | "main" | "settings";

export default function App() {
  const [view, setView] = useState<AppView>("onboarding");
  const [onboardingComplete, setOnboardingComplete] = useState(false);
  const [hasCompletedOnboarding, setHasCompletedOnboarding] = useState(false);
  const [shortcutListening, setShortcutListening] = useState(false);
  const [hasVoiceProfile, setHasVoiceProfile] = useState(false);

  const daemon = useDaemonState();

  // Track connection state
  const [connected, setConnected] = useState(false);
  const conn = useUDSConnection(
    useCallback((raw: Event<any> | any) => {
      // Parse the event payload
      let data: any;
      if (typeof raw === "object" && "payload" in raw) {
        data = (raw as Event<any>).payload;
      } else {
        data = raw;
      }
      daemon.processEvent(data);
    }, [daemon.processEvent]),
    connected,
  );

  // Connection management — the Tauri backend manages the actual UDS
  useEffect(() => {
    // The Rust Tauri backend opens the UDS connection.
    // We set connected once the initial Hello event arrives.
    const tauri = (window as any).__TAURI__;
    if (!tauri?.core?.listen) return;

    // Listen for daemon events
    const unlistenEvent = tauri.core.listen("uds_event", (event: Event<any>) => {
      daemon.processEvent(event.payload);
      if (typeof event.payload === "string" && event.payload.includes("Hello")) {
        setConnected(true);
      }
    });

    // Listen for connection status
    const unlistenConn = tauri.core.listen("uds_connected", () => {
      setConnected(true);
    });

    const unlistenConnLost = tauri.core.listen("uds_disconnected", () => {
      setConnected(false);
      // Reconnect after a short delay
      setTimeout(() => {
        tauri.core.invoke("uds_connect");
      }, 2000);
    });

    // Initial connection
    tauri.core.invoke("uds_connect").catch(() => {});

    // Restore onboarding state from localStorage
    const saved = localStorage.getItem("always_onboarding_done");
    if (saved === "true") {
      setHasCompletedOnboarding(true);
      setView("main");
    }

    return () => {
      unlistenEvent();
      unlistenConn();
      unlistenConnLost();
    };
  }, []);

  // Update voice profile status in local tracking
  useEffect(() => {
    setHasVoiceProfile(daemon.state.enrolled);
  }, [daemon.state.enrolled]);

  // Handle onboarding completion
  const handleOnboardingComplete = useCallback(() => {
    localStorage.setItem("always_onboarding_done", "true");
    setHasCompletedOnboarding(true);
    setView("main");
  }, []);

  // Open settings window (Tauri window)
  const handleOpenSettings = useCallback(() => {
    const tauri = (window as any).__TAURI__;
    if (tauri?.window?.Window) {
      // Open a dedicated settings window
      try {
        tauri.window.AppWindow.getAll().forEach((win: any) => {
          if (win.label === "settings") {
            win.setVisible(true);
            win.setFocus();
            return;
          }
        });
        // If settings window doesn't exist, create it
        const settingsWin = new tauri.window.Window("settings");
        settingsWin.loadUrl("/#/settings");
      } catch {
        // Fall back to in-app settings
        setView("settings");
      }
    } else {
      setView("settings");
    }
  }, []);

  // Open onboarding window
  const handleOpenOnboarding = useCallback(() => {
    const tauri = (window as any).__TAURI__;
    if (tauri?.window?.Window) {
      try {
        const onboardingWin = new tauri.window.Window("onboarding");
        onboardingWin.loadUrl("/#/onboarding");
      } catch {
        setView("onboarding");
      }
    } else {
      setView("onboarding");
    }
  }, []);

  // Main app view
  if (view === "onboarding" && !hasCompletedOnboarding) {
    return (
      <OnboardingView
        onComplete={handleOnboardingComplete}
        onStartSettings={() => {
          setView("main");
          setTimeout(handleOpenSettings, 100);
        }}
        onStartEnrollment={() => {}}
        hasVoiceProfile={hasVoiceProfile}
        shortcutListening={shortcutListening}
      />
    );
  }

  // Settings view (in-app fallback)
  if (view === "settings") {
    return (
      <SettingsWindow
        state={daemon.state}
        onTogglePause={daemon.togglePause}
        setAutoEnter={daemon.setAutoEnter}
        setConsumeMode={daemon.setConsumeMode}
        applyRuntimePreferences={daemon.applyRuntimePreferences}
        setActiveTranscriber={daemon.setActiveTranscriber}
        setLanguage={daemon.setLanguage}
        startVoiceEnrollment={daemon.startVoiceEnrollment}
        cancelVoiceEnrollment={daemon.cancelVoiceEnrollment}
        deleteVoiceProfile={daemon.deleteVoiceProfile}
        setVoiceProfileEnabled={daemon.setVoiceProfileEnabled}
        downloadModel={daemon.downloadModel}
        deleteModel={daemon.deleteModel}
        onClose={() => setView("main")}
      />
    );
  }

  // Main dashboard
  return (
    <div className="min-h-screen bg-surface-950">
      {/* Top bar */}
      <header className="border-b border-surface-800/50 bg-surface-950/80 backdrop-blur-sm">
        <div className="max-w-6xl mx-auto px-6 py-4 flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div className="w-8 h-8 rounded-lg bg-primary-600 flex items-center justify-center">
              <svg className="w-4 h-4 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19 11a7 7 0 01-7 7m0 0a7 7 0 01-7-7m7 7v4m0 0H8m4 0h4m-4-8a3 3 0 01-3-3V5a3 3 0 116 0v6a3 3 0 01-3 3z" />
              </svg>
            </div>
            <div>
              <h1 className="text-base font-semibold text-white">Always</h1>
              <p className="text-xs text-surface-500">
                {connected ? "Connected" : "Connecting…"}
                {daemon.state.backend && ` · ${daemon.state.backend}`}
              </p>
            </div>
          </div>

          <div className="flex items-center gap-2">
            <button
              onClick={handleOpenOnboarding}
              className="px-3 py-1.5 text-xs text-surface-400 hover:text-white bg-surface-800 hover:bg-surface-700 rounded-lg transition-colors"
            >
              Onboarding
            </button>
            <button
              onClick={handleOpenSettings}
              className="px-3 py-1.5 text-xs text-surface-400 hover:text-white bg-surface-800 hover:bg-surface-700 rounded-lg transition-colors"
            >
              Settings
            </button>
          </div>
        </div>
      </header>

      {/* Status Overlay */}
      <main className="max-w-6xl mx-auto px-6 py-6">
        <StatusOverlay
          state={daemon.state}
          onTogglePause={daemon.togglePause}
          onOpenSettings={handleOpenSettings}
        />

        {/* Quick Actions */}
        <div className="mt-8 grid grid-cols-1 md:grid-cols-3 gap-4">
          {/* Active Transcript */}
          <div className="bg-surface-900 rounded-xl border border-surface-800/50 p-4">
            <h3 className="text-xs font-medium text-surface-400 mb-2 uppercase tracking-wider">
              Current Transcript
            </h3>
            <p className="text-sm text-white min-h-[60px]">
              {daemon.state.currentTranscript || (
                <span className="text-surface-600 italic">
                  {daemon.state.interimText || "Waiting for speech…"}
                </span>
              )}
            </p>
            {daemon.state.interimText && (
              <p className="text-xs text-surface-500 italic mt-1">{daemon.state.interimText}</p>
            )}
          </div>

          {/* State */}
          <div className="bg-surface-900 rounded-xl border border-surface-800/50 p-4">
            <h3 className="text-xs font-medium text-surface-400 mb-2 uppercase tracking-wider">
              State
            </h3>
            <div className="space-y-1.5">
              <div className="flex justify-between text-sm">
                <span className="text-surface-500">Mode</span>
                <span className="text-surface-200 capitalize">{daemon.state.state}</span>
              </div>
              <div className="flex justify-between text-sm">
                <span className="text-surface-500">Backend</span>
                <span className="text-surface-200">{daemon.state.backend}</span>
              </div>
              <div className="flex justify-between text-sm">
                <span className="text-surface-500">Auto-Enter</span>
                <span className={`text-sm ${daemon.state.isAutoEnter ? "text-green-400" : "text-surface-500"}`}>
                  {daemon.state.isAutoEnter ? "On" : "Off"}
                </span>
              </div>
              <div className="flex justify-between text-sm">
                <span className="text-surface-500">My Voice</span>
                <span className={`text-sm ${daemon.state.voiceEnabled && daemon.state.enrolled ? "text-primary-400" : "text-surface-500"}`}>
                  {daemon.state.voiceEnabled && daemon.state.enrolled ? "Active" : "Inactive"}
                </span>
              </div>
            </div>
          </div>

          {/* Recent */}
          <div className="bg-surface-900 rounded-xl border border-surface-800/50 p-4">
            <h3 className="text-xs font-medium text-surface-400 mb-2 uppercase tracking-wider">
              Recent
            </h3>
            {daemon.state.recentTranscripts.length === 0 ? (
              <p className="text-sm text-surface-600 italic">No transcripts yet</p>
            ) : (
              <div className="space-y-1">
                {daemon.state.recentTranscripts.slice(0, 3).map((entry, i) => (
                  <p key={i} className="text-sm text-surface-300 truncate">
                    {entry.text}
                  </p>
                ))}
              </div>
            )}
          </div>
        </div>
      </main>

      {/* Correction Dialog */}
      {daemon.state.correctionPending && (
        <CorrectionDialog
          pending={daemon.state.correctionPending}
          onApprove={() => {
            daemon.approveCorrection(daemon.state.correctionPending!.id);
          }}
          onReject={() => {
            daemon.rejectCorrection(daemon.state.correctionPending!.id);
          }}
        />
      )}
    </div>
  );
}