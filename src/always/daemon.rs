// Daemon lifecycle commands print user-facing status to stdout — that's
// the CLI contract, not stray debug output.
#![allow(clippy::print_stdout)]

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::always::{AlwaysConfig, config as always_config, event, pause};
use crate::config;

pub fn pid_path() -> PathBuf {
    config::config_dir().join("always.pid")
}

fn build_id_path() -> PathBuf {
    config::config_dir().join("always.build-id")
}

#[derive(Debug, PartialEq, Eq)]
enum BuildFreshness {
    Fresh,
    Stale { running: String, current: String },
    Unknown,
}

fn build_freshness(current: &str, running: Option<&str>) -> BuildFreshness {
    match running {
        Some(running) if running == current => BuildFreshness::Fresh,
        Some(running) => BuildFreshness::Stale {
            running: running.to_string(),
            current: current.to_string(),
        },
        None => BuildFreshness::Unknown,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RuntimeBuildRecord {
    pid: u32,
    pid_file_mtime_nanos: u128,
    build_id: String,
}

fn parse_runtime_build_record(record: &str) -> Option<RuntimeBuildRecord> {
    let mut lines = record.lines();
    let pid = lines.next()?.parse().ok()?;
    let pid_file_mtime_nanos = lines.next()?.parse().ok()?;
    let build_id = lines.next()?.trim();
    (!build_id.is_empty()).then(|| RuntimeBuildRecord {
        pid,
        pid_file_mtime_nanos,
        build_id: build_id.to_string(),
    })
}

fn pid_file_mtime_nanos() -> Option<u128> {
    std::fs::metadata(pid_path())
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos())
}

fn build_id_for_process(
    record: &str,
    running_pid: u32,
    pid_file_mtime_nanos: u128,
    is_daemon_process: bool,
) -> Option<String> {
    if !is_daemon_process {
        return None;
    }
    let record = parse_runtime_build_record(record)?;
    (record.pid == running_pid && record.pid_file_mtime_nanos == pid_file_mtime_nanos)
        .then_some(record.build_id)
}

fn running_build_id() -> Option<String> {
    let running_pid = read_pid()?;
    build_id_for_process(
        &std::fs::read_to_string(build_id_path()).ok()?,
        running_pid,
        pid_file_mtime_nanos()?,
        list_daemon_pids().contains(&running_pid),
    )
}

fn remove_build_id_for_pid(pid: u32) {
    let Ok(contents) = std::fs::read_to_string(build_id_path()) else {
        return;
    };
    let Some(record) = parse_runtime_build_record(&contents) else {
        return;
    };
    if record.pid == pid && Some(record.pid_file_mtime_nanos) == pid_file_mtime_nanos() {
        let _ = std::fs::remove_file(build_id_path());
    }
}

fn current_build_freshness() -> BuildFreshness {
    build_freshness(
        crate::always::event::BUILD_ID,
        running_build_id().as_deref(),
    )
}

fn refresh_guidance() -> &'static str {
    if config::instance() == "dev" {
        "After dictation is complete, refresh the dev daemon with ./scripts/dev-rebuild-macos.sh."
    } else {
        "After dictation is complete, refresh this instance using its supported production rebuild procedure."
    }
}

fn report_existing_daemon() -> Result<()> {
    match current_build_freshness() {
        BuildFreshness::Fresh => {
            println!("Always-on daemon already running (build matches current executable).");
            Ok(())
        }
        BuildFreshness::Stale { running, current } => anyhow::bail!(
            "Always-on daemon is stale (running build {running}, current executable {current}). {}",
            refresh_guidance()
        ),
        BuildFreshness::Unknown => anyhow::bail!(
            "Always-on daemon is running, but its build identity is unknown. It may be an older daemon. {}",
            refresh_guidance()
        ),
    }
}

/// Best-effort UDS socket path. Mirrors `uds_server::socket_path` but is
/// infallible so it can be used in cleanup paths where erroring is wrong
/// (Drop, signal handlers). On Windows returns the daemon's named-pipe
/// endpoint (`\\.\pipe\always[-dev]`); `None` when `$HOME` is unset.
pub fn socket_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var("HOME").ok().map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Caches")
                .join("Always")
                .join(format!("always{}.sock", config::instance_suffix()))
        })
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Same per-instance name the server binds: without the suffix a dev
        // CLI on Linux reached the production daemon.
        let name = format!("always{}.sock", config::instance_suffix());
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            Some(PathBuf::from(runtime_dir).join(name))
        } else {
            Some(PathBuf::from("/tmp").join(name))
        }
    }
    #[cfg(target_os = "windows")]
    {
        Some(PathBuf::from(format!(
            r"\\.\pipe\always{}",
            config::instance_suffix()
        )))
    }
}

pub fn read_pid() -> Option<u32> {
    std::fs::read_to_string(pid_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

pub fn is_running() -> bool {
    let daemon_pids = list_daemon_pids();
    read_pid().is_some_and(|pid| process_is_running(pid) && pid_is_daemon(pid, &daemon_pids))
}

fn pid_is_daemon(pid: u32, daemon_pids: &[u32]) -> bool {
    daemon_pids.contains(&pid)
}

fn daemon_is_running(pid_matches_daemon: bool, socket_live: bool) -> bool {
    pid_matches_daemon || socket_live
}

fn arguments_after_executable<'a>(command: &'a str, executable: &str) -> Option<&'a str> {
    let command = command.trim_start();
    let (command, quoted) = if let Some(command) = command.strip_prefix('"') {
        (command, true)
    } else {
        (command, false)
    };
    let arguments = command.strip_prefix(executable)?;
    let arguments = if quoted {
        arguments.strip_prefix('"')?
    } else {
        arguments
    };
    arguments
        .starts_with(char::is_whitespace)
        .then(|| arguments.trim_start())
}

fn daemon_process_matches(instance: &str, own_exe: &str, process_exe: &str, command: &str) -> bool {
    if !exe_is_daemon(process_exe) || !same_instance_for(instance, own_exe, process_exe) {
        return false;
    }
    let Some(arguments) = arguments_after_executable(command, process_exe) else {
        return false;
    };
    first_argv(arguments).is_some_and(|(subcommand, _)| subcommand == "run")
}

/// Split off the first argv element, honoring `"…"` quoting — Windows
/// command lines (read from a process's PEB) quote argv[0] whenever the
/// path needs it, `ps` output does not.
fn first_argv(command: &str) -> Option<(&str, &str)> {
    let c = command.trim_start();
    if let Some(rest) = c.strip_prefix('"') {
        let end = rest.find('"')?;
        Some((&rest[..end], rest[end + 1..].trim_start()))
    } else {
        // A bare last word is the whole argv[0]: the Linux systemd unit runs
        // `always run`, which `ps` prints with nothing after `run`.
        let end = c.find(char::is_whitespace).unwrap_or(c.len());
        Some((&c[..end], c[end..].trim_start()))
    }
}

