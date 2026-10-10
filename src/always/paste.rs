//! Clipboard + paste abstraction.
//!
//! The macOS implementation shells out to `pbcopy` for clipboard write and
//! posts synthetic Cmd+V / Return events via Core Graphics. The trait
//! ([`ClipboardProvider`]) exists so the `event_loop` can be exercised
//! end-to-end in tests without actually touching the user's pasteboard,
//! and so future Linux/Windows backends can plug in without rewriting
//! callers.
//!
//! `MockClipboardProvider` (gated behind `cfg(test)`) records every
//! `copy` and `paste` call so tests can assert exactly what would have
//! been sent to the OS.

use anyhow::{Context, Result};

// ─── Windows API bindings ────────────────────────────────────────────────
//
// Used by `WindowsClipboard`, `write_clipboard_text`, `paste_via_sendinput`,
// and the Windows-specific `press_return`.  All gated behind
// `target_os = "windows"`.

#[cfg(target_os = "windows")]
mod win32 {
    pub use windows::Win32::Foundation::{
        CloseHandle, HANDLE, HMODULE, PROC_THREAD_ATTRIBUTE_LIST,
    };
    pub use windows::Win32::Storage::FileSystem::{
        EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
    };
    pub use windows::Win32::System::Memory::{
        GHND, GlobalAlloc, GlobalFree, GlobalLock, GlobalUnlock,
    };
    pub use windows::Win32::UI::WindowsAndMessaging::{
        CF_UNICODETEXT, CloseClipboard, GetForegroundWindow, GetModuleFileNameExW,
        GetWindowThreadProcessId, OpenProcess, lstrlenW,
    };
    pub use windows::core::PCWSTR;

    // ─── Win32 structs & constants ───────────────────────────────────────

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct INPUT {
        pub type_: u32,
        pub II: INPUT_6,
    }

    #[repr(C)]
    pub union INPUT_6 {
        pub ki: KEYBDINPUT,
    }

    #[repr(C)]
    pub struct KEYBDINPUT {
        pub wVk: u16,
        pub wScan: u16,
        pub dwFlags: u32,
        pub time: u32,
        pub dwExtraInfo: usize,
    }

    pub const INPUT_KEYBOARD: u32 = 1;
    pub const KEYEVENTF_KEYUP: u32 = 0x0002;

    // Virtual key codes
    pub const VK_CONTROL: u16 = 0x11;
    pub const VK_V: u16 = 0x56;
    pub const VK_RETURN: u16 = 0x0D;

    // Process access rights
    pub const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    // Global memory allocation flags
    pub const GMEM_MOVEABLE: u32 = 0x0002;

    // lstrlenW
    pub use windows::Win32::UI::Shell::lstrlenW;
}

/// Clipboard + simulated-paste capability.
///
/// All implementations must be `Send + Sync` so the daemon can hand a
/// trait object across thread boundaries.
pub trait ClipboardProvider: Send + Sync {
    /// Write `text` to the system clipboard, replacing the previous content.
    fn copy(&self, text: &str) -> Result<()>;

    /// Paste the clipboard at the current keyboard focus, optionally
    /// followed by Return when `auto_enter` is set.
    fn paste(&self, auto_enter: bool) -> Result<()>;

    /// Convenience: copy followed by paste. Default impl is what every
    /// production caller wants; no need to override.
    fn copy_and_paste(&self, text: &str, auto_enter: bool) -> Result<()> {
        self.copy(text)?;
        self.paste(auto_enter)
    }
}

/// macOS-native clipboard via `pbcopy` + Core Graphics keyboard events.
///
/// Stateless — safe to construct anywhere and share via `Arc`.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacClipboard;

impl ClipboardProvider for MacClipboard {
    fn copy(&self, text: &str) -> Result<()> {
        copy_to_clipboard(text.to_string())
    }

    fn paste(&self, auto_enter: bool) -> Result<()> {
        paste_text(auto_enter)
    }
}

/// Windows-native clipboard via Win32 `SetClipboardData` +
/// `SendInput`-backed Ctrl+V simulation.
///
/// Stateless — safe to construct anywhere and share via `Arc`.
#[cfg(target_os = "windows")]
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsClipboard;

#[cfg(target_os = "windows")]
impl ClipboardProvider for WindowsClipboard {
    fn copy(&self, text: &str) -> Result<()> {
        write_clipboard_text(text)
    }

    fn paste(&self, auto_enter: bool) -> Result<()> {
        paste_via_sendinput(auto_enter)
    }
}

/// Linux clipboard via `xclip` / `xdotool` + optional Wayland ydotool.
///
/// Stateless — safe to construct anywhere and share via `Arc`.
#[cfg(target_os = "linux")]
#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxClipboard;

