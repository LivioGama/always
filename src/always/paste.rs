//! Clipboard + paste abstraction.
//!
//! The macOS implementation shells out to `pbcopy` for clipboard write and
//! posts synthetic Cmd+V / Return events via Core Graphics. The trait
//! ([`ClipboardProvider`]) exists so the `event_loop` can be exercised
//! end-to-end in tests without actually touching the user's pasteboard,
//! and so Linux/Windows backends can plug in without rewriting
//! callers.
//!
//! `MockClipboardProvider` (gated behind `cfg(test)`) records every
//! `copy` and `paste` call so tests can assert exactly what would have
//! been sent to the OS.

use anyhow::{Context, Result};

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
        win32::write_clipboard_text(text)
    }

    fn paste(&self, auto_enter: bool) -> Result<()> {
        win32::paste_via_sendinput(auto_enter)
    }
}

/// Linux clipboard via `wl-clipboard` on Wayland and `xclip` on X11.
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

/// Sandbox guard (launch smoke test, `ALWAYS_NO_PASTE=1`): a sandboxed
/// daemon must never touch the user's clipboard or type into their apps.
fn no_paste_env() -> bool {
    std::env::var("ALWAYS_NO_PASTE").as_deref() == Ok("1")
}

pub fn copy_to_clipboard(text: String) -> Result<()> {
    if no_paste_env() {
        tracing::info!(
            chars = text.chars().count(),
            "clipboard_write_suppressed_no_paste_env"
        );
        return Ok(());
    }
    // Native first: an in-process NSPasteboard write. `pbcopy` costs a
    // fork+exec per paste — measured p50 17.4 ms / p95 22.0 ms (20 runs,
    // 2026-10-13), which was essentially all of `paste_ms`. The subprocess
    // stays as the fallback if the runtime path refuses the write.
    #[cfg(target_os = "macos")]
    if native_pasteboard_write(&text) {
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    return win32::write_clipboard_text(&text);
    #[cfg(not(target_os = "windows"))]
    copy_via_subprocess(text)
}

/// `[NSPasteboard.generalPasteboard clearContents]` +
/// `setString:forType:"public.utf8-plain-text"` through raw objc-runtime
/// calls (same approach as [`pasteboard_change_count`], no AppKit crate).
/// Returns false when any lookup fails or the pasteboard refuses the write.
#[cfg(target_os = "macos")]
fn native_pasteboard_write(text: &str) -> bool {
    use std::ffi::c_void;

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}
    unsafe extern "C" {
        fn objc_getClass(name: *const u8) -> *mut c_void;
        fn sel_registerName(name: *const u8) -> *mut c_void;
        fn objc_msgSend();
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }
    const NS_UTF8_STRING_ENCODING: usize = 4;

    unsafe {
        let pool = objc_autoreleasePoolPush();
        let ok = (|| {
            let send0: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let send_bytes: unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                *const u8,
                usize,
                usize,
            ) -> *mut c_void = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let send_set: unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                *mut c_void,
                *mut c_void,
            ) -> bool = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());

            let pb_class = objc_getClass(c"NSPasteboard".as_ptr().cast());
            let str_class = objc_getClass(c"NSString".as_ptr().cast());
            if pb_class.is_null() || str_class.is_null() {
                return false;
            }
            let general = send0(
                pb_class,
                sel_registerName(c"generalPasteboard".as_ptr().cast()),
            );
            if general.is_null() {
                return false;
            }
            let new_string = |bytes: &[u8]| -> *mut c_void {
                let raw = send0(str_class, sel_registerName(c"alloc".as_ptr().cast()));
                if raw.is_null() {
                    return raw;
                }
                send_bytes(
                    raw,
                    sel_registerName(c"initWithBytes:length:encoding:".as_ptr().cast()),
                    bytes.as_ptr(),
                    bytes.len(),
                    NS_UTF8_STRING_ENCODING,
                )
            };
            let value = new_string(text.as_bytes());
            let kind = new_string(b"public.utf8-plain-text");
            let release = |obj: *mut c_void| {
                if !obj.is_null() {
                    send0(obj, sel_registerName(c"release".as_ptr().cast()));
                }
            };
            if value.is_null() || kind.is_null() {
                release(value);
                release(kind);
                return false;
            }
            send0(general, sel_registerName(c"clearContents".as_ptr().cast()));
            let written = send_set(
                general,
                sel_registerName(c"setString:forType:".as_ptr().cast()),
                value,
                kind,
            );
            release(value);
            release(kind);
            written
        })();
        objc_autoreleasePoolPop(pool);
        if !ok {
            tracing::warn!("native_pasteboard_write_failed_falling_back_to_pbcopy");
        }
        ok
    }
}