/// Basename check tolerant of `.exe` suffixes, Windows path separators
/// and case (`C:\…\Always.exe` vs `/usr/bin/always`).
fn exe_is_daemon(exe: &str) -> bool {
    let base = exe.rsplit(['/', '\\']).next().unwrap_or(exe);
    let stem = base.strip_suffix(".exe").unwrap_or(base);
    stem.eq_ignore_ascii_case("always") || stem.eq_ignore_ascii_case("always-daemon")
}

/// True when `executable` belongs to this process's runtime instance. The
/// kernel-reported executable path, rather than arbitrary command-line text,
/// keeps dev/prod startup cleanup from claiming each other's processes.
fn same_install_directory(own_exe: &str, candidate_exe: &str) -> bool {
    let own_parent = std::path::Path::new(own_exe).parent();
    own_parent.is_some() && own_parent == std::path::Path::new(candidate_exe).parent()
}

/// Pure form of [`same_instance`]. Sandbox instances (anything but prod and
/// dev, e.g. the launch smoke test's `ALWAYS_INSTANCE=smoke`) only ever
/// match their OWN executable — 2026-10-14: the previous rule treated a
/// sandbox like prod and the sandbox daemon's startup sweep killed the
/// production daemon. Prod in turn never matches a sandbox bundle.
fn same_instance_for(instance: &str, own_exe: &str, executable: &str) -> bool {
    match instance {
        "dev" => {
            same_install_directory(own_exe, executable) && executable.contains("Always Dev.app")
        }
        "prod" => {
            same_install_directory(own_exe, executable)
                && !executable.contains("Always Dev.app")
                && !executable.contains("AlwaysSmoke.app")
        }
        _ => !own_exe.is_empty() && executable == own_exe,
    }
}

#[cfg(target_os = "macos")]
fn process_executable_path(pid: u32) -> Option<String> {
    unsafe extern "C" {
        fn proc_pidpath(pid: i32, buffer: *mut u8, buffer_size: u32) -> i32;
    }
    let mut buffer = [0u8; 4096];
    let len = unsafe { proc_pidpath(pid as i32, buffer.as_mut_ptr(), buffer.len() as u32) };
    (len > 0).then(|| String::from_utf8_lossy(&buffer[..len as usize]).into_owned())
}

#[cfg(target_os = "linux")]
fn process_executable_path(pid: u32) -> Option<String> {
    let path = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let path = path.to_string_lossy();
    Some(path.strip_suffix(" (deleted)").unwrap_or(&path).to_string())
}

#[cfg(windows)]
fn process_executable_path(pid: u32) -> Option<String> {
    crate::always::win32_process::process_executable_path(pid)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn process_executable_path(_pid: u32) -> Option<String> {
    None
}

#[cfg(unix)]
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

#[cfg(windows)]
fn process_table() -> Vec<(u32, String)> {
    crate::always::win32_process::process_table()
}

/// All PIDs whose argv matches a voice daemon (may include the current process).
pub fn list_daemon_pids() -> Vec<u32> {
    let own_exe = match std::env::current_exe() {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(_) => return Vec::new(),
    };
    let mut pids: Vec<u32> = process_table()
        .into_iter()
        .filter_map(|(pid, command)| {
            if !command.to_ascii_lowercase().contains("always") {
                return None;
            }
            let process_exe = process_executable_path(pid)?;
            daemon_process_matches(config::instance(), &own_exe, &process_exe, &command)
                .then_some(pid)
        })
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// `kill -TERM` counterpart for this platform. Unix sends SIGTERM (the
/// daemon's signal handler runs the graceful cleanup); Windows has no
/// gentle signal for a detached process, so this is `TerminateProcess`
/// and the pid file is reclaimed by `remove_stale_pid` afterwards.
fn terminate_gracefully(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(windows)]
    {
        crate::always::win32_process::terminate(pid)
    }
}

/// `kill -KILL` counterpart — last-resort stop when the graceful path
/// left the process alive.
fn terminate_forcefully(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(windows)]
    {
        crate::always::win32_process::terminate(pid)
    }
}

/// Terminate stray daemon processes. Never signals `except_pid` (the caller).
pub fn kill_orphan_daemon_processes_except(except_pid: u32) {
    let victims: Vec<u32> = list_daemon_pids()
        .into_iter()
        .filter(|pid| *pid != except_pid)
        .collect();

    for pid in &victims {
        terminate_gracefully(*pid);
    }
    std::thread::sleep(Duration::from_millis(200));
    for pid in victims {
        if process_is_running(pid) {
            terminate_forcefully(pid);
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
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(sock).is_ok()
    }
    #[cfg(windows)]
    {
        // Opening a named pipe IS the liveness probe: it fails with
        // ERROR_FILE_NOT_FOUND when no server created it, succeeds (and
        // immediately closes) when one is listening, and returns
        // ERROR_PIPE_BUSY (231) when the server is alive but every
        // instance is occupied — still live either way.
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(sock)
        {
            Ok(_) => true,
            Err(e) => e.raw_os_error() == Some(231),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

/// IPC endpoint liveness probe. On unix the socket file must exist AND
/// accept a connection; on Windows `\\.\pipe\` paths never stat as
/// files, so opening the pipe is the only probe that works.
fn ipc_endpoint_live(sock: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        sock.exists() && socket_is_live(sock)
    }
    #[cfg(not(unix))]
    {
        socket_is_live(sock)
    }
}

// ── Dev/prod peer handoff ────────────────────────────────────────────────
//
// Two instances can coexist (`Always.app` prod, `Always Dev.app` dev) but
// the machine has ONE microphone. Production wins automatic startup
// arbitration: when Dev starts beside a live Prod daemon, Dev pauses itself
// without changing Prod's user preference. When Prod starts beside Dev, it
// pauses Dev over the peer's UDS socket (`SetPaused{reason:"peer-instance"}`).
// An explicit Dev resume requests a confirmed pause of Prod before taking
// over. A paused-by-peer daemon watches its peer socket and resumes when the
// peer dies; graceful exits also release a peer this process paused.

/// The OTHER instance's UDS socket: prod↔dev.
pub fn peer_socket_path() -> Option<PathBuf> {
    // Only the prod↔dev pair hands off. Any other instance (a sandbox such
    // as the launch smoke test's `ALWAYS_INSTANCE=smoke`) must never pause
    // or resume the user's daemons.
    let file = match config::instance() {
        "prod" => "always-dev.sock",
        "dev" => "always.sock",
        _ => return None,
    };
    #[cfg(target_os = "macos")]
    {
        std::env::var("HOME").ok().map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Caches")
                .join("Always")
                .join(file)
        })
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            Some(PathBuf::from(runtime_dir).join(file))
        } else {
            Some(PathBuf::from("/tmp").join(file))
        }
    }
    #[cfg(target_os = "windows")]
    {
        let pipe = match config::instance() {
            "prod" => r"\\.\pipe\always-dev",
            "dev" => r"\\.\pipe\always",
            _ => return None,
        };
        let _ = file;
        Some(PathBuf::from(pipe))
    }
}

