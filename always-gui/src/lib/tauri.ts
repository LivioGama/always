// Thin wrappers over the Tauri IPC surface exposed by always-tauri.
// Mirrors the Swift app's CLIService + file helpers.

function tauri(): any {
  return (window as any).__TAURI__;
}

export async function invoke<T = any>(cmd: string, args?: Record<string, any>): Promise<T> {
  const t = tauri();
  if (!t?.core?.invoke) throw new Error("tauri unavailable");
  return t.core.invoke(cmd, args) as Promise<T>;
}

export function listen(event: string, handler: (payload: any) => void): () => void {
  const t = tauri();
  if (!t?.event?.listen) return () => {};
  let un: (() => void) | undefined;
  t.event.listen(event, (e: any) => handler(e.payload)).then((f: () => void) => (un = f));
  return () => un?.();
}

// ── Daemon command channel ──────────────────────────────────────────────────

export function sendDaemonCommand(cmd: object): Promise<void> {
  return invoke("send_uds_command", { line: JSON.stringify(cmd) });
}

// ── CLI bridge (always <args>) ─────────────────────────────────────────────

export function cli(args: string[]): Promise<string> {
  return invoke<string>("cli", { args });
}

export const configShow = () => cli(["config", "show"]);
export const configSet = (key: string, value: string) => cli(["config", "set", key, value]);
export const configDelete = (key: string) => cli(["config", "unset", key]).catch(() => cli(["config", "delete", key]));
export const restartDaemon = () => cli(["restart"]);

// ── App files (~/.always) ───────────────────────────────────────────────────

export const readAppFile = (name: string) => invoke<string>("read_app_file", { name });
export const writeAppFile = (name: string, content: string) =>
  invoke("write_app_file", { name, content });
export const appFilePath = (name: string) => invoke<string>("app_file_path", { name });

// ── History ─────────────────────────────────────────────────────────────────

export interface HistoryEntry {
  text: string;
  timestamp_ms: number;
}
export const readHistory = () => invoke<HistoryEntry[]>("read_history");
export const clearHistory = () => invoke("clear_history");
export const historyFilePath = () => invoke<string>("history_file_path");

// ── Openers / misc ──────────────────────────────────────────────────────────

export const udsStatus = () => invoke<boolean>("uds_status");
export const openUrl = (url: string) => invoke("open_url", { url });
export const openPath = (path: string) => invoke("open_path", { path });
export const revealPath = (path: string) => invoke("reveal_path", { path });
export const appVersion = () => invoke<string>("app_version");
export const windowLabel = () => invoke<string>("window_label");
export const platform = () => invoke<string>("platform");
