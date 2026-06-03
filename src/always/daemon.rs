// Daemon lifecycle commands print user-facing status to stdout — that's
// the CLI contract, not stray debug output.
#![allow(clippy::print_stdout)]

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::always::{AlwaysConfig, config as always_config};
use crate::config;

pub fn pid_path() -> PathBuf {
    config::config_dir().join("always.pid")
}

/// Best-effort UDS socket path. Mirrors `uds_server::socket_path` but is
/// infallible so it can be used in cleanup paths where erroring is wrong
/// (Drop, signal handlers). Returns `None` when `$HOME` is unset.
pub fn socket_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        std::env::var("HOME").ok().map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Caches")
                .join("Always")
                .join("always.sock")
        })
    } else if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        Some(PathBuf::from(runtime_dir).join("always.sock"))
    } else {
        Some(PathBuf::from("/tmp/always.sock"))
    }
}

pub fn read_pid() -> Option<u32> {
    std::fs::read_to_string(pid_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

pub fn is_running() -> bool {
    read_pid().is_some_and(process_is_running)
}

fn is_daemon_run_command(command: &str) -> bool {
    let mut args = command.split_whitespace();
    let Some(exe) = args.next() else {
        return false;
    };
    let Some(subcommand) = args.next() else {
        return false;
    };
    let Some(name) = std::path::Path::new(exe)
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return false;
    };
    matches!(name, "always" | "always-daemon") && subcommand == "run"
}

fn process_table() -> Vec<(u32, String)> {
    let output = match std::process::Command::new("ps")
        .args(["-eo", "pid=,args="])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            let split = trimmed.find(char::is_whitespace)?;
            let pid = trimmed[..split].parse().ok()?;
            let command = trimmed[split..].trim().to_string();
            Some((pid, command))
        })
        .collect()
}