/// True when the peer instance's daemon socket accepts a connection.
pub fn peer_is_live() -> bool {
    peer_socket_path().is_some_and(|p| ipc_endpoint_live(&p))
}

static WE_PAUSED_PEER_LEASE: std::sync::LazyLock<parking_lot::Mutex<Option<String>>> =
    std::sync::LazyLock::new(parking_lot::Mutex::default);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupPeerAction {
    None,
    PauseSelf,
    PausePeer,
}

fn startup_peer_action(instance: &str, peer_live: bool) -> StartupPeerAction {
    if !peer_live {
        return StartupPeerAction::None;
    }
    match instance {
        // Dev is a passive candidate when production is already running.
        // Never let a development launch change the user's production pause state.
        "dev" => StartupPeerAction::PauseSelf,
        "prod" => StartupPeerAction::PausePeer,
        _ => StartupPeerAction::None,
    }
}

fn startup_peer_action_when_ready(
    instance: &str,
    peer_live: bool,
    already_paused_by_peer: bool,
) -> StartupPeerAction {
    if already_paused_by_peer {
        StartupPeerAction::None
    } else {
        startup_peer_action(instance, peer_live)
    }
}

fn startup_peer_action_after_peer_pause(
    action: StartupPeerAction,
    peer_pause_confirmed: bool,
) -> StartupPeerAction {
    if action == StartupPeerAction::PausePeer && !peer_pause_confirmed {
        // A newer production daemon must not take the microphone from an
        // older Dev daemon that cannot negotiate a conditional pause lease.
        StartupPeerAction::PauseSelf
    } else {
        action
    }
}

/// Send an ownership-tagged pause request only after negotiating the peer's
/// conditional lease capability. Legacy daemons are never mutated here.
pub fn set_peer_paused(paused: bool) -> bool {
    let Some(sock) = peer_socket_path() else {
        return false;
    };
    if !ipc_endpoint_live(&sock) {
        return false;
    }
    if paused {
        let request_id = WE_PAUSED_PEER_LEASE
            .lock()
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let response = transact_peer_pause_lease(&sock, &request_id, true);
        return record_peer_pause_attempt(&mut WE_PAUSED_PEER_LEASE.lock(), &request_id, response);
    }

    let Some(request_id) = WE_PAUSED_PEER_LEASE.lock().clone() else {
        return false;
    };
    let accepted =
        transact_peer_pause_lease(&sock, &request_id, false).is_some_and(|(accepted, _)| accepted);
    if accepted {
        let mut lease = WE_PAUSED_PEER_LEASE.lock();
        if lease.as_deref() == Some(&request_id) {
            *lease = None;
        }
    }
    accepted
}

fn record_peer_pause_attempt(
    lease: &mut Option<String>,
    request_id: &str,
    response: Option<(bool, bool)>,
) -> bool {
    match response {
        // The command may have reached the peer even though its correlated
        // acknowledgement did not. Keep the same id so a retry is idempotent;
        // never unpause ourselves on this uncertain outcome.
        None => {
            *lease = Some(request_id.to_string());
            false
        }
        Some((true, true)) => {
            *lease = Some(request_id.to_string());
            true
        }
        // A manual pause is safe for takeover, but it is not ours to release.
        Some((true, false)) => {
            *lease = None;
            true
        }
        Some((false, _)) => {
            *lease = None;
            false
        }
    }
}

fn peer_pause_lease_command(request_id: &str, paused: bool) -> Option<String> {
    serde_json::to_string(&serde_json::json!({
        "type": "PeerPauseLease",
        "data": {"request_id": request_id, "paused": paused}
    }))
    .ok()
}

fn peer_supports_pause_lease(line: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    value.get("type").and_then(serde_json::Value::as_str) == Some("Hello")
        && value
            .get("data")
            .and_then(|data| data.get("capabilities"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|capabilities| {
                capabilities.iter().any(|capability| {
                    capability.as_str() == Some(crate::always::event::PEER_PAUSE_LEASE_CAPABILITY)
                })
            })
}

fn peer_pause_lease_result(line: &str, request_id: &str) -> Option<(bool, bool)> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    if value.get("type").and_then(serde_json::Value::as_str) != Some("PeerPauseLeaseResult") {
        return None;
    }
    let data = value.get("data")?;
    if data.get("request_id").and_then(serde_json::Value::as_str) != Some(request_id) {
        return None;
    }
    Some((
        data.get("accepted")?.as_bool()?,
        data.get("lease_owned")?.as_bool()?,
    ))
}

const PEER_PAUSE_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const PEER_PAUSE_MAX_HANDSHAKE_LINES: usize = 256;
const PEER_PAUSE_MAX_LINE_BYTES: usize = 64 * 1024;

fn transact_peer_pause_lease(
    path: &std::path::Path,
    request_id: &str,
    paused: bool,
) -> Option<(bool, bool)> {
    #[cfg(unix)]
    {
        transact_peer_pause_lease_unix(path, request_id, paused)
    }
    #[cfg(windows)]
    {
        transact_peer_pause_lease_windows(path, request_id, paused)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, request_id, paused);
        None
    }
}

#[cfg(unix)]
fn transact_peer_pause_lease_unix(
    path: &std::path::Path,
    request_id: &str,
    paused: bool,
) -> Option<(bool, bool)> {
    use std::io::Write as _;
    let deadline = Instant::now() + PEER_PAUSE_REQUEST_TIMEOUT;
    let stream = std::os::unix::net::UnixStream::connect(path).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .ok()?;
    let mut reader = std::io::BufReader::new(stream);
    let mut line = String::new();
    if !read_peer_line_until_unix(&mut reader, &mut line, deadline).ok()?
        || !peer_supports_pause_lease(&line)
    {
        return None;
    }
    let command = peer_pause_lease_command(request_id, paused)?;
    {
        let stream = reader.get_mut();
        stream.write_all(command.as_bytes()).ok()?;
        stream.write_all(b"\n").ok()?;
    }
    line.clear();
    let mut frames = 0;
    while frames < PEER_PAUSE_MAX_HANDSHAKE_LINES && Instant::now() < deadline {
        if !read_peer_line_until_unix(&mut reader, &mut line, deadline).ok()? {
            return None;
        }
        if line.len() > PEER_PAUSE_MAX_LINE_BYTES {
            return None;
        }
        frames += 1;
        if let Some(result) = peer_pause_lease_result(&line, request_id) {
            return Some(result);
        }
        line.clear();
    }
    None
}

