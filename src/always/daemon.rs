// Daemon lifecycle commands print user-facing status to stdout — that's
// the CLI contract, not stray debug output.
#![allow(clippy::print_stdout)]

use std::io::Write as _;
use std::path::PathBuf;

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

pub fn start(cfg: &AlwaysConfig) -> Result<()> {
    if is_running() {
        println!("Always-on daemon already running.");
        return Ok(());
    }
    remove_stale_pid();

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
    std::thread::sleep(std::time::Duration::from_millis(500));
    if is_running() {
        println!("Always-on daemon ready.");
        Ok(())
    } else {
        anyhow::bail!("Always-on daemon failed to start")
    }
}

pub fn stop() -> Result<()> {
    if !is_running() {
        println!("Always-on daemon not running.");
        remove_stale_pid();
        return Ok(());
    }

    if let Some(pid) = read_pid() {
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
        let _ = std::fs::remove_file(pid_path());
        println!("Always-on daemon stopped (pid {pid}).");
    }
    Ok(())
}

pub fn status() -> Result<()> {
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

pub struct PidGuard;

impl PidGuard {
    pub fn install() -> Result<Self> {
        remove_stale_pid();
        remove_stale_socket();
        if is_running() {
            anyhow::bail!("Always-on daemon already running");
        }
        let path = pid_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .context("failed to write always PID file")?;
        write!(file, "{}", std::process::id())?;
        Ok(Self)
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        // Log daemon stop
        if let Ok(log_path) = always_config::configured_log_path()
            && let Ok(mut log) = crate::always::log::Logger::open(&log_path)
        {
            log.write(crate::always::log::Event::Stop);
        }
        let _ = std::fs::remove_file(pid_path());
        // Socket file must die with the daemon — a stale socket left
        // behind makes the next daemon's `bind()` race with the old
        // inode (we already `remove_file` in `uds_server::start_server`,
        // but cleaning up here closes the gap where the daemon exits
        // before the server task ever ran).
        if let Some(sock) = socket_path() {
            let _ = std::fs::remove_file(sock);
        }
    }
}

fn remove_stale_pid() {
    if read_pid().is_some_and(|pid| !process_is_running(pid)) {
        let _ = std::fs::remove_file(pid_path());
    }
}

/// Remove the UDS socket file if no live daemon owns it. We can't tell
/// directly which process owns a Unix socket, so we infer it from the
/// PID file: if `pid_path()` is gone (or its PID is dead), the socket
/// is by definition stale.
fn remove_stale_socket() {
    if read_pid().is_some_and(process_is_running) {
        return; // live daemon — leave its socket alone
    }
    if let Some(sock) = socket_path() {
        let _ = std::fs::remove_file(sock);
    }
}

/// Install async signal handlers that translate SIGTERM/SIGINT into a
/// graceful `std::process::exit(0)` so Rust runs `PidGuard::Drop` and
/// cleans up the pid + socket files. Without this, the OS reaps the
/// process abruptly and leaves stale files behind — that is exactly
/// what makes the next Swift app launch hang on a phantom daemon.
///
/// Pass a tokio runtime handle — the signal listener task lives on it.
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
        // `std::process::exit` does NOT run Drop, so mirror everything
        // PidGuard::Drop would do — including the Stop event, otherwise
        // SIGTERM (the dominant shutdown path) leaves "started without
        // stop" gaps in the log.
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
