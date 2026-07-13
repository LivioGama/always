// The `logs` subcommand renders structured log records to the user's
// terminal — stdout/stderr are the deliberate output channels.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use anyhow::{Context, Result};
use clap::Parser;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// View and manage Always logs
#[derive(Parser, Debug)]
pub struct LogsCommand {
    /// Print the log directory path and exit
    #[arg(long)]
    pub path: bool,

    /// Filter logs by time (e.g., "1h", "30m", "1d")
    #[arg(long)]
    pub since: Option<String>,

    /// Filter logs by level (e.g., "error", "warn", "info", "debug")
    #[arg(long)]
    pub level: Option<String>,

    /// Use Console.app on macOS instead of tail
    #[arg(long)]
    pub console: bool,

    /// Render logs in pretty emoji format (mirrors pre-9475cc5 layout)
    #[arg(long)]
    pub pretty: bool,

    /// Specific date (YYYY-MM-DD). Defaults to today.
    #[arg(long)]
    pub date: Option<String>,
}

pub fn handle_logs(cmd: LogsCommand) -> Result<()> {
    let log_dir = always::always::telemetry::get_log_directory();

    if cmd.path {
        println!("{}", log_dir.display());
        return Ok(());
    }

    if cmd.console {
        #[cfg(target_os = "macos")]
        {
            return view_logs_in_console();
        }
        #[cfg(not(target_os = "macos"))]
        {
            eprintln!("--console flag is only supported on macOS");
            std::process::exit(1);
        }
    }

    let log_file = resolve_log_file(&log_dir, cmd.date.as_deref())?;

    if cmd.pretty {
        return tail_pretty(&log_file, cmd.level.as_deref());
    }

    let mut tail_cmd = Command::new("tail");
    tail_cmd.arg("-F").arg(&log_file);
    let mut child = tail_cmd.spawn()?;
    child.wait()?;

    Ok(())
}

fn resolve_log_file(log_dir: &Path, date: Option<&str>) -> Result<PathBuf> {
    if let Some(d) = date {
        let path = log_dir.join(format!("always.{d}"));
        if !path.exists() {
            anyhow::bail!(
                "log file not found: {}. Available files: see `always logs --path`",
                path.display()
            );
        }
        return Ok(path);
    }

    // tracing-appender rotates on UTC, but users think in local time. Try
    // local date first, then UTC date, then fall back to the newest
    // `always.*` file in the log dir.
    let local = chrono::Local::now().format("%Y-%m-%d").to_string();
    let utc = chrono::Utc::now().format("%Y-%m-%d").to_string();
    for d in [&local, &utc] {
        let path = log_dir.join(format!("always.{d}"));
        if path.exists() {
            return Ok(path);
        }
    }

    if let Some(newest) = newest_log_file(log_dir) {
        return Ok(newest);
    }

    anyhow::bail!(
        "no log files found in {}. Run the daemon first.",
        log_dir.display()
    );
}

fn newest_log_file(log_dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(log_dir).ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name()?.to_str()?;
        if !name.starts_with("always.") {
            continue;
        }
        let mtime = entry.metadata().ok()?.modified().ok()?;
        if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, p)| p)
}

fn tail_pretty(path: &Path, level_filter: Option<&str>) -> Result<()> {
    let mut child = Command::new("tail")
        .arg("-F")
        .arg("-n")
        .arg("100")
        .arg(path)
        .stdout(Stdio::piped())
        .spawn()
        .context("Failed to spawn tail")?;

    let stdout = child.stdout.take().context("tail stdout missing")?;
    let reader = BufReader::new(stdout);

    let level_filter = level_filter.map(str::to_uppercase);

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(json) = serde_json::from_str::<Value>(&line) else {
            // Not JSON — print raw
            println!("{line}");
            continue;
        };
        if let Some(filter) = &level_filter
            && let Some(lvl) = json.get("level").and_then(Value::as_str)
            && lvl != filter
        {
            continue;
        }
        if let Some(rendered) = render_event(&json) {
            println!("{rendered}");
        }
    }

    child.wait().ok();
    Ok(())
}