#[cfg(unix)]
fn read_peer_line_until_unix<R: std::io::Read>(
    reader: &mut std::io::BufReader<R>,
    line: &mut String,
    deadline: Instant,
) -> std::io::Result<bool> {
    use std::io::BufRead as _;
    while Instant::now() < deadline {
        match reader.read_line(line) {
            Ok(0) => return Ok(false),
            Ok(_) => return Ok(true),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "peer pause handshake timed out",
    ))
}

#[cfg(windows)]
fn transact_peer_pause_lease_windows(
    path: &std::path::Path,
    request_id: &str,
    paused: bool,
) -> Option<(bool, bool)> {
    let deadline = Instant::now() + PEER_PAUSE_REQUEST_TIMEOUT;
    let mut pipe =
        crate::always::peer_transport_windows::connect(path, PEER_PAUSE_REQUEST_TIMEOUT).ok()?;
    let hello = pipe.read_line_until(deadline).ok()??;
    if !peer_supports_pause_lease(&hello) {
        return None;
    }
    let mut command = peer_pause_lease_command(request_id, paused)?;
    command.push('\n');
    pipe.write_all(command.as_bytes(), deadline).ok()?;
    for _ in 0..PEER_PAUSE_MAX_HANDSHAKE_LINES {
        let line = pipe.read_line_until(deadline).ok()??;
        if line.len() > PEER_PAUSE_MAX_LINE_BYTES {
            return None;
        }
        if let Some(result) = peer_pause_lease_result(&line, request_id) {
            return Some(result);
        }
    }
    None
}

/// Apply startup arbitration without letting a Dev launch pause production.
/// Dev's self-standby is applied immediately, before transcriber startup;
/// peer requests are sent only after our socket is live so the peer's
/// resume watchdog will not immediately undo the handoff.
pub fn pause_peer_when_ready() {
    // A dev launch must not take microphone ownership from production. Pause
    // dev immediately when prod is already live, before transcriber startup;
    // its watchdog will resume dev if production exits. Prod retains the
    // existing handoff behavior when it starts after dev.
    let pause_self =
        startup_peer_action(config::instance(), peer_is_live()) == StartupPeerAction::PauseSelf;
    if pause_self {
        pause::set_paused_by_peer(true);
        let (effective, changed) = pause::set_paused(true);
        event::global_broadcaster().master_pause_changed(true);
        if changed {
            if effective {
                pause::dictation_buffer_clear();
                event::global_broadcaster().paused();
            } else {
                event::global_broadcaster().resumed();
            }
        }
        tracing::info!("instance_stayed_passive_for_peer");
    }

    std::thread::spawn(|| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if socket_path().is_some_and(|s| ipc_endpoint_live(&s)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if pause::is_paused_by_peer() {
            spawn_peer_resume_watchdog();
        } else {
            let action = startup_peer_action_when_ready(
                config::instance(),
                peer_is_live(),
                pause::is_paused_by_peer(),
            );
            let action = if action == StartupPeerAction::PausePeer {
                startup_peer_action_after_peer_pause(action, set_peer_paused(true))
            } else {
                action
            };
            match action {
                StartupPeerAction::PauseSelf => {
                    pause::set_paused_by_peer(true);
                    let (effective, changed) = pause::set_paused(true);
                    event::global_broadcaster().master_pause_changed(true);
                    if changed {
                        if effective {
                            pause::dictation_buffer_clear();
                            event::global_broadcaster().paused();
                        } else {
                            event::global_broadcaster().resumed();
                        }
                    }
                    spawn_peer_resume_watchdog();
                    tracing::info!("instance_stayed_passive_for_peer");
                }
                StartupPeerAction::PausePeer => {
                    tracing::info!("peer_instance_paused_on_startup");
                }
                StartupPeerAction::None => {}
            }
        }
    });
}

/// Graceful-exit counterpart: release only the lease we acquired and only
/// through a correlated peer acknowledgement.
pub fn resume_peer_on_exit() {
    if WE_PAUSED_PEER_LEASE.lock().is_some() && set_peer_paused(false) {
        tracing::info!("peer_instance_resumed_on_exit");
    }
}

static PEER_WATCHDOG_RUNNING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// While paused-by-peer, poll the peer's socket once a second; when the
/// peer daemon dies, resume ourselves. Covers the kill -9 path where the
/// peer's `resume_peer_on_exit` never runs. Single-flight.
pub fn spawn_peer_resume_watchdog() {
    use std::sync::atomic::Ordering;
    if PEER_WATCHDOG_RUNNING.swap(true, Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(Duration::from_secs(1));
            // A user/any other pause source cleared the flag — we no
            // longer own this pause, stop watching.
            if !crate::always::pause::is_paused_by_peer() {
                break;
            }
            if !peer_is_live() {
                if crate::always::pause::take_paused_by_peer() {
                    let (_effective, changed) = crate::always::pause::set_paused(false);
                    crate::always::event::global_broadcaster().master_pause_changed(false);
                    if changed {
                        crate::always::event::global_broadcaster().resumed();
                    }
                    tracing::info!("peer_daemon_gone_auto_resumed");
                }
                break;
            }
        }
        PEER_WATCHDOG_RUNNING.store(false, Ordering::Relaxed);
    });
}

/// Current physical footprint of THIS process, in bytes — the same
/// number `vmmap --summary` prints as "Physical footprint".
/// Only available on macOS (via `proc_pid_rusage`).
#[cfg(target_os = "macos")]
pub fn resident_footprint_bytes() -> Option<u64> {
    use std::ffi::{c_int, c_void};

    #[repr(C, align(8))]
    struct RusageInfoV0 {
        _uuid: [u8; 16],
        _user_time: u64,
        _system_time: u64,
        _pkg_idle_wkups: u64,
        _interrupt_wkups: u64,
        _pgins: u64,
        _wired_count: u64,
        _resident_count: u64,
        phys_footprint: u64,
        _proc_start: u64,
        _proc_exit: u64,
    }

    const RUSAGE_INFO_V0: c_int = 0;

    unsafe extern "C" {
        // libproc is part of libSystem — no extra link directive needed
        // (same link posture as `proc_pidpath` in mic_monitor.rs).
        fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut c_void) -> c_int;
    }

    let mut info = RusageInfoV0 {
        _uuid: [0; 16],
        _user_time: 0,
        _system_time: 0,
        _pkg_idle_wkups: 0,
        _interrupt_wkups: 0,
        _pgins: 0,
        _wired_count: 0,
        _resident_count: 0,
        phys_footprint: 0,
        _proc_start: 0,
        _proc_exit: 0,
    };
    let status = unsafe {
        proc_pid_rusage(
            std::process::id() as c_int,
            RUSAGE_INFO_V0,
            (&raw mut info).cast(),
        )
    };
    (status == 0).then_some(info.phys_footprint)
}