#[cfg(not(target_os = "windows"))]
fn copy_via_subprocess(text: String) -> Result<()> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let (copy_cmd, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("pbcopy", &[])
    } else if cfg!(target_os = "linux") && is_wayland_session() && which_present("wl-copy") {
        ("wl-copy", &[])
    } else if cfg!(target_os = "linux") {
        ("xclip", &["-selection", "clipboard", "-in"])
    } else {
        anyhow::bail!("copy_to_clipboard not implemented for this platform");
    };

    let mut command = Command::new(copy_cmd);
    command.args(args);
    // Force a UTF-8 locale for the clipboard tool. pbcopy decodes its stdin
    // bytes via CFStringGetSystemEncoding(); under the launchd-spawned daemon
    // environment (no LANG/LC_*/__CF_USER_TEXT_ENCODING) that is MacRoman, so
    // every non-ASCII transcript ("it’s", "—", "…") landed on the pasteboard
    // as literal mojibake ("it‚Äôs"). LANG alone is sufficient but LC_ALL
    // overrides a stray POSIX LC_* inherited from a non-GUI launch.
    command
        .env("LANG", "en_US.UTF-8")
        .env("LC_ALL", "en_US.UTF-8");
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
    if no_paste_env() {
        return Ok(());
    }
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
    if no_paste_env() {
        return Ok(());
    }
    anyhow::bail!("undo_last_paste not implemented without the macos feature")
}

#[cfg(not(target_os = "macos"))]
pub fn replace_via_undo(_text: &str) -> Result<()> {
    anyhow::bail!("replace_via_undo not implemented without the macos feature")
}

// ─── macOS paste_text ───────────────────────────────────────────────────

#[cfg(target_os = "macos")]
struct MacPasteEvents {
    command_down: core_graphics::event::CGEvent,
    v_down: core_graphics::event::CGEvent,
    v_up: core_graphics::event::CGEvent,
    command_up: core_graphics::event::CGEvent,
    enter: Option<(core_graphics::event::CGEvent, core_graphics::event::CGEvent)>,
}

#[cfg(target_os = "macos")]
fn mac_paste_events(
    source: &core_graphics::event_source::CGEventSource,
    auto_enter: bool,
) -> Result<MacPasteEvents> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGKeyCode, KeyCode};

    let command_keycode: CGKeyCode = KeyCode::COMMAND;
    let v_keycode: CGKeyCode = 9; // 'v' key
    let command_down = CGEvent::new_keyboard_event(source.clone(), command_keycode, true)
        .map_err(|_| anyhow::anyhow!("Failed to create Command key down event"))?;
    command_down.set_flags(CGEventFlags::CGEventFlagCommand);
    let v_down = CGEvent::new_keyboard_event(source.clone(), v_keycode, true)
        .map_err(|_| anyhow::anyhow!("Failed to create V key down event"))?;
    v_down.set_flags(CGEventFlags::CGEventFlagCommand);
    let v_up = CGEvent::new_keyboard_event(source.clone(), v_keycode, false)
        .map_err(|_| anyhow::anyhow!("Failed to create V key up event"))?;
    v_up.set_flags(CGEventFlags::CGEventFlagCommand);
    let command_up = CGEvent::new_keyboard_event(source.clone(), command_keycode, false)
        .map_err(|_| anyhow::anyhow!("Failed to create Command key up event"))?;
    command_up.set_flags(CGEventFlags::empty());

    // Build every event before posting any, so a construction failure cannot
    // leave a modifier logically held in the target application.
    let enter = if auto_enter {
        let enter_keycode: CGKeyCode = KeyCode::RETURN;
        let down = CGEvent::new_keyboard_event(source.clone(), enter_keycode, true)
            .map_err(|_| anyhow::anyhow!("Failed to create Return key down event"))?;
        down.set_flags(CGEventFlags::empty());
        let up = CGEvent::new_keyboard_event(source.clone(), enter_keycode, false)
            .map_err(|_| anyhow::anyhow!("Failed to create Return key up event"))?;
        up.set_flags(CGEventFlags::empty());
        Some((down, up))
    } else {
        None
    };

    Ok(MacPasteEvents {
        command_down,
        v_down,
        v_up,
        command_up,
        enter,
    })
}

