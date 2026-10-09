import { useEffect, useRef, useCallback, useState } from "react";

interface SendRef {
  current: ((data: string) => void) | null;
}

export function useUDSConnection(
  onEvent: (event: any) => void,
  connected: boolean,
) {
  const sendRef = useRef<SendRef>({ current: null });
  const retryTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [error, setError] = useState<string | null>(null);

  const connect = useCallback(() => {
    // In Tauri, the Rust backend manages the UDS connection.
    // sendRef.current is the callback that the daemon state hook uses
    // to push raw JSON into the state machine.
    sendRef.current = {
      current: (data: string) => {
        try {
          // Tauri 2.0 IPC invoke — tells the Rust backend to forward
          // this JSON line to the daemon over UDS.
          const tauri = (window as any).__TAURI__;
          if (tauri?.core?.send) {
            tauri.core.send("uds_send", { data });
          } else if (tauri?.core?.invoke) {
            tauri.core.invoke("uds_send", { data }).catch(() => {});
          }
        } catch (e) {
          console.error("UDS send failed:", e);
        }
      }
    };
  }, []);

  const send = useCallback((data: string) => {
    sendRef.current?.current?.(data);
  }, []);

  // Reconnect when `connected` becomes true again
  useEffect(() => {
    if (connected) {
      connect();
      setError(null);
      return;
    }
    sendRef.current = { current: null };
  }, [connected, connect]);

  // Cleanup
  useEffect(() => {
    return () => {
      if (retryTimer.current) clearTimeout(retryTimer.current);
    };
  }, []);

  return { send, setError, error };
}