/// Linux fallback: read `/proc/self/status` for `VmRSS`.
#[cfg(not(target_os = "macos"))]
pub fn resident_footprint_bytes() -> Option<u64> {
    use std::io::Read;
    let mut buf = String::new();
    if std::fs::File::open("/proc/self/status")
        .ok()?
        .read_to_string(&mut buf)
        .is_err()
    {
        return None;
    }
    buf.lines()
        .find(|l| l.starts_with("VmRSS:"))
        .and_then(|l| {
            l.split_whitespace()
                .nth(1)
                .unwrap_or("0")
                .parse::<u64>()
                .ok()
        })
        .map(|kb| kb * 1024)
}

/// True when the daemon should exit and let the GUI respawn it: the
/// physical footprint crossed [`RSS_CEILING_BYTES`] while the daemon is
/// not actively listening (effective-paused, or no voice for at least
/// [`SELF_RESTART_IDLE_GRACE_SECS`]). Never restarts mid-utterance.
///
/// Why this exists (2026-09-29): ONNX Runtime 2.0.0-rc.12 on this macOS
/// build retains a slice of every inference's working buffers at the
/// process level for the daemon's lifetime — measured live: 0.7 GB at
/// launch → 4.2 GB within the hour while the user dictated (tens of GB
/// per working day, matching the "18 GB → 30 GB" report; the leaked
/// blocks are `std::shared_ptr_pointer<void const**>` nodes visible in
/// `heap`). Session recycling and the arena/pattern knobs do NOT bound
/// it (fresh sessions still retain, and drops don't reclaim — see the
/// leak-probe history in vad_silero.rs). A ceiling restart converts the
/// unbounded growth into a bounded sawtooth: the daemon exits while
/// idle, the GUI respawns it within seconds, and the mic handoff
/// already covers the transition.
pub fn should_self_restart(footprint: Option<u64>, idle_secs: u64, effective_paused: bool) -> bool {
    const IDLE_GRACE_SECS: u64 = 60;
    match footprint {
        Some(bytes) if bytes > RSS_CEILING_BYTES => {
            effective_paused || idle_secs >= IDLE_GRACE_SECS
        }
        _ => false,
    }
}

/// Physical-footprint ceiling that triggers the self-restart. Well above
/// any legitimate steady state (the heaviest documented profile — local
/// STT model loaded — is ~0.9 GB; the leak-free Groq backend sits under
/// 0.5 GB), and low enough that a dictation-heavy day restarts the
/// daemon a handful of times instead of ever approaching the
/// tens-of-GB regime the user reported.
pub const RSS_CEILING_BYTES: u64 = 3 * 1024 * 1024 * 1024;

