use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::always::{config as always_config, AlwaysConfig};
use crate::config;

pub fn pid_path() -> PathBuf {
    config::config_dir().join("always.pid")
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
        if let Ok(log_path) = always_config::configured_log_path() {
            if let Ok(mut log) = crate::always::log::Logger::open(&log_path) {
                log.write(crate::always::log::Event::Stop);
            }
        }
        let _ = std::fs::remove_file(pid_path());
    }
}

fn remove_stale_pid() {
    if read_pid().is_some_and(|pid| !process_is_running(pid)) {
        let _ = std::fs::remove_file(pid_path());
    }
}

fn process_is_running(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .output()
        .is_ok_and(|output| output.status.success())
}
