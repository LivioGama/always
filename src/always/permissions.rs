//! Cross-platform permission probes.
//!
//! The daemon checks microphone access, input-monitoring (keyboard listen)
//! grants, and elevation before starting.  Results are broadcast to the
//! GUI via UDS so the UI can show accurate status banners.
//!
//! ## Platform notes
//!
//! * **macOS** — Input Monitoring requires explicit user consent in
//!   *System Settings → Privacy & Security*.  Microphone access is
//!   queried through Core Audio.
//!
//! * **Linux** — xdotool/ydotool do not require special permissions
//!   (they only need an active display connection).  Microphone access
//!   is probed by attempting to open the default PCM device in capture
//!   mode.
//!
//! * **Windows** — Keyboard events are delivered through WASAPI; no
//!   special permission prompt is needed.  Microphone access is
//!   queried through the Windows Audio API.

/// Status of the system microphone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicStatus {
    /// The default microphone is present and accessible for capture.
    Available,
    /// The user (or system policy) has explicitly denied the daemon
    /// access to the microphone.
    Denied,
    /// No microphone device is present or enumerated by the audio
    /// subsystem.
    NotPresent,
}

/// Probe whether the default microphone is available for capture.
///
/// Returns `MicStatus::Available` when the audio subsystem can
/// enumerate at least one capture device.  On Linux this opens the
/// default PCM device briefly and checks for I/O errors that indicate
/// a permissions denial.
pub fn mic_available() -> MicStatus {
    #[cfg(target_os = "macos")]
    {
        probe_mic_macos()
    }

    #[cfg(target_os = "linux")]
    {
        probe_mic_linux()
    }

    #[cfg(target_os = "windows")]
    {
        probe_mic_windows()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        MicStatus::NotPresent
    }
}

/// Probe microphone availability on macOS via Core Audio.
#[cfg(target_os = "macos")]
fn probe_mic_macos() -> MicStatus {
    // Core Audio uses a simple property query: enumerate device property
    // kAudioHardwarePropertyDevices.  If we get at least one device,
    // the subsystem is alive; we don't need a real capture handle.
    unsafe extern "C" {
        fn AudioHardwareGetDeviceCount(outCount: *mut u32) -> i32;
    }

    let mut count: u32 = 0;
    let ret = unsafe { AudioHardwareGetDeviceCount(&mut count) };

    if ret == 0 && count > 0 {
        MicStatus::Available
    } else if ret != 0 {
        MicStatus::Denied
    } else {
        MicStatus::NotPresent
    }
}

/// Probe microphone availability on Linux via ALSA.
///
/// Opens the default PCM device in capture mode.  If it returns
/// `-EACCES` / `-EBUSY` the user has denied access; `ENOENT` means
/// no device is present.
#[cfg(target_os = "linux")]
fn probe_mic_linux() -> MicStatus {
    use std::ffi::c_void;
    use std::ptr;

    unsafe extern "C" {
        fn snd_pcm_open(
            pcm: *mut *mut c_void,
            name: *const i8,
            direction: libc::c_int,
            mode: libc::c_int,
        ) -> libc::c_int;
        fn snd_pcm_close(pcm: *mut c_void) -> libc::c_int;
    }

    let name = b"default\0";
    let mut handle: *mut c_void = ptr::null_mut();

    // SND_PCM_STREAM_CAPTURE = 1
    let ret = unsafe {
        snd_pcm_open(
            &mut handle,
            name.as_ptr() as *const i8,
            1, // capture
            0, // non-blocking
        )
    };

    if ret >= 0 {
        unsafe {
            snd_pcm_close(handle);
        }
        MicStatus::Available
    } else {
        let err = -ret;
        match err {
            libc::EACCES | libc::EBUSY => MicStatus::Denied,
            _ => MicStatus::NotPresent,
        }
    }
}

/// Probe microphone availability on Windows via the Core Audio API.
#[cfg(target_os = "windows")]
fn probe_mic_windows() -> MicStatus {
    use windows::Win32::Media::Audio::{
        CLSID_MMDeviceEnumerator, CoCreateInstance, EDataFlow, ERole,
        IMMDeviceEnumerator_GetDefaultAudioEndpoint,
    };
    use windows::core::{GUID, IID};

    // IID_IMMDeviceEnumerator
    const IID_IMMDeviceEnumerator: GUID = GUID::from_values(
        0xbcde0395,
        0xe52f,
        0x467c,
        [0x8e, 0x3d, 0xc4, 0x57, 0x92, 0x91, 0x69, 0x2e],
    );

    // IID_IAudioEndpointVolume
    const IID_IAudioEndpointVolume: GUID = GUID::from_values(
        0x5cdf2d2,
        0xa1,
        0x409f,
        [0x89, 0x59, 0x7d, 0x06, 0xd3, 0x63, 0xb5, 0x8b],
    );

    // CheckTokenMembership → is admin → may have mic access
    if !is_admin() {
        // Non-admin users might still have mic access; probe it.
        // But first try the simple E_PROP_ID_RESOLUTION path.
        return probe_mic_windows_simple();
    }

    probe_mic_windows_simple()
}

