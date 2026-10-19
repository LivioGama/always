#![cfg(windows)]

//! Bounded client transport for the Windows per-instance peer pipe.
//!
//! This module intentionally uses a byte pipe and overlapped I/O. Synchronous
//! named-pipe reads and writes can wait forever when the other instance stalls.

use std::ffi::c_void;
use std::io;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

type Handle = *mut c_void;
type Bool = i32;
type Dword = u32;

const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
const GENERIC_READ: Dword = 0x8000_0000;
const GENERIC_WRITE: Dword = 0x4000_0000;
const OPEN_EXISTING: Dword = 3;
const FILE_FLAG_OVERLAPPED: Dword = 0x4000_0000;
const ERROR_FILE_NOT_FOUND: Dword = 2;
const ERROR_BROKEN_PIPE: Dword = 109;
const ERROR_NO_DATA: Dword = 232;
const ERROR_PIPE_NOT_CONNECTED: Dword = 233;
const ERROR_PIPE_BUSY: Dword = 231;
const ERROR_IO_PENDING: Dword = 997;
const ERROR_SEM_TIMEOUT: Dword = 121;
const WAIT_OBJECT_0: Dword = 0;
const WAIT_TIMEOUT: Dword = 258;
const WAIT_FAILED: Dword = 0xFFFF_FFFF;
const MAX_LINE_BYTES: usize = 64 * 1024;
const CANCEL_DRAIN_MILLIS: Dword = 100;
const RETRY_MILLIS: u64 = 5;

#[repr(C)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: Dword,
    offset_high: Dword,
    event: Handle,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: Dword,
        share_mode: Dword,
        security_attributes: *mut c_void,
        creation_disposition: Dword,
        flags_and_attributes: Dword,
        template_file: Handle,
    ) -> Handle;
    fn WaitNamedPipeW(name: *const u16, timeout: Dword) -> Bool;
    fn CreateEventW(
        security_attributes: *mut c_void,
        manual_reset: Bool,
        initial_state: Bool,
        name: *const u16,
    ) -> Handle;
    fn WaitForSingleObject(handle: Handle, milliseconds: Dword) -> Dword;
    fn ReadFile(
        file: Handle,
        buffer: *mut c_void,
        bytes_to_read: Dword,
        bytes_read: *mut Dword,
        overlapped: *mut Overlapped,
    ) -> Bool;
    fn WriteFile(
        file: Handle,
        buffer: *const c_void,
        bytes_to_write: Dword,
        bytes_written: *mut Dword,
        overlapped: *mut Overlapped,
    ) -> Bool;
    fn GetOverlappedResult(
        file: Handle,
        overlapped: *mut Overlapped,
        transferred: *mut Dword,
        wait: Bool,
    ) -> Bool;
    fn CancelIoEx(file: Handle, overlapped: *mut Overlapped) -> Bool;
    fn GetLastError() -> Dword;
    fn CloseHandle(handle: Handle) -> Bool;
}

/// An opened peer pipe. The handle is owned by this value and closed on drop.
pub(crate) struct PeerPipe {
    handle: Handle,
}

/// Connect to a peer pipe before `timeout` elapses.
pub(crate) fn connect(path: &Path, timeout: Duration) -> io::Result<PeerPipe> {
    let deadline = Instant::now() + timeout;
    let name = wide_path(path)?;

    loop {
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            return Ok(PeerPipe { handle });
        }

        let error = unsafe { GetLastError() };
        match error {
            ERROR_PIPE_BUSY => {
                let remaining = remaining(deadline)?;
                let wait = duration_millis_ceil(remaining);
                let ok = unsafe { WaitNamedPipeW(name.as_ptr(), wait) };
                if ok == 0 {
                    let wait_error = unsafe { GetLastError() };
                    if wait_error != ERROR_SEM_TIMEOUT && wait_error != ERROR_PIPE_BUSY {
                        if wait_error == ERROR_FILE_NOT_FOUND {
                            sleep_until_retry(deadline)?;
                            continue;
                        }
                        return Err(io::Error::from_raw_os_error(wait_error as i32));
                    }
                }
            }
            ERROR_FILE_NOT_FOUND | ERROR_NO_DATA => sleep_until_retry(deadline)?,
            _ => return Err(io::Error::from_raw_os_error(error as i32)),
        }
    }
}