pub fn start(cfg: &AlwaysConfig) -> Result<()> {
    let socket_live = socket_path().is_some_and(|sock| sock.exists() && socket_is_live(&sock));
    if is_running() || socket_live {
        return report_existing_daemon();
    }
    let existing = list_daemon_pids();
    if !existing.is_empty() {
        anyhow::bail!(
            "Always-on daemon process exists (pids: {:?}), but its build identity is unknown. {}",
            existing,
            refresh_guidance()
        );
    }

    let skip_orphan_kill = std::env::var("ALWAYS_SKIP_ORPHAN_KILL")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if skip_orphan_kill {
        remove_stale_pid();
        remove_stale_socket();
    } else {
        // Kill any existing daemons immediately before starting a new one
        // This prevents duplicate daemons from running in parallel
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
    }

    let socket_live = socket_path().is_some_and(|sock| sock.exists() && socket_is_live(&sock));
    if is_running() || socket_live {
        return report_existing_daemon();
    }
    let existing = list_daemon_pids();
    if !existing.is_empty() {
        anyhow::bail!(
            "Always-on daemon process exists (pids: {:?}), but its build identity is unknown. {}",
            existing,
            refresh_guidance()
        );
    }

    let exe = std::env::current_exe().context("cannot find always binary")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.args([
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
    .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        // Detach fully: a child that inherits the console dies when the
        // terminal that ran `always start` closes, taking the daemon
        // with it.
        use std::os::windows::process::CommandExt as _;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
    let child = cmd
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
    let daemon_pids = list_daemon_pids();
    if let Some(pid) = read_pid().filter(|pid| pid_is_daemon(*pid, &daemon_pids)) {
        terminate_gracefully(pid);
        remove_build_id_for_pid(pid);
        println!("Always-on daemon stopped (pid {pid}).");
    } else if read_pid().is_some() {
        println!("Always-on daemon PID identity is unknown; leaving that process untouched.");
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
    let recorded_pid = read_pid();
    let recorded_pid_is_daemon = recorded_pid.is_some_and(|pid| pids.contains(&pid));
    let socket_live = socket_path().is_some_and(|sock| ipc_endpoint_live(&sock));
    if daemon_is_running(recorded_pid_is_daemon, socket_live) {
        if let Some(pid) = recorded_pid.filter(|_| recorded_pid_is_daemon) {
            println!("Always-on daemon running (pid {pid})");
            match current_build_freshness() {
                BuildFreshness::Fresh => println!("Build freshness: current."),
                BuildFreshness::Stale { running, current } => println!(
                    "WARNING: daemon build is stale (running {running}; current executable {current}). {}",
                    refresh_guidance()
                ),
                BuildFreshness::Unknown => println!(
                    "WARNING: daemon build identity is unknown. {}",
                    refresh_guidance()
                ),
            }
            println!("Log: {}", always_config::configured_log_path()?.display());
        } else {
            println!("Always-on daemon running (PID unknown)");
            println!(
                "WARNING: daemon build identity is unknown. {}",
                refresh_guidance()
            );
            println!("Log: {}", always_config::configured_log_path()?.display());
        }
    } else {
        remove_stale_pid();
        println!("Always-on daemon not running.");
    }
    // Notify-only (SPEC §12a): what the last background check found.
    if let Some(line) = crate::always::updater::status_line() {
        println!("{line}");
    }
    Ok(())
}

pub struct PidGuard {
    _lock: File,
    /// Windows single-instance lock — the named-mutex counterpart of the
    /// pid-file `flock` used on Unix.
    #[cfg(windows)]
    _instance_mutex: crate::always::win32_process::InstanceMutex,
}

impl PidGuard {
    pub fn install() -> Result<Self> {
        // Refuse-to-start short-circuit: if a peer is already up and
        // accepting on the UDS socket, bail without touching anything.
        if let Some(sock) = socket_path()
            && socket_is_live(&sock)
        {
            anyhow::bail!(
                "Always-on daemon already running (UDS socket live) — refusing to start a second instance"
            );
        }

        let skip_orphan_kill = std::env::var("ALWAYS_SKIP_ORPHAN_KILL")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        // Acquire the flock BEFORE touching other daemons. Previous order
        // (kill orphans → take flock) had a fatal race: if daemon1 was
        // mid-startup (loading a multi-second local model and not yet
        // bound to the socket), daemon2's "socket isn't live" check
        // passed, the orphan kill murdered daemon1, and daemon2 stole the
        // role. Now: whoever holds the flock IS the daemon; anyone
        // arriving second sees the lock and bails. Only then do we
        // consider sweeping zombies (which by definition no longer hold
        // the lock).
        let path = pid_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        // Open WITHOUT truncating: zeroing the file before we own the lock
        // would make a live daemon's PID briefly read as empty (read_pid →
        // None → "looks dead"). We truncate + write our PID only after the
        // flock is held AND every post-lock recheck has passed (below).
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .context("failed to open always PID file")?;
        #[cfg(unix)]
        acquire_exclusive_lock(&file)
            .context("Always-on daemon already running (pid lock held)")?;
        #[cfg(windows)]
        let instance_mutex = crate::always::win32_process::InstanceMutex::acquire()
            .context("Always-on daemon already running (instance mutex held)")?;

        // Post-lock recheck: if a peer beat us to binding the socket
        // between our short-circuit probe and the flock acquire (it
        // would have had to crash with the lock held — flock would block
        // us otherwise), bail rather than racing the bind. We have not
        // written our PID yet, but `open` with create(true) may have
        // created a fresh 0-byte file; dropping `file` only closes the fd
        // and does NOT unlink it, so remove it explicitly to avoid leaking
        // an empty PID file that the next `start` would read as "no daemon".
        if let Some(sock) = socket_path()
            && socket_is_live(&sock)
        {
            let _ = std::fs::remove_file(&path);
            anyhow::bail!(
                "Another always daemon bound the UDS socket during startup — refusing to double-bind"
            );
        }

        // Now that we own the lock, it's safe to sweep zombies — any
        // process listed here CANNOT be the canonical daemon (we hold
        // its flock) so killing it can't murder a live peer.
        if !skip_orphan_kill {
            let others: Vec<u32> = list_daemon_pids()
                .into_iter()
                .filter(|pid| *pid != std::process::id())
                .collect();
            if !others.is_empty() {
                tracing::warn!(
                    pids = ?others,
                    "reaping daemon processes that don't hold the PID lock"
                );
                kill_orphan_daemon_processes_except(std::process::id());
                let _ = wait_until_no_daemon_processes(Duration::from_secs(5));
            }
        }
        remove_stale_socket();

        // We hold the lock and no live daemon exists, so this file is ours.
        // Truncate any stale contents now (we deliberately did NOT truncate at
        // open time) and write our PID. If any step fails partway, unlink the
        // file rather than leak a 0-byte / partial PID that the next `start`
        // would parse as "no daemon" and duplicate the mic.
        use std::io::{Seek, SeekFrom};
        let write_result = file
            .set_len(0)
            .and_then(|()| file.seek(SeekFrom::Start(0)).map(|_| ()))
            .and_then(|()| write!(file, "{}", std::process::id()));
        if let Err(e) = write_result {
            let _ = std::fs::remove_file(&path);
            return Err(e).context("failed to write always PID file");
        }
        let pid_file_mtime = pid_file_mtime_nanos()
            .context("cannot read always PID file timestamp for daemon identity")?;
        let build_id_record = format!(
            "{}\n{}\n{}",
            std::process::id(),
            pid_file_mtime,
            crate::always::event::BUILD_ID
        );
        if let Err(e) = std::fs::write(build_id_path(), build_id_record) {
            let _ = std::fs::remove_file(&path);
            return Err(e).context("failed to write always daemon build identity");
        }
        let count = list_daemon_pids();
        if count.len() > 1 {
            tracing::error!(
                pids = ?count,
                "multiple daemon processes after PidGuard::install — duplicate transcripts likely"
            );
        }
        Ok(Self {
            _lock: file,
            #[cfg(windows)]
            _instance_mutex: instance_mutex,
        })
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = self._lock.as_raw_fd();
            unsafe {
                libc::flock(fd, libc::LOCK_UN);
            }
        }
        // Windows: `_instance_mutex` releases the named mutex on drop;
        // `_lock` was only ever the pid file (no flock equivalent needed).
        if let Ok(log_path) = always_config::configured_log_path()
            && let Ok(mut log) = crate::always::log::Logger::open(&log_path)
        {
            log.write(crate::always::log::Event::Stop);
        }
        remove_build_id_for_pid(std::process::id());
        let _ = std::fs::remove_file(pid_path());
        if let Some(sock) = socket_path() {
            let _ = std::fs::remove_file(sock);
        }
        resume_peer_on_exit();
    }
}

#[cfg(unix)]
fn acquire_exclusive_lock(file: &File) -> Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(());
    }
    anyhow::bail!("pid file flock failed (rc={rc})")
}

fn remove_stale_pid() {
    if let Some(pid) = read_pid()
        && !process_is_running(pid)
    {
        remove_build_id_for_pid(pid);
        let _ = std::fs::remove_file(pid_path());
    }
}