#[cfg(target_os = "macos")]
pub fn paste_text(auto_enter: bool) -> Result<()> {
    if no_paste_env() {
        return Ok(());
    }
    use core_graphics::event::CGEventTapLocation;
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| anyhow::anyhow!("Failed to create CGEventSource"))?;
    let events = mac_paste_events(&source, auto_enter)?;
    events.command_down.post(CGEventTapLocation::HID);
    events.v_down.post(CGEventTapLocation::HID);
    events.v_up.post(CGEventTapLocation::HID);
    events.command_up.post(CGEventTapLocation::HID);

    if let Some((enter_down, enter_up)) = events.enter {
        // Longer sleep: the Cmd flag from the V keystrokes above can
        // still register as held when Return posts < 20ms later. In
        // Ghostty (and other apps that bind Cmd+Return) that surfaces
        // as a spurious binding hit instead of a newline. Keep the 50ms
        // delay, with Command explicitly released before the bare Return.
        std::thread::sleep(std::time::Duration::from_millis(50));
        enter_down.post(CGEventTapLocation::HID);
        enter_up.post(CGEventTapLocation::HID);
    }

    Ok(())
}

// ─── Windows paste_text ─────────────────────────────────────────────────

/// Simulate Ctrl+V (and optionally Return) via SendInput.
#[cfg(target_os = "windows")]
pub fn paste_text(auto_enter: bool) -> Result<()> {
    if no_paste_env() {
        return Ok(());
    }
    win32::paste_via_sendinput(auto_enter)
}

// ─── Linux paste_text ───────────────────────────────────────────────────