impl PeerPipe {
    /// Write every byte, returning if the peer stops consuming before deadline.
    pub(crate) fn write_all(&mut self, bytes: &[u8], deadline: Instant) -> io::Result<()> {
        let mut offset = 0;
        while offset < bytes.len() {
            let end = offset.saturating_add(Dword::MAX as usize).min(bytes.len());
            let written = self.write_chunk(&bytes[offset..end], deadline)?;
            if written == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "peer pipe accepted no bytes",
                ));
            }
            offset += written;
        }
        Ok(())
    }

    /// Read one UTF-8 newline-terminated line, preserving fragmented codepoints.
    /// Returns `None` only when EOF arrives with no buffered bytes.
    pub(crate) fn read_line_until(&mut self, deadline: Instant) -> io::Result<Option<String>> {
        let mut bytes = Vec::with_capacity(256);
        loop {
            let mut byte = [0u8; 1];
            match self.read_chunk(&mut byte, deadline) {
                Ok(0) => {
                    if bytes.is_empty() {
                        return Ok(None);
                    }
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "peer pipe closed before newline",
                    ));
                }
                Ok(_) => {
                    if byte[0] == b'\n' {
                        return String::from_utf8(bytes)
                            .map(Some)
                            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
                    }
                    if bytes.len() == MAX_LINE_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "peer pipe line exceeds 64 KiB",
                        ));
                    }
                    bytes.push(byte[0]);
                }
                Err(error) if is_eof_error(&error) => {
                    if bytes.is_empty() {
                        return Ok(None);
                    }
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "peer pipe closed before newline",
                    ));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn write_chunk(&self, bytes: &[u8], deadline: Instant) -> io::Result<usize> {
        let mut operation = PendingOperation::new(bytes.to_vec())?;
        let ok = unsafe {
            WriteFile(
                self.handle,
                operation.buffer.as_ptr().cast(),
                bytes.len() as Dword,
                null_mut(),
                operation.overlapped_mut(),
            )
        };
        if ok != 0 {
            return operation.completed(self.handle);
        }
        let error = unsafe { GetLastError() };
        if error != ERROR_IO_PENDING {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        operation.wait(self.handle, deadline)
    }

    fn read_chunk(&self, bytes: &mut [u8], deadline: Instant) -> io::Result<usize> {
        let mut operation = PendingOperation::new(vec![0; bytes.len()])?;
        let ok = unsafe {
            ReadFile(
                self.handle,
                operation.buffer.as_mut_ptr().cast(),
                bytes.len() as Dword,
                null_mut(),
                operation.overlapped_mut(),
            )
        };
        let result = if ok != 0 {
            operation.completed(self.handle)
        } else {
            let error = unsafe { GetLastError() };
            if error != ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            operation.wait(self.handle, deadline)
        }?;
        bytes[..result].copy_from_slice(&operation.buffer[..result]);
        Ok(result)
    }
}

impl Drop for PeerPipe {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

struct PendingOperation {
    overlapped: Box<Overlapped>,
    event: Handle,
    // ReadFile/WriteFile may still borrow this buffer after a timeout while
    // CancelIoEx is completing. It must live as long as the OVERLAPPED.
    buffer: Vec<u8>,
}

impl PendingOperation {
    fn new(buffer: Vec<u8>) -> io::Result<Self> {
        let event = unsafe { CreateEventW(null_mut(), 1, 0, null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let overlapped = Box::new(Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event,
        });
        Ok(Self {
            overlapped,
            event,
            buffer,
        })
    }

    fn overlapped_mut(&mut self) -> *mut Overlapped {
        &mut *self.overlapped
    }

    fn wait(&mut self, handle: Handle, deadline: Instant) -> io::Result<usize> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            self.cancel_and_drain(handle);
            return Err(timed_out());
        }
        let wait_ms = duration_millis_ceil(remaining);
        let wait = unsafe { WaitForSingleObject(self.event, wait_ms) };
        if wait == WAIT_TIMEOUT {
            self.cancel_and_drain(handle);
            return Err(timed_out());
        }
        if wait == WAIT_FAILED {
            let error = io::Error::last_os_error();
            self.cancel_and_drain(handle);
            return Err(error);
        }
        if wait != WAIT_OBJECT_0 {
            self.cancel_and_drain(handle);
            return Err(io::Error::other("unexpected wait result for peer pipe I/O"));
        }
        let mut transferred = 0;
        let ok = unsafe { GetOverlappedResult(handle, self.overlapped_mut(), &mut transferred, 0) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(transferred as usize)
    }