fn render_event(json: &Value) -> Option<String> {
    let fields = json.get("fields")?;
    let message = fields.get("message").and_then(Value::as_str)?;
    let ts = json
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(format_timestamp)
        .unwrap_or_default();
    let prefix = if ts.is_empty() {
        String::new()
    } else {
        format!("[{ts}] ")
    };

    let body = match message {
        "daemon_started" => format!(
            "🚀 DAEMON STARTED  ⚙️  energy={}  silence={}s  filter={}  auto_enter={}",
            field_str(fields, "energy_threshold"),
            field_str(fields, "silence_secs"),
            field_str(fields, "filter_enabled"),
            field_str(fields, "auto_enter"),
        ),
        "daemon_stopped" => "🛑 DAEMON STOPPED".to_string(),
        "voice_detected" => "🎤 Voice detected...".to_string(),
        "transcription_received" => {
            let energy = field_str(fields, "energy");
            let text = fields.get("text").and_then(Value::as_str);
            match text {
                Some(t) => format!("✓ Transcribed: \"{t}\"\n           │ Energy: {energy}"),
                None => format!(
                    "✓ Transcribed: <{} chars hidden>  │ Energy: {energy}",
                    field_str(fields, "chars")
                ),
            }
        }
        "transcription_pasted" => {
            let text = fields
                .get("processed_text")
                .or_else(|| fields.get("raw_text"))
                .and_then(Value::as_str);
            let energy = field_str(fields, "energy");
            match text {
                Some(t) => format!("✎ Pasted: {t}\n           │ Energy: {energy}"),
                None => format!(
                    "✎ Pasted: <{} chars hidden>  │ Energy: {energy}",
                    field_str(fields, "chars")
                ),
            }
        }
        "transcription_filtered" => {
            let reason = field_str(fields, "reason");
            let energy = field_str(fields, "energy");
            let text = fields.get("text").and_then(Value::as_str);
            match text {
                Some(t) => format!("🚫 Filtered [{reason}]: {t}\n           │ Energy: {energy}"),
                None => format!(
                    "🚫 Filtered [{reason}]: <{} chars hidden>  │ Energy: {energy}",
                    field_str(fields, "chars")
                ),
            }
        }
        "force_pasted_filtered" => {
            let text = fields.get("text").and_then(Value::as_str);
            match text {
                Some(t) => format!("⚡ Force-pasted: {t}"),
                None => format!(
                    "⚡ Force-pasted: <{} chars hidden>",
                    field_str(fields, "chars")
                ),
            }
        }
        "dropped_low_energy" => format!("· dropped (low energy {})", field_str(fields, "energy")),
        "dropped_noise" => format!("· dropped (noise, {} chars)", field_str(fields, "chars")),
        "pause_toggled" => {
            if field_str(fields, "paused") == "true" {
                "⏸  Paused".to_string()
            } else {
                "▶  Resumed".to_string()
            }
        }
        "auto_enter_toggled" => {
            let on = field_str(fields, "enabled") == "true";
            format!("↵  Auto-enter {}", if on { "ON" } else { "OFF" })
        }
        "microphone_auto_paused" => {
            format!("🎙️  Auto-paused — apps: {}", field_str(fields, "apps"))
        }
        "microphone_auto_resumed" => "🎙️  Auto-resumed".to_string(),
        "daemon_error" => format!("❌ {}", field_str(fields, "message")),
        "uds_server_listening" => format!("🔌 UDS listening: {}", field_str(fields, "socket_path")),
        other => {
            let level = json.get("level").and_then(Value::as_str).unwrap_or("INFO");
            format!("[{level}] {other} {}", flatten_fields(fields))
        }
    };

    Some(format!("{prefix}{body}"))
}

fn field_str(fields: &Value, key: &str) -> String {
    match fields.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn flatten_fields(fields: &Value) -> String {
    let Value::Object(map) = fields else {
        return String::new();
    };
    let mut parts = Vec::new();
    for (k, v) in map {
        if k == "message" {
            continue;
        }
        parts.push(format!("{k}={v}"));
    }
    parts.join(" ")
}

fn format_timestamp(ts: &str) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    Some(
        parsed
            .with_timezone(&chrono::Local)
            .format("%H:%M:%S")
            .to_string(),
    )
}

#[cfg(target_os = "macos")]
fn view_logs_in_console() -> Result<()> {
    let status = Command::new("log")
        .args([
            "stream",
            "--predicate",
            "subsystem CONTAINS \"com.always\"",
            "--level",
            "debug",
        ])
        .status()?;

    if !status.success() {
        anyhow::bail!("Failed to open Console.app logs");
    }

    Ok(())
}