fn remove_stale_socket() {
    if read_pid().is_some_and(process_is_running) {
        return;
    }
    if let Some(sock) = socket_path() {
        if ipc_endpoint_live(&sock) {
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
    #[cfg(unix)]
    {
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
            remove_build_id_for_pid(std::process::id());
            let _ = std::fs::remove_file(pid_path());
            if let Some(sock) = socket_path() {
                let _ = std::fs::remove_file(sock);
            }
            resume_peer_on_exit();
            std::process::exit(0);
        });
    }
    #[cfg(windows)]
    {
        // No signals on Windows, but `always run` in a console can still
        // be stopped gracefully with Ctrl+C — do the same cleanup the
        // Unix handlers do. A daemon spawned via `always start` is
        // console-less and simply never sees the event.
        handle.spawn(async move {
            match tokio::signal::ctrl_c().await {
                Ok(()) => tracing::info!("received Ctrl+C, shutting down"),
                Err(e) => {
                    tracing::warn!(error = %e, "failed_to_listen_for_ctrl_c");
                    return;
                }
            }
            if let Ok(log_path) = always_config::configured_log_path()
                && let Ok(mut log) = crate::always::log::Logger::open(&log_path)
            {
                log.write(crate::always::log::Event::Stop);
            }
            remove_build_id_for_pid(std::process::id());
            let _ = std::fs::remove_file(pid_path());
            if let Some(sock) = socket_path() {
                let _ = std::fs::remove_file(sock);
            }
            resume_peer_on_exit();
            std::process::exit(0);
        });
    }
}

#[cfg(unix)]
fn process_is_running(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .output()
        .is_ok_and(|output| output.status.success())
}