    fn completed(&mut self, handle: Handle) -> io::Result<usize> {
        let mut transferred = 0;
        let ok = unsafe { GetOverlappedResult(handle, self.overlapped_mut(), &mut transferred, 0) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(transferred as usize)
    }

    fn cancel_and_drain(&mut self, handle: Handle) {
        unsafe {
            CancelIoEx(handle, self.overlapped_mut());
            let wait = WaitForSingleObject(self.event, CANCEL_DRAIN_MILLIS);
            if wait == WAIT_OBJECT_0 {
                let mut ignored = 0;
                GetOverlappedResult(handle, self.overlapped_mut(), &mut ignored, 0);
            } else {
                // The OS still owns the OVERLAPPED pointer while cancellation is
                // pending. Keep both it and its event alive rather than risk UAF.
                mem::forget(mem::replace(self, Self::leaked_placeholder()));
            }
        }
    }

    fn leaked_placeholder() -> Self {
        Self {
            overlapped: Box::new(Overlapped {
                internal: 0,
                internal_high: 0,
                offset: 0,
                offset_high: 0,
                event: null_mut(),
            }),
            event: null_mut(),
            buffer: Vec::new(),
        }
    }
}

impl Drop for PendingOperation {
    fn drop(&mut self) {
        if !self.event.is_null() {
            unsafe { CloseHandle(self.event) };
        }
    }
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.is_empty() || wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid named pipe path",
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(timed_out())
    } else {
        Ok(remaining)
    }
}

fn duration_millis_ceil(duration: Duration) -> Dword {
    let millis = duration
        .as_millis()
        .saturating_add(u128::from(duration.subsec_nanos() % 1_000_000 != 0));
    millis.clamp(1, u128::from(Dword::MAX - 1)) as Dword
}

fn sleep_until_retry(deadline: Instant) -> io::Result<()> {
    let wait = remaining(deadline)?;
    std::thread::sleep(wait.min(Duration::from_millis(RETRY_MILLIS)));
    remaining(deadline).map(|_| ())
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "peer pipe operation timed out")
}