/// All PIDs whose argv matches a voice daemon (may include the current process).
pub fn list_daemon_pids() -> Vec<u32> {
    let mut pids: Vec<u32> = process_table()
        .into_iter()
        .filter_map(|(pid, command)| is_daemon_run_command(&command).then_some(pid))
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// Terminate stray daemon processes. Never signals `except_pid` (the caller).
pub fn kill_orphan_daemon_processes_except(except_pid: u32) {
    let victims: Vec<u32> = list_daemon_pids()
        .into_iter()
        .filter(|pid| *pid != except_pid)
        .collect();

    for pid in &victims {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
    std::thread::sleep(Duration::from_millis(200));
    for pid in victims {
        if process_is_running(pid) {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
    }
    std::thread::sleep(Duration::from_millis(100));
}

pub fn kill_orphan_daemon_processes() {
    kill_orphan_daemon_processes_except(std::process::id());
}

/// If another always daemon is still running, terminate it. Both
/// processes capture the same mic and paste independently — the most
/// common cause of "double transcript" reports.
pub fn reconcile_duplicate_processes() {
    let self_pid = std::process::id();
    let others: Vec<u32> = list_daemon_pids()
        .into_iter()
        .filter(|pid| *pid != self_pid)
        .collect();
    if others.is_empty() {
        return;
    }
    tracing::error!(
        self_pid,
        others = ?others,
        "duplicate_daemon_processes_detected"
    );
    kill_orphan_daemon_processes_except(self_pid);
}

/// Block until no `always-daemon run` / `always run` processes remain.
/// Returns `true` when the process list is empty.
pub fn wait_until_no_daemon_processes(max_wait: Duration) -> bool {
    let deadline = Instant::now() + max_wait;
    while Instant::now() < deadline {
        if list_daemon_pids().is_empty() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    list_daemon_pids().is_empty()
}

/// True when a process is already accepting connections on the UDS socket.
pub fn socket_is_live(sock: &std::path::Path) -> bool {
    std::os::unix::net::UnixStream::connect(sock).is_ok()
}

pub fn start(cfg: &AlwaysConfig) -> Result<()> {
    if is_running() {
        println!("Always-on daemon already running.");
        return Ok(());
    }
    if let Some(sock) = socket_path()
        && sock.exists()
        && socket_is_live(&sock)
    {
        println!("Always-on daemon already running.");
        return Ok(());
    }
    let existing = list_daemon_pids();
    if !existing.is_empty() {
        println!("Always-on daemon already running (pids: {:?}).", existing);
        return Ok(());
    }
    kill_orphan_daemon_processes();
    if !wait_until_no_daemon_processes(Duration::from_secs(5)) {
        tracing::warn!(
            pids = ?list_daemon_pids(),
            "daemon processes still alive after kill — forcing second SIGKILL pass"
        );
        kill_orphan_daemon_processes();
        let _ = wait_until_no_daemon_processes(Duration::from_secs(2));
    }
    remove_stale_pid();
    remove_stale_socket();

    let exe = std::env::current_exe().context("cannot find always binary")?;
    let child = std::process::Command::new(&exe)
        .args([
            "run",
            "--lang",
            &cfg.lang,
            "--timeout",
            &cfg.timeout_secs.to_string(),
            "--silence",
            &cfg.silence_secs.to_string(),
        ])
        .args(if cfg.auto_enter {
            vec!["--auto-enter"]
        } else {
            vec![]
        })
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("failed to spawn always daemon process")?;

    println!("Always-on daemon starting (pid {})...", child.id());
    println!("Log: {}", cfg.log_path.display());
    // Cold start can take several seconds (config, models, UDS bind).
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if is_running() {
            println!("Always-on daemon ready.");
            return Ok(());
        }
        if let Some(sock) = socket_path()
            && sock.exists()
            && socket_is_live(&sock)
        {
            println!("Always-on daemon ready.");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("Always-on daemon failed to start")
}

pub fn stop() -> Result<()> {
    kill_orphan_daemon_processes();
    let _ = wait_until_no_daemon_processes(Duration::from_secs(3));
    remove_stale_pid();
    remove_stale_socket();
    if let Some(pid) = read_pid() {
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
        println!("Always-on daemon stopped (pid {pid}).");
    } else {
        println!("Always-on daemon not running.");
    }
    Ok(())
}

pub fn status() -> Result<()> {
    let pids = list_daemon_pids();
    if pids.len() > 1 {
        println!(
            "WARNING: {} always daemon processes detected (pids: {:?}) — duplicate transcripts possible",
            pids.len(),
            pids
        );
    }
    if is_running() {
        if let Some(pid) = read_pid() {
            println!("Always-on daemon running (pid {pid})");
            println!("Log: {}", always_config::configured_log_path()?.display());
        }
    } else {
        remove_stale_pid();
        println!("Always-on daemon not running.");
    }
    Ok(())
}

pub struct PidGuard {
    _lock: File,
}

impl PidGuard {
    pub fn install() -> Result<Self> {
        kill_orphan_daemon_processes();
        if !wait_until_no_daemon_processes(Duration::from_secs(5)) {
            kill_orphan_daemon_processes();
            let _ = wait_until_no_daemon_processes(Duration::from_secs(2));
        }
        remove_stale_pid();
        remove_stale_socket();

        let path = pid_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .context("failed to open always PID file")?;
        acquire_exclusive_lock(&file)
            .context("Always-on daemon already running (pid lock held)")?;
        write!(file, "{}", std::process::id())?;
        let count = list_daemon_pids();
        if count.len() > 1 {
            tracing::error!(
                pids = ?count,
                "multiple daemon processes after PidGuard::install — duplicate transcripts likely"
            );
        }
        Ok(Self { _lock: file })
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        let fd = self._lock.as_raw_fd();
        unsafe {
            libc::flock(fd, libc::LOCK_UN);
        }
        if let Ok(log_path) = always_config::configured_log_path()
            && let Ok(mut log) = crate::always::log::Logger::open(&log_path)
        {
            log.write(crate::always::log::Event::Stop);
        }
        let _ = std::fs::remove_file(pid_path());
        if let Some(sock) = socket_path() {
            let _ = std::fs::remove_file(sock);
        }
    }
}

fn acquire_exclusive_lock(file: &File) -> Result<()> {
    let fd = file.as_raw_fd();
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(());
    }
    anyhow::bail!("pid file flock failed (rc={rc})")
}

fn remove_stale_pid() {
    if read_pid().is_some_and(|pid| !process_is_running(pid)) {
        let _ = std::fs::remove_file(pid_path());
    }
}

fn remove_stale_socket() {
    if read_pid().is_some_and(process_is_running) {
        return;
    }
    if let Some(sock) = socket_path() {
        if sock.exists() && socket_is_live(&sock) {
            tracing::warn!(
                socket = %sock.display(),
                "not removing live UDS socket — another daemon is listening"
            );
            return;
        }
        let _ = std::fs::remove_file(sock);
    }
}

/// Install async signal handlers that translate SIGTERM/SIGINT into a
/// graceful `std::process::exit(0)` so Rust runs `PidGuard::Drop` and
/// cleans up the pid + socket files.
pub fn install_signal_handlers(handle: &tokio::runtime::Handle) {
    use tokio::signal::unix::{SignalKind, signal};
    let _guard = handle.enter();
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "failed_to_install_sigterm_handler");
            return;
        }
    };
    let mut int = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "failed_to_install_sigint_handler");
            return;
        }
    };
    handle.spawn(async move {
        tokio::select! {
            _ = term.recv() => tracing::info!("received SIGTERM, shutting down"),
            _ = int.recv()  => tracing::info!("received SIGINT, shutting down"),
        }
        if let Ok(log_path) = always_config::configured_log_path()
            && let Ok(mut log) = crate::always::log::Logger::open(&log_path)
        {
            log.write(crate::always::log::Event::Stop);
        }
        let _ = std::fs::remove_file(pid_path());
        if let Some(sock) = socket_path() {
            let _ = std::fs::remove_file(sock);
        }
        std::process::exit(0);
    });
}

fn process_is_running(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .output()
        .is_ok_and(|output| output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_daemon_pids_dedupes() {
        let pids = list_daemon_pids();
        let mut sorted = pids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(pids.len(), sorted.len());
    }

    #[test]
    fn daemon_run_matcher_rejects_diagnostic_commands() {
        assert!(is_daemon_run_command(
            "/Applications/Always.app/Contents/MacOS/always-daemon run --lang en"
        ));
        assert!(is_daemon_run_command(
            "/Users/livio/Documents/always/target/release/always run --timeout 30"
        ));
        assert!(!is_daemon_run_command(
            "/bin/zsh -lc ps aux | rg \"(/Applications/Always.app|always run|always-daemon)\""
        ));
        assert!(!is_daemon_run_command(
            "rg (/Applications/Always.app|always run|always-daemon)"
        ));
        assert!(!is_daemon_run_command(
            "/Applications/Always.app/Contents/MacOS/always-daemon status"
        ));
    }
}