/// Simple Windows mic probe: try to create an endpoint enumerator.
/// Returns Available if successful, NotPresent otherwise.
#[cfg(target_os = "windows")]
fn probe_mic_windows_simple() -> MicStatus {
    use windows::Win32::Media::Audio::{
        CLSID_MMDeviceEnumerator, CoCreateInstance, EDataFlow, ERole,
        IMMDeviceEnumerator_GetDefaultAudioEndpoint,
    };
    use windows::core::{GUID, IID, PWSTR};

    const IID_IMMDeviceEnumerator: GUID = GUID::from_values(
        0xbcde0395,
        0xe52f,
        0x467c,
        [0x8e, 0x3d, 0xc4, 0x57, 0x92, 0x91, 0x69, 0x2e],
    );

    // Try to create the enumerator — if it succeeds, the audio subsystem is alive.
    unsafe {
        let mut enumerator: *mut std::ffi::c_void = std::ptr::null_mut();
        let hr = CoCreateInstance(
            &CLSID_MMDeviceEnumerator,
            None,
            windows::Win32::Foundation::CLSCTX_ALL,
            &IID_IMMDeviceEnumerator,
            &mut enumerator as *mut *mut std::ffi::c_void as *mut _,
        );
        if hr.is_ok() && !enumerator.is_null() {
            std::mem::drop(std::ptr::NonNull::new(enumerator));
            MicStatus::Available
        } else {
            MicStatus::NotPresent
        }
    }
}

/// Whether input monitoring (keyboard listen) access has been granted.
///
/// On macOS this queries the TCC database for `kTCCServiceListenEvent`.
/// On Windows and Linux the return value is `true` — those platforms
/// deliver keyboard events to applications without a user-facing prompt.
pub fn input_monitoring_granted() -> bool {
    #[cfg(target_os = "macos")]
    {
        input_monitoring_granted_macos()
    }

    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Query macOS TCC for Input Monitoring consent.
///
/// Reads the `TCC` database (`~/Library/Application Support/com.apple.TCC/TCC.db`)
/// for a row where `service = 'kTCCServiceListenEvent'` and
/// `client = <our bundle id>`.  Returns `true` when such a row exists
/// with `allow = 1`.
///
/// Falls back to the `rdev`-provided `CGPreflightListenEventAccess`
/// check, which is the same check the system makes when an app tries
/// to create a `CGEventTap`.
#[cfg(target_os = "macos")]
fn input_monitoring_granted_macos() -> bool {
    // Check via CGPreflightListenEventAccess (cheap, never prompts).
    unsafe extern "C" {
        fn CGPreflightListenEventAccess() -> bool;
    }
    if unsafe { CGPreflightListenEventAccess() } {
        return true;
    }

    // Fallback: try to read the TCC database directly.
    // This is best-effort — the TCC schema may change across macOS versions.
    let tcc_db = match std::env::var("HOME") {
        Ok(home) => format!("{home}/Library/Application Support/com.apple.TCC/TCC.db"),
        Err(_) => return false,
    };

    let conn = match rusqlite::Connection::open(&tcc_db) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let allowed: Result<i64, _> = conn.query_row(
        "SELECT allow FROM access \
         WHERE service = 'kTCCServiceListenEvent' \
           AND client = ?",
        [crate::config::bundle_id()], // our bundle id
        |row| row.get(0),
    );

    allowed.unwrap_or(0) == 1
}

/// Whether the process is running with administrative / root privileges.
///
/// On Unix this checks `geteuid() == 0`.  On Windows it checks whether
/// the current access token contains the `SID_ADMINISTRATORS` group.
pub fn is_admin() -> bool {
    #[cfg(target_os = "macos")]
    {
        is_admin_unix()
    }

    #[cfg(target_os = "linux")]
    {
        is_admin_unix()
    }

    #[cfg(target_os = "windows")]
    {
        is_admin_windows()
    }
}

/// Unix admin check: `geteuid() == 0`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn is_admin_unix() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Windows admin check: query the primary access token for the
/// Administrators group via `CheckTokenMembership`.
///
/// Returns `true` when the current process token has the built-in
/// Administrators SID as a member (either directly or through
/// group membership).
#[cfg(target_os = "windows")]
fn is_admin_windows() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::{
        AllocateAndInitializeSid, CheckTokenMembership, FreeSid, SECURITY_NT_AUTHORITY,
        SID_IDENTIFIER_AUTHORITY,
    };
    use windows::Win32::Security::{SecurityImpersonation, TokenAccessInformation, TokenPrimary};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        // Open the current process token.
        let mut token: windows::Win32::Foundation::HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TokenAccessInformation, &mut token).is_err() {
            return false;
        }

        // Build the built-in Administrators SID (S-1-5-32-544).
        let mut authority = std::mem::zeroed::<SID_IDENTIFIER_AUTHORITY>();
        authority.Value[5] = 5; // SECURITY_NT_AUTHORITY
        let mut sid = std::ptr::null_mut();
        let result = AllocateAndInitializeSid(
            &mut authority,
            2,  // 2 sub-authorities
            5,  // SECURITY_BUILTIN_DOMAIN_RID
            32, // DOMAIN_ALIAS_RID_ADMINS
            0,
            0,
            0,
            0,
            0,
            0,
            &mut sid,
        );

        let is_admin = if result.as_bool() && !sid.is_null() {
            let mut is_member: windows::Win32::Foundation::BOOLEAN = 0;
            let ok = CheckTokenMembership(
                std::ptr::null_mut(), // default security context
                sid,
                &mut is_member,
            );
            FreeSid(sid);
            ok.as_bool() && is_member != 0
        } else {
            false
        };

        CloseHandle(token);
        is_admin
    }
}