fn is_eof_error(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error().map(|code| code as Dword),
        Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;

    const PIPE_ACCESS_DUPLEX: Dword = 0x0000_0003;
    const PIPE_TYPE_BYTE: Dword = 0;
    const PIPE_READMODE_BYTE: Dword = 0;
    const PIPE_WAIT: Dword = 0;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateNamedPipeW(
            name: *const u16,
            open_mode: Dword,
            pipe_mode: Dword,
            max_instances: Dword,
            out_buffer_size: Dword,
            in_buffer_size: Dword,
            default_timeout: Dword,
            security_attributes: *mut c_void,
        ) -> Handle;
        fn ConnectNamedPipe(pipe: Handle, overlapped: *mut Overlapped) -> Bool;
        fn DisconnectNamedPipe(pipe: Handle) -> Bool;
        fn FlushFileBuffers(pipe: Handle) -> Bool;
    }

    fn fixture_pipe() -> (std::path::PathBuf, Vec<u16>) {
        let name = format!(r"\\.\pipe\codex-peer-transport-{}", uuid::Uuid::new_v4());
        let path = std::path::PathBuf::from(&name);
        let wide: Vec<u16> = std::ffi::OsStr::new(&name)
            .encode_wide()
            .chain([0])
            .collect();
        (path, wide)
    }

    #[test]
    fn fixture_names_use_the_local_named_pipe_namespace() {
        let (path, _) = fixture_pipe();
        assert!(
            path.to_string_lossy()
                .starts_with(r"\\.\pipe\codex-peer-transport-")
        );
    }

    #[test]
    fn eof_mapping_does_not_swallow_other_pipe_errors() {
        assert!(is_eof_error(&io::Error::from_raw_os_error(
            ERROR_BROKEN_PIPE as i32
        )));
        assert!(is_eof_error(&io::Error::from_raw_os_error(
            ERROR_PIPE_NOT_CONNECTED as i32
        )));
        assert!(!is_eof_error(&io::Error::from_raw_os_error(
            ERROR_NO_DATA as i32
        )));
        assert!(!is_eof_error(&io::Error::other("unrelated read failure")));
    }

    fn fixture_server<F>(handler: F) -> (std::path::PathBuf, thread::JoinHandle<()>)
    where
        F: FnOnce(Handle) + Send + 'static,
    {
        let (path, wide) = fixture_pipe();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = thread::spawn(move || unsafe {
            let pipe = CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                4096,
                4096,
                0,
                null_mut(),
            );
            assert_ne!(pipe, INVALID_HANDLE_VALUE);
            ready_tx.send(()).unwrap();
            let connected = ConnectNamedPipe(pipe, null_mut());
            assert!(connected != 0 || GetLastError() == 535); // ERROR_PIPE_CONNECTED
            handler(pipe);
            DisconnectNamedPipe(pipe);
            CloseHandle(pipe);
        });
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        (path, thread)
    }

    fn server_write(pipe: Handle, bytes: &[u8]) {
        let mut written = 0;
        let ok = unsafe {
            WriteFile(
                pipe,
                bytes.as_ptr().cast(),
                bytes.len() as Dword,
                &mut written,
                null_mut(),
            )
        };
        assert_ne!(ok, 0);
        assert_eq!(written as usize, bytes.len());
    }

    fn server_flush(pipe: Handle) {
        let flushed = unsafe { FlushFileBuffers(pipe) };
        assert_ne!(flushed, 0);
    }

    #[test]
    fn bounds_connect_when_fixture_pipe_is_absent() {
        let (path, _) = fixture_pipe();
        let started = Instant::now();
        let error = match connect(&path, Duration::from_millis(50)) {
            Ok(_) => panic!("connection unexpectedly succeeded for an absent fixture pipe"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_millis(300));
    }

    #[test]
    fn reads_fragmented_utf8_line_and_clean_eof() {
        let (path, server) = fixture_server(|pipe| {
            server_write(pipe, b"caf\xC3");
            thread::sleep(Duration::from_millis(10));
            server_write(pipe, b"\xA9\n");
            server_flush(pipe);
        });
        let mut client = connect(&path, Duration::from_secs(1)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            client.read_line_until(deadline).unwrap().as_deref(),
            Some("café")
        );
        assert_eq!(client.read_line_until(deadline).unwrap(), None);
        server.join().unwrap();
    }

    #[test]
    fn rejects_eof_mid_line() {
        let (path, server) = fixture_server(|pipe| {
            server_write(pipe, b"partial");
            server_flush(pipe);
        });
        let mut client = connect(&path, Duration::from_secs(1)).unwrap();
        let error = client
            .read_line_until(Instant::now() + Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        server.join().unwrap();
    }

    #[test]
    fn bounds_write_when_server_does_not_read() {
        let (path, server) = fixture_server(|_pipe| {
            thread::sleep(Duration::from_millis(300));
        });
        let mut client = connect(&path, Duration::from_secs(1)).unwrap();
        let payload = vec![0x5Au8; 4 * 1024 * 1024];
        let deadline = Instant::now() + Duration::from_millis(100);
        let started = Instant::now();
        let error = client.write_all(&payload, deadline).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_millis(500));
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn bounds_read_when_server_stays_silent() {
        let (path, server) = fixture_server(|_pipe| thread::sleep(Duration::from_millis(200)));
        let mut client = connect(&path, Duration::from_secs(1)).unwrap();
        let started = Instant::now();
        let error = client
            .read_line_until(Instant::now() + Duration::from_millis(50))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_millis(300));
        drop(client);
        server.join().unwrap();
    }
}