#[cfg(target_os = "linux")]
impl ClipboardProvider for LinuxClipboard {
    fn copy(&self, text: &str) -> Result<()> {
        copy_to_clipboard(text.to_string())
    }

    fn paste(&self, auto_enter: bool) -> Result<()> {
        paste_text(auto_enter)
    }
}

pub fn copy_to_clipboard(text: String) -> Result<()> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let copy_cmd = if cfg!(target_os = "macos") {
        "pbcopy"
    } else if cfg!(target_os = "linux") {
        "xclip"
    } else {
        anyhow::bail!("copy_to_clipboard not implemented for this platform");
    };

    let mut command = Command::new(copy_cmd);
    if cfg!(target_os = "linux") {
        command.args(["-selection", "clipboard", "-in"]);
    }

    let mut copy = command
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to run {copy_cmd}"))?;
    let write_res: Result<()> = (|| {
        copy.stdin
            .as_mut()
            .with_context(|| format!("Failed to open {copy_cmd} stdin"))?
            .write_all(text.as_bytes())
            .context("Failed to write transcript to clipboard")?;
        Ok(())
    })();
    // Always reap the child even when the write failed. `wait()` closes our
    // stdin handle first (so the clipboard tool sees EOF and exits); skipping it on the
    // error path would leak a zombie process on this always-on daemon.
    let status = copy
        .wait()
        .with_context(|| format!("Failed waiting for {copy_cmd}"))?;
    write_res?;
    if !status.success() {
        anyhow::bail!("{copy_cmd} failed");
    }
    Ok(())
}

/// How long after the initial paste we may undo+repaste a grammar patch.
/// Beyond this the user may have edited the field and undo is unsafe.
pub const GRAMMAR_PATCH_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(4);

#[cfg(target_os = "macos")]
fn post_cmd_key(
    source: &core_graphics::event_source::CGEventSource,
    keycode: core_graphics::event::CGKeyCode,
) -> Result<()> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};

    let down = CGEvent::new_keyboard_event(source.clone(), keycode, true)
        .map_err(|_| anyhow::anyhow!("Failed to create key down event"))?;
    down.set_flags(CGEventFlags::CGEventFlagCommand);
    down.post(CGEventTapLocation::HID);

    let up = CGEvent::new_keyboard_event(source.clone(), keycode, false)
        .map_err(|_| anyhow::anyhow!("Failed to create key up event"))?;
    up.set_flags(CGEventFlags::CGEventFlagCommand);
    up.post(CGEventTapLocation::HID);
    Ok(())
}

/// Undo the most recent paste in the focused app (Cmd+Z).
#[cfg(target_os = "macos")]
pub fn undo_last_paste() -> Result<()> {
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| anyhow::anyhow!("Failed to create CGEventSource"))?;
    // 'z' key
    post_cmd_key(&source, 6)?;
    // Let the target app process undo before we repaste.
    std::thread::sleep(std::time::Duration::from_millis(40));
    Ok(())
}

/// Replace the last paste with `text` via undo + clipboard paste (no Return).
#[cfg(target_os = "macos")]
pub fn replace_via_undo(text: &str) -> Result<()> {
    undo_last_paste()?;
    copy_to_clipboard(text.to_string())?;
    paste_text(false)
}

#[cfg(not(target_os = "macos"))]
pub fn undo_last_paste() -> Result<()> {
    anyhow::bail!("undo_last_paste not implemented without the macos feature")
}

#[cfg(not(target_os = "macos"))]
pub fn replace_via_undo(_text: &str) -> Result<()> {
    anyhow::bail!("replace_via_undo not implemented without the macos feature")
}

// ─── macOS paste_text ───────────────────────────────────────────────────

#[cfg(target_os = "macos")]
pub fn paste_text(auto_enter: bool) -> Result<()> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| anyhow::anyhow!("Failed to create CGEventSource"))?;

    // Simulate Cmd+V
    let v_keycode: CGKeyCode = 9; // 'v' key
    let key_down = CGEvent::new_keyboard_event(source.clone(), v_keycode, true)
        .map_err(|_| anyhow::anyhow!("Failed to create key down event"))?;
    key_down.set_flags(CGEventFlags::CGEventFlagCommand);
    key_down.post(CGEventTapLocation::HID);

    let key_up = CGEvent::new_keyboard_event(source.clone(), v_keycode, false)
        .map_err(|_| anyhow::anyhow!("Failed to create key up event"))?;
    key_up.set_flags(CGEventFlags::CGEventFlagCommand);
    key_up.post(CGEventTapLocation::HID);

    if auto_enter {
        // Longer sleep: the Cmd flag from the V keystrokes above can
        // still register as held when Return posts < 20ms later. In
        // Ghostty (and other apps that bind Cmd+Return) that surfaces
        // as a spurious binding hit instead of a newline. Bumping to
        // 50ms and explicitly clearing flags on the Return events
        // makes the keystroke read as a bare Return everywhere.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let enter_keycode: CGKeyCode = 36; // Return key
        let enter_down = CGEvent::new_keyboard_event(source.clone(), enter_keycode, true)
            .map_err(|_| anyhow::anyhow!("Failed to create enter key down event"))?;
        enter_down.set_flags(CGEventFlags::empty());
        enter_down.post(CGEventTapLocation::HID);

        let enter_up = CGEvent::new_keyboard_event(source, enter_keycode, false)
            .map_err(|_| anyhow::anyhow!("Failed to create enter key up event"))?;
        enter_up.set_flags(CGEventFlags::empty());
        enter_up.post(CGEventTapLocation::HID);
    }

    Ok(())
}