#[cfg(windows)]
fn process_is_running(pid: u32) -> bool {
    crate::always::win32_process::process_is_running(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_pause_lease_requires_explicit_capability_negotiation() {
        let capable = serde_json::json!({
            "type": "Hello",
            "data": {"version": 12, "capabilities": [event::PEER_PAUSE_LEASE_CAPABILITY]}
        })
        .to_string();
        let legacy = serde_json::json!({"type": "Hello", "data": {"version": 12}}).to_string();

        assert!(peer_supports_pause_lease(&capable));
        assert!(!peer_supports_pause_lease(&legacy));
        assert!(!peer_supports_pause_lease(r#"{"type":"ListeningStarted"}"#));
    }

    #[test]
    fn peer_pause_lease_ack_must_match_request_id_and_schema() {
        let matching = serde_json::json!({
            "type": "PeerPauseLeaseResult",
            "data": {"request_id": "request-a", "accepted": true, "lease_owned": true}
        })
        .to_string();
        let stale = matching.replace("request-a", "request-b");
        let malformed = serde_json::json!({
            "type": "PeerPauseLeaseResult",
            "data": {"request_id": "request-a", "accepted": true}
        })
        .to_string();

        assert_eq!(
            peer_pause_lease_result(&matching, "request-a"),
            Some((true, true))
        );
        assert_eq!(peer_pause_lease_result(&stale, "request-a"), None);
        assert_eq!(peer_pause_lease_result(&malformed, "request-a"), None);
    }

    #[test]
    fn lost_pause_ack_keeps_a_retryable_id_without_claiming_success() {
        let mut lease = None;
        assert!(!record_peer_pause_attempt(&mut lease, "request-a", None));
        assert_eq!(lease.as_deref(), Some("request-a"));

        // A later matching response that says the peer is manually paused
        // permits safe takeover but drops the uncertain release authority.
        assert!(record_peer_pause_attempt(
            &mut lease,
            "request-a",
            Some((true, false))
        ));
        assert_eq!(lease, None);
    }

    #[test]
    fn dev_startup_never_pauses_a_live_production_peer() {
        assert_eq!(
            startup_peer_action("dev", true),
            StartupPeerAction::PauseSelf
        );
        assert_eq!(startup_peer_action("dev", false), StartupPeerAction::None);
        assert_eq!(
            startup_peer_action("prod", true),
            StartupPeerAction::PausePeer
        );
        assert_eq!(startup_peer_action("prod", false), StartupPeerAction::None);
        assert_eq!(startup_peer_action("smoke", true), StartupPeerAction::None);
    }

    #[test]
    fn dev_startup_rechecks_prod_after_its_socket_becomes_ready() {
        let initial = startup_peer_action("dev", false);
        assert_eq!(initial, StartupPeerAction::None);

        // Prod can appear after Dev's initial liveness check but before Dev's
        // IPC socket is ready. Readiness must make the same safe decision.
        assert_eq!(
            startup_peer_action_when_ready("dev", true, false),
            StartupPeerAction::PauseSelf
        );
        assert_eq!(
            startup_peer_action_when_ready("dev", true, true),
            StartupPeerAction::None
        );
    }

    #[test]
    fn prod_startup_stays_passive_when_legacy_dev_cannot_confirm_pause() {
        let legacy_hello = r#"{"type":"Hello","data":{"version":12}}"#;
        let can_confirm_pause = peer_supports_pause_lease(legacy_hello);
        let initial_action = startup_peer_action("prod", true);

        assert_eq!(initial_action, StartupPeerAction::PausePeer);
        assert_eq!(
            startup_peer_action_after_peer_pause(initial_action, can_confirm_pause),
            StartupPeerAction::PauseSelf
        );
    }

    /// The 2026-09-29 leak safety net: the restart fires only when the
    /// footprint is past the ceiling AND the daemon is not actively
    /// listening (effective-paused, or no voice for the grace period).
    #[test]
    fn self_restart_needs_ceiling_and_quiet() {
        let ceiling = RSS_CEILING_BYTES;
        // Under the ceiling: never restart, whatever the pause state.
        assert!(!should_self_restart(Some(ceiling), 0, true));
        assert!(!should_self_restart(Some(ceiling - 1), 10_000, true));
        // Over the ceiling but actively listening (not paused, voice
        // seconds ago): never restart mid-utterance.
        assert!(!should_self_restart(Some(ceiling + 1), 0, false));
        assert!(!should_self_restart(Some(ceiling * 10), 59, false));
        // Over the ceiling and quiet: restart.
        assert!(should_self_restart(Some(ceiling + 1), 60, false));
        assert!(should_self_restart(Some(ceiling * 10), 5, true));
        // A missing reading must never restart.
        assert!(!should_self_restart(None, 10_000, true));
    }

    #[test]
    fn build_freshness_distinguishes_matching_stale_and_unknown_daemons() {
        assert_eq!(
            build_freshness("current-build", Some("current-build")),
            BuildFreshness::Fresh
        );
        assert_eq!(
            build_freshness("current-build", Some("older-build")),
            BuildFreshness::Stale {
                running: "older-build".into(),
                current: "current-build".into(),
            }
        );
        assert_eq!(
            build_freshness("current-build", None),
            BuildFreshness::Unknown
        );
    }

    #[test]
    fn recorded_build_id_must_match_the_running_pid_and_pid_file() {
        assert_eq!(
            build_id_for_process("42\n100\nbuild-a", 42, 100, true),
            Some("build-a".into())
        );
        assert_eq!(
            build_id_for_process("42\n100\nbuild-a", 42, 200, true),
            None
        );
        assert_eq!(
            build_id_for_process("42\n100\nbuild-a", 43, 100, true),
            None
        );
        assert_eq!(build_id_for_process("", 42, 100, true), None);
        assert_eq!(
            build_id_for_process("42\nnot-a-time\nbuild-a", 42, 100, true),
            None
        );
        assert_eq!(build_id_for_process("42\n100\n", 42, 100, true), None);
        assert_eq!(
            build_id_for_process("42\n100\nbuild-a", 42, 100, false),
            None,
            "a reused PID owned by an unrelated process cannot inherit a stale build marker"
        );
    }

    #[test]
    fn old_daemon_cannot_inherit_a_newer_process_build_marker() {
        // Build B stopped but left a marker; legacy build A starts and
        // rewrites the PID file. The PID may even be reused, so the file's
        // modification token must match too.
        let marker = "42\n100\nbuild-b";
        let running_id = build_id_for_process(marker, 42, 200, true);
        assert_eq!(
            build_freshness("build-b", running_id.as_deref()),
            BuildFreshness::Unknown
        );
    }

    #[test]
    fn live_socket_counts_as_running_when_pid_record_is_missing() {
        assert!(daemon_is_running(false, true));
        assert!(daemon_is_running(true, false));
        assert!(!daemon_is_running(false, false));
    }

    #[test]
    fn bare_run_command_line_is_a_daemon() {
        // The Linux systemd unit's `ExecStart=always run`, as `ps` prints it.
        let exe = "/home/u/.local/bin/always";
        assert!(daemon_process_matches(
            "prod",
            exe,
            exe,
            &format!("{exe} run")
        ));
        assert!(daemon_process_matches(
            "smoke",
            exe,
            exe,
            &format!("{exe} run")
        ));
        assert!(!daemon_process_matches(
            "prod",
            exe,
            exe,
            &format!("{exe} status")
        ));
        assert!(!daemon_process_matches(
            "prod",
            exe,
            exe,
            &format!("{exe} runner")
        ));
        assert_eq!(first_argv("run"), Some(("run", "")));
    }

    #[test]
    fn daemon_instance_identity_uses_executable_path_not_argument_text() {
        let prod_exe = "/Applications/Always.app/Contents/MacOS/always-daemon";
        let dev_exe = "/Applications/Always Dev.app/Contents/MacOS/always-daemon";
        let prod = format!("{prod_exe} run --label 'Always Dev.app'");
        assert!(daemon_process_matches("prod", prod_exe, prod_exe, &prod));
        assert!(!daemon_process_matches("dev", dev_exe, prod_exe, &prod));
        // macOS `ps -o args` emits this live-style path unquoted even though
        // the app bundle name contains spaces; the kernel image path anchors
        // the executable boundary before parsing the `run` subcommand.
        let dev_command = format!("{dev_exe} run --lang auto");
        assert!(daemon_process_matches(
            "dev",
            dev_exe,
            dev_exe,
            &dev_command
        ));
        assert!(!daemon_process_matches(
            "prod",
            prod_exe,
            dev_exe,
            &dev_command
        ));
        assert!(!daemon_process_matches(
            "dev",
            dev_exe,
            "/tmp/always-daemon",
            "/tmp/always-daemon run --label 'Always Dev.app'"
        ));
    }

    #[test]
    fn recorded_pid_is_owned_only_when_process_identity_matches() {
        assert!(pid_is_daemon(42, &[7, 42]));
        assert!(!pid_is_daemon(42, &[7, 43]));
        assert!(!pid_is_daemon(42, &[]));
    }

    /// The live-footprint reader works on this platform and returns a
    /// plausible magnitude (tens of MB to tens of GB, never 0 or wild).
    #[cfg(target_os = "macos")]
    #[test]
    fn resident_footprint_is_plausible() {
        let fp = resident_footprint_bytes().expect("proc_pid_rusage works");
        assert!(
            fp > 1_000_000 && fp < 64 * 1024 * 1024 * 1024,
            "implausible footprint: {fp} bytes"
        );
    }

    #[test]
    fn list_daemon_pids_dedupes() {
        let pids = list_daemon_pids();
        let mut sorted = pids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(pids.len(), sorted.len());
    }

    #[test]
    fn sandbox_instance_never_matches_prod_or_dev() {
        let prod_exe = "/Applications/Always.app/Contents/MacOS/always-daemon";
        let dev_exe = "/Applications/Always Dev.app/Contents/MacOS/always-daemon";
        let smoke_exe = "/tmp/x/AlwaysSmoke.app/Contents/MacOS/always-daemon";
        assert!(
            !same_instance_for("smoke", smoke_exe, prod_exe),
            "sandbox must not reap prod"
        );
        assert!(!same_instance_for("smoke", smoke_exe, dev_exe));
        assert!(same_instance_for("smoke", smoke_exe, smoke_exe));
        assert!(
            !same_instance_for("prod", prod_exe, smoke_exe),
            "prod must not reap a sandbox"
        );
        assert!(same_instance_for("prod", prod_exe, prod_exe));
        assert!(same_instance_for("dev", dev_exe, dev_exe));
        assert!(!same_instance_for("dev", dev_exe, prod_exe));
    }

    #[test]
    fn daemon_run_matcher_rejects_diagnostic_commands() {
        let app_exe = "/Applications/Always.app/Contents/MacOS/always-daemon";
        let local_exe = "/Users/livio/Documents/always/target/release/always";
        assert!(daemon_process_matches(
            "prod",
            app_exe,
            app_exe,
            &format!("{app_exe} run --lang en")
        ));
        assert!(daemon_process_matches(
            "prod",
            local_exe,
            local_exe,
            &format!("{local_exe} run --timeout 30")
        ));
        assert!(!daemon_process_matches(
            "prod",
            app_exe,
            "/bin/zsh",
            "/bin/zsh -lc ps aux | rg \"(/Applications/Always.app|always run|always-daemon)\""
        ));
        assert!(!daemon_process_matches(
            "prod",
            app_exe,
            "/usr/bin/rg",
            "rg (/Applications/Always.app|always run|always-daemon)"
        ));
        assert!(!daemon_process_matches(
            "prod",
            app_exe,
            app_exe,
            &format!("{app_exe} status")
        ));
        // Test binaries run as the prod instance: dev-bundle commands must
        // be foreign so prod never reconciles/kills the dev daemon.
        let dev_exe = "/Applications/Always Dev.app/Contents/MacOS/always-daemon";
        assert!(!daemon_process_matches(
            "prod",
            app_exe,
            dev_exe,
            &format!("{dev_exe} run --lang en")
        ));
    }
}
