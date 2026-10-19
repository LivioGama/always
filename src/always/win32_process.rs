//! Windows process-table and single-instance primitives used by
//! `daemon.rs`. The Unix side shells out to `ps`/`kill`/`flock`; none of
//! that exists on Windows, so these are the native equivalents:
//!
//! - [`process_table`] — Toolhelp32 snapshot for the pid list +
//!   `NtQueryInformationProcess`/PEB read for each process's command
//!   line (the only non-WMI way to get another process's argv).
//! - [`process_is_running`] — `OpenProcess` + `QueryFullProcessImageNameW`
//!   so a recycled pid can't impersonate the daemon.
//! - [`terminate`] — `TerminateProcess`. Windows has no SIGTERM
//!   equivalent for a detached process; the pid file is reclaimed by
//!   `remove_stale_pid` on the next start.
//! - [`InstanceMutex`] — the `flock` replacement: a named kernel mutex
//!   per runtime instance, held for the daemon's lifetime and released
//!   by the kernel on process death (even an abrupt TerminateProcess).

use anyhow::{Context, Result};
use windows::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    CreateMutexW, OpenProcess, PEB, PROCESS_ACCESS_RIGHTS, PROCESS_BASIC_INFORMATION,
    PROCESS_NAME_FORMAT, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE, PROCESS_VM_READ, QueryFullProcessImageNameW, RTL_USER_PROCESS_PARAMETERS,
    ReleaseMutex, TerminateProcess,
};

/// Closes the kernel handle on drop.
struct HandleGuard(HANDLE);

impl Drop for HandleGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> Option<HandleGuard> {
    unsafe { OpenProcess(access, false, pid).ok() }.map(HandleGuard)
}

/// True when `pid` refers to a live `always*.exe` process. Matching the
/// image name keeps a recycled pid from looking like our daemon — the
/// pid file can outlive its process by arbitrary time.
pub fn process_is_running(pid: u32) -> bool {
    let Some(guard) = open(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
        return false;
    };
    image_basename(guard.0).is_some_and(|name| exe_is_daemon(&name))
}

/// Basename check shared by the liveness probe and the process-table
/// filter: `always.exe` / `always-daemon.exe`, case-insensitive.
fn exe_is_daemon(name: &str) -> bool {
    let stem = name.strip_suffix(".exe").unwrap_or(name);
    stem.eq_ignore_ascii_case("always") || stem.eq_ignore_ascii_case("always-daemon")
}

fn image_basename(process: HANDLE) -> Option<String> {
    let path = image_path(process)?;
    path.rsplit('\\').next().map(str::to_owned)
}

fn image_path(process: HANDLE) -> Option<String> {
    let mut buf = [0u16; 260]; // MAX_PATH
    let mut len = buf.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_FORMAT(0), // ProcessNameFormatWin32
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .ok()?;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// Kernel-reported process image path, independent of its command-line argv0.
pub fn process_executable_path(pid: u32) -> Option<String> {
    let guard = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    image_path(guard.0)
}

/// `kill -TERM`/`kill -KILL` equivalent — there is only one way to stop
/// a detached Windows process and it is forceful.
pub fn terminate(pid: u32) -> bool {
    let Some(guard) = open(pid, PROCESS_TERMINATE) else {
        return false;
    };
    unsafe { TerminateProcess(guard.0, 1) }.is_ok()
}

/// `(pid, command line)` for every `always*.exe` process — the Windows
/// counterpart of the `ps -eo pid=,args=` table the Unix side builds.
/// Processes whose command line cannot be read are skipped (a caller
/// sweeping orphans would rather miss one than mis-identify it).
pub fn process_table() -> Vec<(u32, String)> {
    let snap = match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) } {
        Ok(h) => HandleGuard(h),
        Err(_) => return Vec::new(),
    };
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut out = Vec::new();
    unsafe {
        if Process32FirstW(snap.0, &mut entry).is_err() {
            return out;
        }
        loop {
            let exe = exe_name(&entry);
            if exe_is_daemon(&exe)
                && let Some(cmd) = command_line(entry.th32ProcessID)
            {
                out.push((entry.th32ProcessID, cmd));
            }
            if Process32NextW(snap.0, &mut entry).is_err() {
                break;
            }
        }
    }
    out
}

fn exe_name(entry: &PROCESSENTRY32W) -> String {
    let end = entry
        .szExeFile
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(entry.szExeFile.len());
    String::from_utf16_lossy(&entry.szExeFile[..end])
}

/// Read another process's command line: `NtQueryInformationProcess`
/// gives its PEB address, `ProcessParameters.CommandLine` is a
/// `UNICODE_STRING` inside it, and the buffer lives in that process's
/// address space — three `ReadProcessMemory` hops.
fn command_line(pid: u32) -> Option<String> {
    if pid == std::process::id() {
        return Some(std::env::args().collect::<Vec<_>>().join(" "));
    }
    let guard = open(pid, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)?;

    let mut info: PROCESS_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let mut ret_len = 0u32;
    let status = unsafe {
        NtQueryInformationProcess(
            guard.0,
            ProcessBasicInformation,
            (&mut info as *mut PROCESS_BASIC_INFORMATION).cast(),
            std::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            &mut ret_len,
        )
    };
    if status.0 < 0 {
        return None;
    }

    let mut peb: PEB = unsafe { std::mem::zeroed() };
    read_remote(guard.0, info.PebBaseAddress.cast(), &mut peb)?;
    let mut params: RTL_USER_PROCESS_PARAMETERS = unsafe { std::mem::zeroed() };
    read_remote(guard.0, peb.ProcessParameters.cast(), &mut params)?;

    // UNICODE_STRING.Length is in bytes; a malformed (or racing) value
    // larger than the OS limit means we can't trust the read.
    let len = usize::from(params.CommandLine.Length / 2);
    if len == 0 || len > 32 * 1024 {
        return None;
    }
    let mut buf = vec![0u16; len];
    unsafe {
        ReadProcessMemory(
            guard.0,
            params.CommandLine.Buffer.0.cast(),
            buf.as_mut_ptr().cast(),
            len * 2,
            None,
        )
        .ok()?;
    }
    Some(String::from_utf16_lossy(&buf))
}

fn read_remote<T>(process: HANDLE, base: *const core::ffi::c_void, out: &mut T) -> Option<()> {
    unsafe {
        ReadProcessMemory(
            process,
            base,
            (out as *mut T).cast(),
            std::mem::size_of::<T>(),
            None,
        )
        .ok()
    }
}

/// The `flock` equivalent: a named kernel mutex, one per runtime
/// instance (`Local\always-daemon`, `-dev`, … matching the pid file's
/// per-instance naming). Held for the daemon's whole lifetime; the
/// kernel releases it on process death, so a dead daemon can never hold
/// the lock — the same guarantee `flock` gives on Unix.
pub struct InstanceMutex(HANDLE);

impl InstanceMutex {
    pub fn acquire() -> Result<Self> {
        let name: Vec<u16> = format!("Local\\always-daemon{}", crate::config::instance_suffix())
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe { CreateMutexW(None, true, windows::core::PCWSTR(name.as_ptr())) }
            .context("CreateMutexW failed")?;
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            let _ = unsafe { CloseHandle(handle) };
            anyhow::bail!("another always daemon holds the instance mutex");
        }
        Ok(Self(handle))
    }
}

impl Drop for InstanceMutex {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}
