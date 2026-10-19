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
use std::time::Instant;
use tauri::Manager;

use crate::uds_client::UdsClient;
use crate::window_manager::{ShowOnboardingReason, WindowManager};

// ── CLI bridge ──────────────────────────────────────────────────────────────

/// Locate the `always` CLI binary: the sibling of this exe (installed
/// layout puts `always-tauri` next to `always`), else PATH.
fn always_bin(resource_dir: Option<&std::path::Path>) -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("ALWAYS_CLI") {
        return Ok(PathBuf::from(p));
    }
    if let Some(resources) = resource_dir {
        let name = if cfg!(windows) {
            "always.exe"
        } else {
            "always"
        };
        let bundled = resources.join("daemon").join(name);
        if bundled.exists() {
            return Ok(bundled);
        }
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

/// Ensure the sibling daemon is running before the settings UI connects.
/// The daemon's `start` command is detached and single-instance safe, so this
/// is also a no-op when an existing daemon already owns the endpoint.
pub fn start_daemon(resource_dir: Option<&std::path::Path>) -> Result<(), String> {
    let bin = always_bin(resource_dir)?;
    let metadata = std::fs::metadata(&bin).ok();
    let daemon_is_file = metadata.as_ref().is_some_and(|metadata| metadata.is_file());
    #[cfg(unix)]
    let daemon_is_executable = {
        use std::os::unix::fs::PermissionsExt;
        metadata
            .as_ref()
            .is_some_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    };
    #[cfg(not(unix))]
    let daemon_is_executable = daemon_is_file;

    let started_at = Instant::now();
    tracing::info!(
        daemon_path = %bin.display(),
        daemon_is_file,
        daemon_is_executable,
        resource_dir = ?resource_dir.map(|path| path.display().to_string()),
        "daemon_start_begin"
    );
    let output = Command::new(&bin)
        .arg("start")
        .output()
        .map_err(|error| {
            tracing::warn!(
                daemon_path = %bin.display(),
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                error = %error,
                "daemon_start_spawn_failed"
            );
            format!("start daemon with {}: {error}", bin.display())
        })?;
    if output.status.success() {
        tracing::info!(
            daemon_path = %bin.display(),
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            exit_status = %output.status,
            "daemon_start_succeeded"
        );
        Ok(())
    } else {
        tracing::warn!(
            daemon_path = %bin.display(),
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            exit_status = %output.status,
            "daemon_start_nonzero_exit"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "`always start` exited {}: {}",
            output.status,
            stderr.trim()
        ))
    }
}

/// Run `always <args…>` and return stdout, like `CLIService.runCLI`.
#[tauri::command]
pub async fn cli(app: tauri::AppHandle, args: Vec<String>) -> Result<String, String> {
    let resource_dir = app.path().resource_dir().ok();
    let bin = always_bin(resource_dir.as_deref())?;
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
    Ok(parse_history_entries(&content))
}

fn parse_history_entries(content: &str) -> Vec<serde_json::Value> {
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
            .or_else(|| v.get("ts"))
            .and_then(|t| t.as_i64())
            .unwrap_or(0);
        entries.push(serde_json::json!({ "text": text, "timestamp_ms": ts }));
    }
    entries
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
        use std::ffi::c_void;
        use std::ptr::null_mut;

        let operation = nul_terminated_wide("open");
        let target = nul_terminated_wide(target);

        #[link(name = "shell32")]
        extern "system" {
            fn ShellExecuteW(
                hwnd: *mut c_void,
                operation: *const u16,
                file: *const u16,
                parameters: *const u16,
                directory: *const u16,
                show_command: i32,
            ) -> *mut c_void;
        }

        let result = unsafe {
            ShellExecuteW(
                null_mut(),
                operation.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
            )
        };
        if result as isize <= 32 {
            return Err(format!("Windows shell could not open target (code {})", result as isize));
        }
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

#[cfg(any(windows, test))]
fn nul_terminated_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::{nul_terminated_wide, parse_history_entries};

    #[test]
    fn history_reader_accepts_daemon_timestamp_field() {
        let entries = parse_history_entries(
            r#"{"ts":1720000000123,"raw_text":"hello","processed_text":"Hello."}"#,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["text"], "hello");
        assert_eq!(entries[0]["timestamp_ms"], 1720000000123i64);
    }

    #[test]
    fn windows_shell_target_remains_one_literal_argument() {
        let target = "https://example.test/path & calc.exe";
        let encoded = nul_terminated_wide(target);
        let decoded = String::from_utf16(&encoded[..encoded.len() - 1]).unwrap();

        assert_eq!(decoded, target);
        assert_eq!(encoded.last(), Some(&0));
    }
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

// ── Windows ────────────────────────────────────────────────────────────────

/// Both windows start hidden; the settings webview calls one of these once
/// it knows whether onboarding is done. They build the manager from the app
/// handle rather than managed state: the webview can call before `setup` runs.
#[tauri::command]
pub fn show_settings_window(app: tauri::AppHandle) {
    WindowManager::new(app).show_settings();
}

#[tauri::command]
pub fn show_onboarding_window(app: tauri::AppHandle) {
    WindowManager::new(app).show_onboarding(ShowOnboardingReason::FirstLaunch);
}

/// Onboarding finished or skipped: swap to the settings window.
#[tauri::command]
pub fn finish_onboarding(app: tauri::AppHandle) {
    let wm = WindowManager::new(app);
    wm.hide_onboarding();
    wm.show_settings();
}

/// Which platform the frontend should present (permission rows, copy).
#[tauri::command]
pub fn platform() -> String {
    std::env::consts::OS.to_string()
}
