//! Tauri commands exposed to the always-gui frontend.
//!
//! Mirrors the macOS `CLIService` + file helpers the Swift app uses:
//! - `cli`        — run the sibling `always` binary (config show/set/…).
//! - `send_uds_command` — forward one JSON command line to the daemon.
//! - app-file helpers — `~/.always/glossary.json` / `snippets.json`.
//! - history helpers — `<config_dir>/always/history.jsonl`.
//! - openers — URLs, files, "reveal in file manager".

use std::path::PathBuf;
use std::process::Command;

use crate::uds_client::UdsClient;

// ── CLI bridge ──────────────────────────────────────────────────────────────

/// Locate the `always` CLI binary: the sibling of this exe (installed
/// layout puts `always-tauri` next to `always`), else PATH.
fn always_bin() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("ALWAYS_CLI") {
        return Ok(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let name = if cfg!(windows) {
                "always.exe"
            } else {
                "always"
            };
            let candidate = dir.join(name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    Ok(PathBuf::from(if cfg!(windows) {
        "always.exe"
    } else {
        "always"
    }))
}

/// Run `always <args…>` and return stdout, like `CLIService.runCLI`.
#[tauri::command]
pub async fn cli(args: Vec<String>) -> Result<String, String> {
    let bin = always_bin()?;
    tauri::async_runtime::spawn_blocking(move || {
        let output = Command::new(&bin)
            .args(&args)
            .output()
            .map_err(|e| format!("spawn {}: {e}", bin.display()))?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        if output.status.success() {
            Ok(stdout)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!(
                "`always {}` exited {}: {}{}",
                args.join(" "),
                output.status,
                stderr.trim(),
                if stderr.is_empty() { stdout.trim() } else { "" }
            ))
        }
    })
    .await
    .map_err(|e| format!("cli task: {e}"))?
}

// ── Daemon command channel ──────────────────────────────────────────────────

/// Forward one raw JSON command line to the daemon. Passing the line
/// through verbatim means new `DaemonCommand` variants never need a
/// Rust-side enum update before the GUI can use them.
#[tauri::command]
pub async fn send_uds_command(
    uds: tauri::State<'_, std::sync::Arc<UdsClient>>,
    line: String,
) -> Result<(), String> {
    uds.send_line(line).await.map_err(|e| e.to_string())
}

/// Whether the daemon IPC channel is currently connected — the frontend
/// polls this since the one-shot `uds_connected` event can race the
/// webview's JS listener registration.
#[tauri::command]
pub async fn uds_status(uds: tauri::State<'_, std::sync::Arc<UdsClient>>) -> Result<bool, String> {
    Ok(uds.is_connected().await)
}

// ── ~/.always files (glossary.json, snippets.json) ─────────────────────────

fn always_home_file(name: &str) -> Result<PathBuf, String> {
    if !matches!(name, "glossary.json" | "snippets.json") {
        return Err(format!("refusing to touch {name}"));
    }
    dirs::home_dir()
        .map(|h| h.join(".always").join(name))
        .ok_or_else(|| "no home dir".to_string())
}

#[tauri::command]
pub fn app_file_path(name: String) -> Result<String, String> {
    Ok(always_home_file(&name)?.to_string_lossy().to_string())
}

#[tauri::command]
pub fn read_app_file(name: String) -> Result<String, String> {
    let path = always_home_file(&name)?;
    if !path.exists() {
        // Mirror the Swift side: an absent file is an empty list.
        return Ok("[]".to_string());
    }
    std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))
}

#[tauri::command]
pub fn write_app_file(name: String, content: String) -> Result<(), String> {
    let path = always_home_file(&name)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    }
    // Atomic-ish write: temp file + rename, same intent as the Swift store.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, content).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename to {}: {e}", path.display()))
}

// ── Transcript history (<config_dir>/always/history.jsonl) ─────────────────

fn history_path() -> PathBuf {
    if let Ok(p) = std::env::var("ALWAYS_HISTORY_PATH") {
        return PathBuf::from(p);
    }
    let suffix = std::env::var("ALWAYS_INSTANCE")
        .ok()
        .filter(|i| i != "prod" && !i.is_empty())
        .map(|i| format!("-{i}"))
        .unwrap_or_default();
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(format!("always{suffix}"))
        .join("history.jsonl")
}

#[tauri::command]
pub fn history_file_path() -> String {
    history_path().to_string_lossy().to_string()
}

/// Return today's (and yesterday's tail) entries, newest last, as
/// `{text, timestamp_ms}`. Parses the JSONL the daemon appends per paste.
#[tauri::command]
pub fn read_history() -> Result<Vec<serde_json::Value>, String> {
    let path = history_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let mut entries = Vec::new();
    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let text = v
            .get("raw_text")
            .or_else(|| v.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        let ts = v
            .get("timestamp_ms")
            .or_else(|| v.get("timestamp"))
            .and_then(|t| t.as_i64())
            .unwrap_or(0);
        entries.push(serde_json::json!({ "text": text, "timestamp_ms": ts }));
    }
    Ok(entries)
}

#[tauri::command]
pub fn clear_history() -> Result<(), String> {
    let path = history_path();
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("remove {}: {e}", path.display()))?;
    }
    Ok(())
}

// ── Openers ────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
    open_detached(&url)
}

#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    open_detached(&path)
}

/// Reveal `path` in the platform file manager (Explorer / Finder / xdg).
#[tauri::command]
pub fn reveal_path(path: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        Command::new("explorer.exe")
            .arg("/select,")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg("-R")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open")
            .arg(
                std::path::Path::new(&path)
                    .parent()
                    .unwrap_or(std::path::Path::new(&path)),
            )
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn open_detached(target: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        Command::new("cmd")
            .args(["/C", "start", "", target])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(target)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open")
            .arg(target)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ── Misc ───────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
pub fn window_label(window: tauri::Window) -> String {
    window.label().to_string()
}

/// Which platform the frontend should present (permission rows, copy).
#[tauri::command]
pub fn platform() -> String {
    std::env::consts::OS.to_string()
}