/// Paste on Linux: detect Wayland (ydotool first, xdotool fallback) or X11.
#[cfg(target_os = "linux")]
pub fn paste_text(auto_enter: bool) -> Result<()> {
    if no_paste_env() {
        return Ok(());
    }
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

/// Paste on a Wayland session (the transcript is already on the clipboard).
///
/// Key injectors, in order; one that fails falls through to the next:
/// 1. `wtype` — the compositor's virtual-keyboard protocol (wlroots
///    compositors such as Hyprland and Sway; part of Omarchy's base install).
///    Needs no daemon and no `/dev/uinput` access, and is keyboard-layout
///    independent. It exits non-zero where the protocol is missing (GNOME,
///    KDE), which is what moves us on.
/// 2. `ydotool key` — kernel uinput; needs a running `ydotoold`.
/// 3. `xdotool` — reaches XWayland windows only.
///
/// On Hyprland the focused window is resolved, so terminals get Ctrl+Shift+V
/// as on X11. Other compositors offer no portable query; they get Ctrl+V.
#[cfg(target_os = "linux")]
fn paste_wayland(auto_enter: bool) -> Result<()> {
    let terminal = hyprland_focused_window_is_terminal().unwrap_or(false);
    for (bin, paste_args, enter_args) in wayland_injectors(terminal) {
        if !which_present(bin) {
            continue;
        }
        match run_injector(bin, &paste_args) {
            Ok(()) => {
                if auto_enter {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    run_injector(bin, &enter_args)?;
                }
                return Ok(());
            }
            Err(e) => tracing::warn!(injector = bin, error = %e, "wayland_paste_injector_failed"),
        }
    }
    paste_x11(auto_enter)
}

/// `(binary, paste chord, Enter)` for each Wayland injector, in preference
/// order. ydotool 1.0 accepts only `key [-d ms] CODE:STATE…` with raw evdev
/// codes (29 ctrl, 42 shift, 47 v, 28 enter) and exits 0 on anything else —
/// the 0.1-era `ctrl:v` form pasted nothing, silently. So the argument shape
/// is the contract, and the tests pin it.
/// ponytail: ydotool codes are positional — on layouts where V is not on the
/// QWERTY V key (Dvorak) the chord is wrong; wtype is tried first and has no
/// such limit.
#[cfg(target_os = "linux")]
fn wayland_injectors(terminal: bool) -> [(&'static str, Vec<&'static str>, Vec<&'static str>); 2] {
    let (wtype_paste, ydotool_paste) = if terminal {
        (
            vec![
                "-M", "ctrl", "-M", "shift", "-k", "v", "-m", "shift", "-m", "ctrl",
            ],
            vec![
                "key", "-d", "3", "29:1", "42:1", "47:1", "47:0", "42:0", "29:0",
            ],
        )
    } else {
        (
            vec!["-M", "ctrl", "-k", "v", "-m", "ctrl"],
            vec!["key", "-d", "3", "29:1", "47:1", "47:0", "29:0"],
        )
    };
    [
        ("wtype", wtype_paste, vec!["-k", "Return"]),
        (
            "ydotool",
            ydotool_paste,
            vec!["key", "-d", "3", "28:1", "28:0"],
        ),
    ]
}

#[cfg(target_os = "linux")]
fn run_injector(bin: &str, args: &[&str]) -> Result<()> {
    let status = std::process::Command::new(bin)
        .args(args)
        .status()
        .with_context(|| format!("Failed to run {bin}"))?;
    if !status.success() {
        anyhow::bail!("{bin} failed with status {status}");
    }
    Ok(())
}

/// On Hyprland, whether the focused window is a terminal. `None` off
/// Hyprland or when the query fails — the caller then sends Ctrl+V.
#[cfg(target_os = "linux")]
fn hyprland_focused_window_is_terminal() -> Option<bool> {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let out = std::process::Command::new("hyprctl")
        .args(["activewindow", "-j"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    active_window_is_terminal(&out.stdout, |pid| {
        std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()
    })
}

/// Classify `hyprctl activewindow -j` output. The class alone misses
/// terminals launched under a custom app-id (`foot --app-id=…`), so the
/// window's process name is checked as well.
#[cfg(target_os = "linux")]
fn active_window_is_terminal(
    json: &[u8],
    process_name: impl Fn(i64) -> Option<String>,
) -> Option<bool> {
    let window: serde_json::Value = serde_json::from_slice(json).ok()?;
    let class = window.get("class").and_then(|c| c.as_str()).unwrap_or("");
    let process_is_terminal = window
        .get("pid")
        .and_then(|p| p.as_i64())
        .and_then(process_name)
        .is_some_and(|name| is_terminal_window_class(name.trim()));
    Some(is_terminal_window_class(class) || process_is_terminal)
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
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
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
#[cfg(not(target_os = "windows"))]
pub fn read_clipboard_text() -> Result<String> {
    let (cmd, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("pbpaste", &[])
    } else if cfg!(target_os = "linux") && is_wayland_session() && which_present("wl-paste") {
        ("wl-paste", &["--no-newline"])
    } else if cfg!(target_os = "linux") {
        ("xclip", &["-selection", "clipboard", "-out"])
    } else {
        anyhow::bail!("read_clipboard_text not implemented for this platform");
    };
    let output = std::process::Command::new(cmd)
        .args(args)
        // pbpaste ENCODES the pasteboard string to stdout via the system
        // encoding — under a C/POSIX locale that emits MacRoman bytes, which
        // the UTF-8 decode below then turns into mojibake. Pin UTF-8.
        .env("LANG", "en_US.UTF-8")
        .env("LC_ALL", "en_US.UTF-8")
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
    if no_paste_env() {
        return Ok(());
    }
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
    if no_paste_env() {
        return Ok(());
    }
    win32::press_return()
}

#[cfg(target_os = "linux")]
pub fn press_return() -> Result<()> {
    if no_paste_env() {
        return Ok(());
    }
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
            | "ghostty"
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

// ════════════════════════════════════════════════════════════════════════
// Windows clipboard + keyboard bindings (Win32 API, `windows` crate 0.62)
// ════════════════════════════════════════════════════════════════════════

/// Win32 helpers behind `WindowsClipboard` and the Windows `paste_text` /
/// `press_return`. Compiled only on Windows.
#[cfg(target_os = "windows")]
mod win32 {
    use anyhow::{Context, Result};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HGLOBAL, HWND};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GHND, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::CF_UNICODETEXT;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
        VIRTUAL_KEY, VK_CONTROL, VK_RETURN, VK_V,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    use windows::core::PWSTR;

    /// Keeps the clipboard open for the lifetime of the guard and closes it
    /// on every exit path (including early `?` returns).
    struct ClipboardGuard;

    impl ClipboardGuard {
        fn open() -> Result<Self> {
            // `None` associates the open clipboard with the current task.
            unsafe { OpenClipboard(None) }.context("OpenClipboard failed")?;
            Ok(Self)
        }
    }

    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            let _ = unsafe { CloseClipboard() };
        }
    }

    /// Write `text` to the system clipboard as `CF_UNICODETEXT`.
    ///
    /// Opens the clipboard, empties it, allocates a `GHND` (moveable +
    /// zero-initialised) global block, copies the UTF-16LE text including
    /// its NUL terminator, and hands the block to the system.
    pub fn write_clipboard_text(text: &str) -> Result<()> {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let byte_len = std::mem::size_of_val(wide.as_slice());

        let _clipboard = ClipboardGuard::open()?;
        unsafe {
            EmptyClipboard().context("EmptyClipboard failed")?;

            let hglobal = GlobalAlloc(GHND, byte_len).context("GlobalAlloc failed")?;
            let dst = GlobalLock(hglobal) as *mut u16;
            if dst.is_null() {
                // Ownership never moved to the system, so release it here.
                let _ = windows::Win32::Foundation::GlobalFree(Some(hglobal));
                anyhow::bail!("GlobalLock failed");
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
            // GlobalUnlock reports an error when the lock count reaches zero;
            // that is the expected outcome here.
            let _ = GlobalUnlock(hglobal);

            // On success the system owns the block, so it must NOT be freed.
            if let Err(err) = SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(hglobal.0)))
            {
                let _ = windows::Win32::Foundation::GlobalFree(Some(hglobal));
                return Err(err).context("SetClipboardData failed");
            }
        }
        Ok(())
    }

    /// Read the current clipboard text (`CF_UNICODETEXT`).
    pub fn read_clipboard_text() -> Result<String> {
        let _clipboard = ClipboardGuard::open()?;
        unsafe {
            let handle = GetClipboardData(u32::from(CF_UNICODETEXT.0))
                .context("Clipboard does not contain text")?;
            let hglobal = HGLOBAL(handle.0);
            let lock = GlobalLock(hglobal) as *const u16;
            if lock.is_null() {
                anyhow::bail!("GlobalLock failed");
            }
            let mut len = 0usize;
            while *lock.add(len) != 0 {
                len += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(lock, len));
            let _ = GlobalUnlock(hglobal);
            Ok(text)
        }
    }

    fn key_event(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn send_key(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) {
        let input = [key_event(vk, flags)];
        unsafe {
            SendInput(&input, std::mem::size_of::<INPUT>() as i32);
        }
    }

    fn sleep_ms(ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }

    /// Simulate a single Return keypress via `SendInput`.
    pub fn press_return() -> Result<()> {
        send_key(VK_RETURN, KEYBD_EVENT_FLAGS(0));
        sleep_ms(5);
        send_key(VK_RETURN, KEYEVENTF_KEYUP);
        Ok(())
    }

    /// Simulate Ctrl+V via `SendInput`: Ctrl-down, V-down, V-up, Ctrl-up,
    /// with brief sleeps so the target app processes the events in order.
    /// Optionally follows with Return when `auto_enter` is set.
    pub fn paste_via_sendinput(auto_enter: bool) -> Result<()> {
        send_key(VK_CONTROL, KEYBD_EVENT_FLAGS(0));
        sleep_ms(10);
        send_key(VK_V, KEYBD_EVENT_FLAGS(0));
        sleep_ms(5);
        send_key(VK_V, KEYEVENTF_KEYUP);
        sleep_ms(10);
        send_key(VK_CONTROL, KEYEVENTF_KEYUP);

        if auto_enter {
            sleep_ms(50);
            press_return()?;
        }
        Ok(())
    }

    /// Whether the foreground window belongs to a known terminal emulator,
    /// judged by the owning process's executable name.
    #[allow(dead_code)]
    pub fn focused_window_is_terminal() -> bool {
        unsafe {
            let hwnd: HWND = GetForegroundWindow();
            if hwnd.0.is_null() {
                return false;
            }

            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 {
                return false;
            }

            let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return false;
            };

            let mut buffer = [0u16; 260];
            let mut len = buffer.len() as u32;
            let queried = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut len,
            );
            let _ = CloseHandle(process);
            if queried.is_err() || len == 0 {
                return false;
            }

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
}

// ─── Clipboard change-count helpers ─────────────────────────────────────

/// Read the current clipboard text (Windows: `CF_UNICODETEXT` via Win32).
#[cfg(target_os = "windows")]
pub fn read_clipboard_text() -> Result<String> {
    win32::read_clipboard_text()
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
    /// Touches the real general pasteboard, so ignored by default. Restores
    /// the prior text afterwards. Run: `cargo test --features local-stt
    /// --lib native_pasteboard_roundtrip -- --ignored --nocapture`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn native_pasteboard_roundtrip() {
        let prior = super::read_clipboard_text().unwrap_or_default();
        let sample = "Hand-back test — ünïcødé … 🎙️ done ".to_string();
        let mut ms = Vec::new();
        for _ in 0..20 {
            let t = std::time::Instant::now();
            assert!(super::native_pasteboard_write(&sample));
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let back = super::read_clipboard_text().unwrap_or_default();
        let _ = super::copy_to_clipboard(prior);
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "native write ms p50={:.3} p95={:.3} max={:.3}",
            ms[10], ms[18], ms[19]
        );
        assert_eq!(back, sample);
    }

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

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_paste_sends_balanced_command_flags_changed_events() {
        use core_graphics::event::{CGEvent, CGEventFlags, CGEventType, EventField, KeyCode};
        use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

        fn command_seen_by_flags_observer(events: &[&CGEvent]) -> (bool, bool) {
            let mut command_down_during_v = false;
            let mut command_down_after_sequence = false;
            let mut command_down = false;
            for event in events {
                if event.get_type() as u32 == CGEventType::FlagsChanged as u32 {
                    command_down = event.get_flags().contains(CGEventFlags::CGEventFlagCommand);
                }
                if event.get_type() as u32 == CGEventType::KeyDown as u32
                    && event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) == 9
                // 'v' key
                {
                    command_down_during_v = command_down;
                }
                command_down_after_sequence = command_down;
            }
            (command_down_during_v, command_down_after_sequence)
        }

        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).unwrap();
        let events = mac_paste_events(&source, false).unwrap();

        assert_eq!(
            events.command_down.get_type() as u32,
            CGEventType::FlagsChanged as u32
        );
        assert_eq!(
            events.command_up.get_type() as u32,
            CGEventType::FlagsChanged as u32
        );
        assert_eq!(
            events
                .command_down
                .get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE),
            i64::from(KeyCode::COMMAND)
        );
        assert_eq!(
            events
                .command_up
                .get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE),
            i64::from(KeyCode::COMMAND)
        );
        assert!(
            events
                .command_down
                .get_flags()
                .contains(CGEventFlags::CGEventFlagCommand)
        );
        assert!(
            !events
                .command_up
                .get_flags()
                .contains(CGEventFlags::CGEventFlagCommand)
        );

        // Old implementation: V events carry Command flags, but a UTM-style
        // observer that updates modifier state only for FlagsChanged sees plain v.
        let old_v_down = CGEvent::new_keyboard_event(source.clone(), 9, true).unwrap();
        old_v_down.set_flags(CGEventFlags::CGEventFlagCommand);
        let old_v_up = CGEvent::new_keyboard_event(source.clone(), 9, false).unwrap();
        old_v_up.set_flags(CGEventFlags::CGEventFlagCommand);
        assert_eq!(
            command_seen_by_flags_observer(&[&old_v_down, &old_v_up]),
            (false, false)
        );

        assert_eq!(
            command_seen_by_flags_observer(&[
                &events.command_down,
                &events.v_down,
                &events.v_up,
                &events.command_up,
            ]),
            (true, false)
        );
        assert!(events.enter.is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn wayland_injector_args_match_what_the_tools_accept() {
        for terminal in [false, true] {
            let [
                (wtype, wtype_paste, wtype_enter),
                (ydotool, ydo_paste, ydo_enter),
            ] = wayland_injectors(terminal);
            assert_eq!((wtype, ydotool), ("wtype", "ydotool"));
            // ydotool 1.0: `key` subcommand, then only CODE:STATE pairs.
            for args in [&ydo_paste, &ydo_enter] {
                assert_eq!(&args[..3], &["key", "-d", "3"]);
                for code in &args[3..] {
                    let (k, s) = code.split_once(':').expect("CODE:STATE");
                    assert!(k.parse::<u16>().is_ok() && (s == "0" || s == "1"), "{code}");
                }
            }
            assert_eq!(ydo_enter[3..], ["28:1", "28:0"]);
            assert_eq!(wtype_enter, ["-k", "Return"]);
            // Shift is in the chord exactly when the target is a terminal.
            assert_eq!(ydo_paste.contains(&"42:1"), terminal);
            assert_eq!(wtype_paste.contains(&"shift"), terminal);
        }
        let [_, (_, ydo, _)] = wayland_injectors(false);
        assert_eq!(ydo[3..], ["29:1", "47:1", "47:0", "29:0"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn hyprland_active_window_classification() {
        let name = |n: &'static str| move |_pid: i64| Some(format!("{n}\n"));
        let w = |class: &str| format!(r#"{{"class":"{class}","pid":42}}"#).into_bytes();
        assert_eq!(
            active_window_is_terminal(&w("Alacritty"), name("alacritty")),
            Some(true)
        );
        // Custom app-id, but the process is foot.
        assert_eq!(
            active_window_is_terminal(&w("tmux-minisx"), name("foot")),
            Some(true)
        );
        assert_eq!(
            active_window_is_terminal(&w("com.mitchellh.ghostty"), |_| None),
            Some(true)
        );
        assert_eq!(
            active_window_is_terminal(&w("chromium"), name("chromium")),
            Some(false)
        );
        // No focused window: hyprctl prints `{}`.
        assert_eq!(active_window_is_terminal(b"{}", |_| None), Some(false));
        assert_eq!(active_window_is_terminal(b"Invalid", |_| None), None);
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