// ─── Windows paste_text ─────────────────────────────────────────────────

/// Simulate Ctrl+V (and optionally Return) via SendInput.
#[cfg(target_os = "windows")]
pub fn paste_text(auto_enter: bool) -> Result<()> {
    use win32_keyboard::paste_via_sendinput as paste_via_sendinput_inner;
    paste_via_sendinput_inner(auto_enter)
}

// ─── Linux paste_text ───────────────────────────────────────────────────

/// Paste on Linux: detect Wayland (ydotool first, xdotool fallback) or X11.
#[cfg(target_os = "linux")]
pub fn paste_text(auto_enter: bool) -> Result<()> {
    // Detect Wayland: try ydotool first, fall back to xdotool.
    if is_wayland_session() {
        paste_wayland(auto_enter)?;
    } else {
        paste_x11(auto_enter)?;
    }
    Ok(())
}

/// Detect whether the current session is Wayland by inspecting
/// `WAYLAND_DISPLAY`.  Returns `true` when the env var is set to a
/// non-empty string (which the desktop environment sets for every
/// Wayland compositor).
pub fn is_wayland_session() -> bool {
    std::env::var("WAYLAND_DISPLAY")
        .ok()
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// Paste on a Wayland session.
///
/// Strategy:
/// 1. Try `ydotool type <text>` — direct text injection via evdev.
/// 2. Fall back to `xdotool ctrl+v` if ydotool is unavailable.
///
/// `ydotool` works on both Wayland and X11, but when xdotool is also
/// present (e.g. in a Wayland session with XWayland compatibility),
/// preferring `ydotool` avoids the X11 round-trip that xdotool forces.
#[cfg(target_os = "linux")]
fn paste_wayland(auto_enter: bool) -> Result<()> {
    // Try ydotool first — it types raw text directly into the focused window.
    if which_present("ydotool") {
        return paste_wayland_ydotool(auto_enter);
    }

    // Fall back to xdotool — works on Wayland via XWayland compatibility.
    paste_x11(auto_enter)
}

/// Paste via `ydotool type` — types plain text into the focused window.
#[cfg(target_os = "linux")]
fn paste_wayland_ydotool(_auto_enter: bool) -> Result<()> {
    // ydotool type sends the text as a series of key events.
    // For clipboard paste on Wayland, we first write to clipboard via wl-copy/xdg-clipper,
    // then simulate Ctrl+V via ydotool key codes.
    post_ydotool_key(["--delay", "3", "ctrl:v"])
}

/// Paste on an X11 session via xdotool.
///
/// In terminals we use Ctrl+Shift+V (many terminals bind Paste to that),
/// while in regular apps Ctrl+V is the standard paste shortcut.
#[cfg(target_os = "linux")]
fn paste_x11(auto_enter: bool) -> Result<()> {
    if focused_window_is_terminal().unwrap_or(false) {
        post_xdotool_key(["key", "--clearmodifiers", "ctrl+shift+v"])?;
    } else {
        post_xdotool_key(["key", "--clearmodifiers", "ctrl+v"])?;
    }
    if auto_enter {
        std::thread::sleep(std::time::Duration::from_millis(50));
        post_xdotool_key(["key", "--clearmodifiers", "Return"])?;
    }
    Ok(())
}

/// Check whether an executable is available on PATH.
#[allow(dead_code)]
fn which_present(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn paste(text: &str, auto_enter: bool) -> Result<()> {
    copy_to_clipboard(text.to_string())?;
    paste_text(auto_enter)?;
    Ok(())
}

/// Read the current clipboard text via `pbpaste`.
///
/// Mirrors `correction::read_clipboard` (the Cmd+C capture path already
/// uses this exact pattern). Used to snapshot the user's clipboard before
/// we overwrite it with a transcript so it can be restored afterwards.
///
/// NOTE: this preserves only the **string** flavor of the pasteboard.
/// Non-text flavors (images, RTF, file URLs) on the prior clipboard are
/// not captured and therefore not restored — plain-text round-trip is the
/// high-value case for an always-on dictation daemon.
pub fn read_clipboard_text() -> Result<String> {
    let (cmd, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("pbpaste", &[])
    } else if cfg!(target_os = "linux") {
        ("xclip", &["-selection", "clipboard", "-out"])
    } else {
        anyhow::bail!("read_clipboard_text not implemented for this platform");
    };
    let output = std::process::Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("spawn {cmd}"))?;
    if !output.status.success() {
        anyhow::bail!("{cmd} exited with status {}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Restore a previously-snapshotted clipboard string, but only if the
/// current clipboard still holds exactly what we wrote (`just_wrote`).
///
/// This is the guard against clobbering a *fresh* user copy: after the
/// synthetic Cmd+V has been consumed we re-read the clipboard; if it no
/// longer equals the transcript we put there, then the user (or another
/// app, or the passive watcher) copied something new in the meantime —
/// so we leave their clipboard alone. Only when the clipboard is still
/// our own transcript do we put `prev` back.
///
/// `write_token` is the [`pasteboard_change_count`] captured right after
/// our own clipboard write. When available it is the stronger guard: any
/// later write bumps the count — including one that wrote the *same
/// string* or only non-text flavors, both of which the string compare
/// below cannot see. `None` (off-macOS, runtime lookup failed) degrades
/// to the string compare alone.
///
/// Caller is responsible for sleeping long enough that the target app has
/// consumed the paste before invoking this. Best-effort: a failed restore
/// is logged by the caller, never propagated — losing the user's prior
/// clipboard is a worse outcome than skipping the restore, but a failed
/// re-read here should likewise not abort the caller.
///
/// As with [`read_clipboard_text`], only the string flavor is restored;
/// non-text flavors that were on the clipboard before are not preserved.
pub fn restore_clipboard_if_unchanged(
    prev: &str,
    just_wrote: &str,
    write_token: Option<i64>,
) -> Result<()> {
    // Strong guard: the pasteboard server counted a write after ours —
    // whatever is on the clipboard now, it isn't (only) our transcript.
    if let (Some(token), Some(current)) = (write_token, pasteboard_change_count())
        && current != token
    {
        return Ok(());
    }
    // If the clipboard no longer matches the transcript we wrote, a fresh
    // copy happened during the paste window — don't clobber it.
    match read_clipboard_text() {
        Ok(current) if current == just_wrote => copy_to_clipboard(prev.to_string()),
        Ok(_) => Ok(()), // user/app copied something new — leave it.
        // Re-read failed: be conservative and skip the restore rather than
        // risk overwriting an unknown clipboard state.
        Err(e) => Err(e),
    }
}

/// Synthesize a single Return keypress in the focused app. Used to
/// commit auto-enter after a countdown overlay completes (vs.
/// inline in `paste_text(auto_enter=true)`).
///
/// Explicitly clears `CGEventFlags` on both down and up events: without
/// this the Return can inherit lingering Command/Option state from a
/// preceding Cmd+V (or from a modifier the user happens to be holding)
/// and apps like Ghostty interpret the result as Cmd+Return — a
/// configured binding, not a newline.
#[cfg(target_os = "macos")]
pub fn press_return() -> Result<()> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| anyhow::anyhow!("Failed to create CGEventSource"))?;
    let enter_keycode: CGKeyCode = 36;
    let enter_down = CGEvent::new_keyboard_event(source.clone(), enter_keycode, true)
        .map_err(|_| anyhow::anyhow!("Failed to create enter key down event"))?;
    enter_down.set_flags(CGEventFlags::empty());
    enter_down.post(CGEventTapLocation::HID);
    let enter_up = CGEvent::new_keyboard_event(source, enter_keycode, false)
        .map_err(|_| anyhow::anyhow!("Failed to create enter key up event"))?;
    enter_up.set_flags(CGEventFlags::empty());
    enter_up.post(CGEventTapLocation::HID);
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn press_return() -> Result<()> {
    use std::ptr;

    // Return down
    unsafe {
        let mut enter_down = std::mem::zeroed::<INPUT>();
        enter_down.II.ki.wVk = 0x0D; // VK_RETURN
        enter_down.II.ki.dwFlags = 0;
        SendInput(
            1,
            &mut enter_down as *mut INPUT,
            std::mem::size_of::<INPUT>() as i32,
        );
    }

    std::thread::sleep(std::time::Duration::from_millis(5));

    // Return up
    unsafe {
        let mut enter_up = std::mem::zeroed::<INPUT>();
        enter_up.II.ki.wVk = 0x0D;
        enter_up.II.ki.dwFlags = KEYEVENTF_KEYUP;
        SendInput(
            1,
            &mut enter_up as *mut INPUT,
            std::mem::size_of::<INPUT>() as i32,
        );
    }

    Ok(())
}

#[cfg(target_os = "linux")]
pub fn press_return() -> Result<()> {
    post_xdotool_key(["key", "--clearmodifiers", "Return"])
}

// ─── Window class detection (Linux / macOS stub) ────────────────────────

#[cfg(target_os = "linux")]
fn focused_window_is_terminal() -> Result<bool> {
    let output = std::process::Command::new("xdotool")
        .args(["getactivewindow", "getwindowclassname"])
        .output()
        .context("Failed to query active window class with xdotool")?;
    if !output.status.success() {
        anyhow::bail!(
            "xdotool getwindowclassname failed with status {}",
            output.status
        );
    }
    Ok(is_terminal_window_class(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

#[cfg(target_os = "linux")]
fn is_terminal_window_class(class_name: &str) -> bool {
    matches!(
        class_name.to_ascii_lowercase().as_str(),
        "alacritty"
            | "com.mitchellh.ghostty"
            | "foot"
            | "gnome-terminal-server"
            | "hyper"
            | "kgx"
            | "kitty"
            | "konsole"
            | "org.wezfurlong.wezterm"
            | "ptyxis"
            | "tabby"
            | "terminator"
            | "tilix"
            | "urxvt"
            | "wezterm"
            | "xfce4-terminal"
            | "xterm"
    )
}

#[cfg(target_os = "linux")]
fn post_xdotool_key<const N: usize>(args: [&str; N]) -> Result<()> {
    let status = std::process::Command::new("xdotool")
        .args(args)
        .status()
        .context("Failed to run xdotool")?;
    if !status.success() {
        anyhow::bail!("xdotool failed with status {status}");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn post_ydotool_key<const N: usize>(args: [&str; N]) -> Result<()> {
    let status = std::process::Command::new("ydotool")
        .args(args)
        .status()
        .context("Failed to run ydotool")?;
    if !status.success() {
        anyhow::bail!("ydotool failed with status {status}");
    }
    Ok(())
}

// ════════════════════════════════════════════════════════════════════════
// Windows clipboard + keyboard bindings (Win32 API)
// ════════════════════════════════════════════════════════════════════════

/// Windows clipboard: write `text` to the system clipboard via `SetClipboardData`.
///
/// Opens the clipboard, empties it, allocates a `GHND`-flagged global handle,
/// copies the UTF-16LE encoded text, and closes the clipboard.
#[cfg(target_os = "windows")]
fn write_clipboard_text(text: &str) -> Result<()> {
    use std::ptr;

    unsafe {
        // Open the clipboard; fail if another process owns it.
        if OpenClipboard(ptr::null_mut()) == 0 {
            anyhow::bail!("OpenClipboard failed");
        }

        // Clear the clipboard first.
        EmptyClipboard();

        // Convert text to UTF-16LE (including null terminator).
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let byte_len = wide.len() * std::mem::size_of::<u16>();

        // Allocate a global handle with the GHND flag (moveable + zero-init).
        let hglob = GlobalAlloc(GHND as usize, byte_len);
        if hglob.is_null() {
            CloseClipboard();
            anyhow::bail!("GlobalAlloc failed");
        }

        // Lock the handle, copy the data, unlock.
        let ptr = GlobalLock(hglob) as *mut u8;
        if ptr.is_null() {
            GlobalFree(hglob);
            CloseClipboard();
            anyhow::bail!("GlobalLock failed");
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr, byte_len);
        GlobalUnlock(hglob);

        // Place the handle on the clipboard as CF_UNICODETEXT.
        SetClipboardData(CF_UNICODETEXT as usize, hglob as *mut std::ffi::c_void);

        // Close the clipboard — SetClipboardData transfers ownership
        // of hglob to the system, so we must NOT Free it.
        CloseClipboard();
    }
    Ok(())
}

/// Read the current clipboard text via the Win32 API.
#[cfg(target_os = "windows")]
fn read_clipboard_via_api() -> Result<String> {
    use std::ptr;

    unsafe {
        if OpenClipboard(ptr::null_mut()) == 0 {
            anyhow::bail!("OpenClipboard failed");
        }

        let handle = GetClipboardData(CF_UNICODETEXT as usize);
        if handle.is_null() {
            // Clipboard doesn't contain CF_UNICODETEXT — close and bail.
            CloseClipboard();
            anyhow::bail!("Clipboard does not contain text");
        }

        let lock = GlobalLock(handle);
        if lock.is_null() {
            CloseClipboard();
            anyhow::bail!("GlobalLock failed");
        }

        let len = lstrlenW(lock as *const u16);
        let wide_slice = std::slice::from_raw_parts(lock as *const u16, len as usize);
        let s = String::from_utf16_lossy(wide_slice);

        GlobalUnlock(handle);
        CloseClipboard();

        Ok(s)
    }
}

#[cfg(target_os = "windows")]
fn is_terminal_window_class(_class_name: &str) -> bool {
    // On Windows, we check the focused window process name instead of class.
    // Terminals: ConHost (conhost.exe), WindowsTerminal (WindowsTerminal.exe),
    // wt.exe, wt-pipe, etc. This is handled in the caller.
    // We fall back to assuming non-terminal to be safe.
    false
}

/// Determine whether the currently focused window on Windows is a terminal.
///
/// Checks the top-level foreground window's process name.
#[cfg(target_os = "windows")]
fn focused_window_is_terminal() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return false;
        }

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);

        if pid == 0 {
            return false;
        }

        // Open the process to query its name.
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }

        let mut buffer: [u16; 260] = [0; 260];
        let len = GetModuleFileNameExW(
            handle,
            ptr::null_mut(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        );
        CloseHandle(handle);

        if len == 0 {
            return false;
        }

        // Extract just the file name from the full path.
        let path = String::from_utf16_lossy(&buffer[..len as usize]);
        let file_name = path.rsplit('\\').next().unwrap_or("").to_ascii_lowercase();

        matches!(
            file_name.as_str(),
            "conhost.exe"
                | "windowsterminal.exe"
                | "wt.exe"
                | "wt-pipe.exe"
                | "iterm.exe"
                | "mintty.exe"
                | "alacritty.exe"
                | "ghostty.exe"
                | "kitty.exe"
                | "wezterm-gui.exe"
                | "tabby.exe"
                | "hyper.exe"
                | "terminology.exe"
                | "st.exe"
                | "foot"
                | "alacritty"
        )
    }
}

/// Windows-specific key-event helpers via `SendInput`.
#[cfg(target_os = "windows")]
mod win32 {
    pub use win32_keyboard::*;
}

#[cfg(target_os = "windows")]
mod win32_keyboard {
    use std::ptr;

    /// Simulate Ctrl+V via SendInput.
    ///
    /// Sends a controlled sequence of keyboard events: Ctrl-down → V-down → V-up → Ctrl-up,
    /// with brief sleeps between each to ensure the OS and target app process the events
    /// in the correct order.  After the paste, optionally sends a Return keypress when
    /// `auto_enter` is true.
    pub fn paste_via_sendinput(auto_enter: bool) -> Result<()> {
        use std::ptr;

        // Ctrl key down
        unsafe {
            let mut ctrl_down = std::mem::zeroed::<INPUT>();
            ctrl_down.II.ki.wVk = 0x11; // VK_CONTROL
            ctrl_down.II.ki.dwFlags = 0;
            SendInput(
                1,
                &mut ctrl_down as *mut INPUT,
                std::mem::size_of::<INPUT>() as i32,
            );
        }

        std::thread::sleep(std::time::Duration::from_millis(10));

        // 'V' key down
        unsafe {
            let mut v_down = std::mem::zeroed::<INPUT>();
            v_down.II.ki.wVk = 0x56; // VK_V
            v_down.II.ki.dwFlags = 0;
            SendInput(
                1,
                &mut v_down as *mut INPUT,
                std::mem::size_of::<INPUT>() as i32,
            );
        }

        std::thread::sleep(std::time::Duration::from_millis(5));

        // 'V' key up
        unsafe {
            let mut v_up = std::mem::zeroed::<INPUT>();
            v_up.II.ki.wVk = 0x56;
            v_up.II.ki.dwFlags = KEYEVENTF_KEYUP;
            SendInput(
                1,
                &mut v_up as *mut INPUT,
                std::mem::size_of::<INPUT>() as i32,
            );
        }

        std::thread::sleep(std::time::Duration::from_millis(10));

        // Ctrl key up
        unsafe {
            let mut ctrl_up = std::mem::zeroed::<INPUT>();
            ctrl_up.II.ki.wVk = 0x11;
            ctrl_up.II.ki.dwFlags = KEYEVENTF_KEYUP;
            SendInput(
                1,
                &mut ctrl_up as *mut INPUT,
                std::mem::size_of::<INPUT>() as i32,
            );
        }

        if auto_enter {
            std::thread::sleep(std::time::Duration::from_millis(50));

            // Return down
            unsafe {
                let mut enter_down = std::mem::zeroed::<INPUT>();
                enter_down.II.ki.wVk = 0x0D; // VK_RETURN
                enter_down.II.ki.dwFlags = 0;
                SendInput(
                    1,
                    &mut enter_down as *mut INPUT,
                    std::mem::size_of::<INPUT>() as i32,
                );
            }

            std::thread::sleep(std::time::Duration::from_millis(5));

            // Return up
            unsafe {
                let mut enter_up = std::mem::zeroed::<INPUT>();
                enter_up.II.ki.wVk = 0x0D;
                enter_up.II.ki.dwFlags = KEYEVENTF_KEYUP;
                SendInput(
                    1,
                    &mut enter_up as *mut INPUT,
                    std::mem::size_of::<INPUT>() as i32,
                );
            }
        }

        Ok(())
    }

    /// Simulate a single Return key via SendInput.
    pub fn press_return() -> Result<()> {
        unsafe {
            // Return down
            let mut enter_down = std::mem::zeroed::<INPUT>();
            enter_down.II.ki.wVk = 0x0D;
            enter_down.II.ki.dwFlags = 0;
            SendInput(
                1,
                &mut enter_down as *mut INPUT,
                std::mem::size_of::<INPUT>() as i32,
            );
        }

        std::thread::sleep(std::time::Duration::from_millis(5));

        // Return up
        unsafe {
            let mut enter_up = std::mem::zeroed::<INPUT>();
            enter_up.II.ki.wVk = 0x0D;
            enter_up.II.ki.dwFlags = KEYEVENTF_KEYUP;
            SendInput(
                1,
                &mut enter_up as *mut INPUT,
                std::mem::size_of::<INPUT>() as i32,
            );
        }

        Ok(())
    }
}

#[cfg(target_os = "windows")]
pub use win32_keyboard::{
    paste_via_sendinput as paste_via_sendinput_fn, press_return as press_return_win,
};

// ─── Clipboard change-count helpers ─────────────────────────────────────

/// Read the current clipboard text via `pbpaste`.
///
/// Mirrors `correction::read_clipboard` (the Cmd+C capture path already
/// uses this exact pattern). Used to snapshot the user's clipboard before
/// we overwrite it with a transcript so it can be restored afterwards.
///
/// NOTE: this preserves only the **string** flavor of the pasteboard.
/// Non-text flavors (images, RTF, file URLs) on the prior clipboard are
/// not captured and therefore not restored — plain-text round-trip is the
/// high-value case for an always-on dictation daemon.
#[cfg(target_os = "windows")]
pub fn read_clipboard_text() -> Result<String> {
    read_clipboard_via_api()
}

/// `NSPasteboard.general.changeCount` — a counter the pasteboard server
/// bumps on every write by any process. Capturing it right after our own
/// `pbcopy` gives a token that detects *any* later clipboard write, even
/// one that wrote the identical string or a non-text flavor (both
/// invisible to the `pbpaste` string compare).
///
/// Raw objc-runtime calls rather than an AppKit crate dependency — two
/// message sends are not worth a new dep tree. Returns `None` off-macOS
/// or if the runtime lookup fails, in which case callers fall back to
/// the string-compare guard alone.
#[cfg(target_os = "macos")]
pub fn pasteboard_change_count() -> Option<i64> {
    use std::ffi::c_void;

    // Force AppKit into the process so the NSPasteboard class is
    // registered with the runtime — CoreGraphics alone doesn't pull it in.
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}
    unsafe extern "C" {
        fn objc_getClass(name: *const u8) -> *mut c_void;
        fn sel_registerName(name: *const u8) -> *mut c_void;
        fn objc_msgSend();
    }

    unsafe {
        let class = objc_getClass(c"NSPasteboard".as_ptr().cast());
        if class.is_null() {
            return None;
        }
        let send_ptr: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let general = send_ptr(
            class,
            sel_registerName(c"generalPasteboard".as_ptr().cast()),
        );
        if general.is_null() {
            return None;
        }
        let send_int: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i64 =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        Some(send_int(
            general,
            sel_registerName(c"changeCount".as_ptr().cast()),
        ))
    }
}

#[cfg(not(target_os = "macos"))]
pub fn pasteboard_change_count() -> Option<i64> {
    None
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mock::MockClipboardProvider;

    #[test]
    fn mock_records_copy_and_paste() {
        let p = MockClipboardProvider::new();
        p.copy("hello").unwrap();
        p.paste(false).unwrap();
        assert_eq!(p.copied(), vec!["hello".to_string()]);
        assert_eq!(p.paste_calls(), vec![false]);
    }

    #[test]
    fn copy_and_paste_default_chains_calls() {
        let p = MockClipboardProvider::new();
        p.copy_and_paste("git status", true).unwrap();
        assert_eq!(p.copied(), vec!["git status".to_string()]);
        assert_eq!(p.paste_calls(), vec![true]);
    }

    #[test]
    fn provider_is_object_safe() {
        // Erased trait object — the daemon hands `Box<dyn ClipboardProvider>`
        // around. If this stops compiling we've broken DI.
        let _: Box<dyn ClipboardProvider> = Box::new(MockClipboardProvider::new());
    }

    #[cfg(all(not(target_os = "macos"), target_os = "linux"))]
    #[test]
    fn terminal_window_classes_use_terminal_paste_chord() {
        assert!(is_terminal_window_class("kitty"));
        assert!(is_terminal_window_class("Gnome-Terminal-Server"));
        assert!(is_terminal_window_class("org.wezfurlong.WezTerm"));
        assert!(!is_terminal_window_class("firefox"));
    }

    #[test]
    fn wayland_detection_with_unset_var() {
        // Default (unset) — not wayland.
        assert!(!is_wayland_session());
    }
}

#[cfg(test)]
pub mod mock {
    //! In-memory test double for [`ClipboardProvider`].
    //!
    //! Records every call so tests can assert what the daemon would have
    //! pasted into a real application. Not gated behind `pub(crate)`
    //! because the end-to-end integration tests in `tests/` need it too.

    use super::ClipboardProvider;
    use anyhow::Result;
    use parking_lot::Mutex;
    use std::sync::Arc;

    #[derive(Debug, Default, Clone)]
    pub struct MockClipboardProvider {
        calls: Arc<Mutex<MockState>>,
    }

    #[derive(Debug, Default)]
    struct MockState {
        copied: Vec<String>,
        paste_calls: Vec<bool>,
    }

    impl MockClipboardProvider {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn copied(&self) -> Vec<String> {
            self.calls.lock().copied.clone()
        }

        pub fn paste_calls(&self) -> Vec<bool> {
            self.calls.lock().paste_calls.clone()
        }
    }

    impl ClipboardProvider for MockClipboardProvider {
        fn copy(&self, text: &str) -> Result<()> {
            self.calls.lock().copied.push(text.to_string());
            Ok(())
        }
        fn paste(&self, auto_enter: bool) -> Result<()> {
            self.calls.lock().paste_calls.push(auto_enter);
            Ok(())
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod pasteboard_tests {
    use super::*;

    /// Restores the plain-text clipboard even when a live probe panics.
    ///
    /// This deliberately mirrors the production limitation documented on
    /// `read_clipboard_text`: non-text pasteboard flavors cannot be preserved.
    struct ClipboardTextRestore(String);

    impl Drop for ClipboardTextRestore {
        fn drop(&mut self) {
            let _ = copy_to_clipboard(self.0.clone());
        }
    }

    /// Live UTF-8 boundary probe for the exact `pbcopy`/`pbpaste` path used
    /// by dictation.  It is ignored because it temporarily owns the real
    /// clipboard; the guard restores its prior plain-text contents.
    #[test]
    #[ignore = "touches the real macOS pasteboard"]
    fn pbcopy_round_trips_french_utf8_bytes() {
        let _restore = ClipboardTextRestore(read_clipboard_text().unwrap_or_default());
        let french = "Salut, ça va ? Même l'été à Genève — déjà vu.";

        MacClipboard
            .copy(french)
            .expect("pbcopy accepts valid UTF-8");
        let pasted = read_clipboard_text().expect("pbpaste reads clipboard text");

        assert_eq!(pasted, french, "text must not be decoded as MacRoman");
        assert_eq!(
            pasted.as_bytes(),
            french.as_bytes(),
            "UTF-8 bytes must survive"
        );
    }

    /// Live probe of the objc changeCount bridge. Ignored by default
    /// because it writes to (and then restores) the real pasteboard.
    #[test]
    #[ignore = "touches the real macOS pasteboard"]
    fn change_count_bumps_on_write_and_restore_respects_token() {
        let prev = read_clipboard_text().unwrap_or_default();

        let before = pasteboard_change_count().expect("changeCount available");
        copy_to_clipboard("changecount-probe".into()).expect("pbcopy");
        let token = pasteboard_change_count().expect("changeCount available");
        assert!(token > before, "write must bump changeCount");

        // Simulate a foreign write after ours: the restore must skip.
        copy_to_clipboard("foreign-write".into()).expect("pbcopy");
        restore_clipboard_if_unchanged(&prev, "changecount-probe", Some(token))
            .expect("guarded restore");
        assert_eq!(
            read_clipboard_text().expect("pbpaste"),
            "foreign-write",
            "restore must not clobber a post-write clipboard change"
        );

        // With a fresh token and untouched clipboard, the restore fires.
        copy_to_clipboard("changecount-probe".into()).expect("pbcopy");
        let token2 = pasteboard_change_count().expect("changeCount available");
        restore_clipboard_if_unchanged(&prev, "changecount-probe", Some(token2))
            .expect("guarded restore");
        assert_eq!(
            read_clipboard_text().expect("pbpaste"),
            prev,
            "restore must put the prior clipboard back"
        );
    }
}
