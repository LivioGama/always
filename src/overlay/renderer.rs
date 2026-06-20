//! X11 overlay renderer for Linux.
//!
//! The overlay is intentionally small and self-contained: a borderless,
//! always-on-top X11 window near the bottom center of the active display.
//! This implements the X11 backend for the overlay system.

use super::state::OverlayState;
use anyhow::{Context, Result};
use std::ffi::CString;
use std::os::raw::{c_int, c_long};
use std::time::Duration;
use tracing::info;
use x11::xlib::*;

const WINDOW_WIDTH: u32 = 420;
const WINDOW_HEIGHT: u32 = 68;
const BOTTOM_MARGIN: i32 = 160;
const TEXT_MAX_CHARS: usize = 58;

/// X11 overlay renderer - handles visual rendering on X11 sessions.
#[derive(Debug)]
pub struct X11Renderer {
    current_state: Option<OverlayState>,
    visible: bool,
    display: *mut Display,
    window: Window,
    gc: GC,
    width: u32,
    height: u32,
}

impl X11Renderer {
    /// Create a new X11 renderer.
    pub fn new() -> Result<Self> {
        info!("Initializing X11 overlay renderer");

        unsafe {
            let display = XOpenDisplay(std::ptr::null());
            if display.is_null() {
                anyhow::bail!(
                    "Failed to open X11 display. Linux overlay currently requires an X11 session."
                );
            }

            let screen = XDefaultScreen(display);
            let root = XRootWindow(display, screen);
            let screen_width = XDisplayWidth(display, screen);
            let screen_height = XDisplayHeight(display, screen);

            // Position at bottom center with margin
            let x = ((screen_width - WINDOW_WIDTH as i32) / 2).max(0);
            let y = (screen_height - WINDOW_HEIGHT as i32 - BOTTOM_MARGIN).max(0);

            let window = XCreateSimpleWindow(
                display,
                root,
                x,
                y,
                WINDOW_WIDTH,
                WINDOW_HEIGHT,
                0,
                0,
                0x0d1117, // Darker background (GitHub dark theme)
            );
            if window == 0 {
                XCloseDisplay(display);
                anyhow::bail!("Failed to create X11 overlay window");
            }

            let gc = XCreateGC(display, window, 0, std::ptr::null_mut());
            if gc.is_null() {
                XDestroyWindow(display, window);
                XCloseDisplay(display);
                anyhow::bail!("Failed to create X11 graphics context");
            }

            configure_window(display, window)?;
            XSelectInput(
                display,
                window,
                (ExposureMask | StructureNotifyMask) as c_long,
            );
            XUnmapWindow(display, window);
            XFlush(display);

            info!("X11 overlay window created: {} at ({}, {})", window, x, y);

            Ok(Self {
                current_state: None,
                visible: false,
                display,
                window,
                gc,
                width: WINDOW_WIDTH,
                height: WINDOW_HEIGHT,
            })
        }
    }

    /// Update the overlay state.
    pub async fn update_state(&mut self, new_state: OverlayState) -> Result<()> {
        self.current_state = Some(new_state.clone());
        if matches!(new_state, OverlayState::Hidden) {
            self.hide();
            return Ok(());
        }

        self.show(&new_state);
        Ok(())
    }

    /// Wait for renderer events.
    pub async fn wait_for_event(&mut self) -> Result<()> {
        unsafe {
            while XPending(self.display) > 0 {
                let mut event: XEvent = std::mem::zeroed();
                XNextEvent(self.display, &mut event);
                if event.get_type() == Expose
                    && let Some(state) = self.current_state.clone()
                    && !matches!(state, OverlayState::Hidden)
                {
                    self.draw(&state);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(())
    }
}

impl X11Renderer {
    fn show(&mut self, state: &OverlayState) {
        unsafe {
            if !self.visible {
                XMapRaised(self.display, self.window);
                self.visible = true;
            } else {
                XRaiseWindow(self.display, self.window);
            }
        }
        self.draw(state);
    }

    fn hide(&mut self) {
        if self.visible {
            unsafe {
                XUnmapWindow(self.display, self.window);
                XFlush(self.display);
            }
            self.visible = false;
        }
    }

    fn draw(&mut self, state: &OverlayState) {
        let accent = accent_color(state);
        let text = truncate_text(&state.display_text(), TEXT_MAX_CHARS);

        unsafe {
            // Draw background with rounded corners (simulated with rectangles)
            XSetForeground(self.display, self.gc, 0x0d1117);
            XFillRectangle(
                self.display,
                self.window,
                self.gc,
                0,
                0,
                self.width,
                self.height,
            );

            // Draw inner panel with rounded corners
            XSetForeground(self.display, self.gc, 0x161b22);
            XFillRectangle(
                self.display,
                self.window,
                self.gc,
                6,
                6,
                self.width - 12,
                self.height - 12,
            );

            // Draw state indicator circle
            XSetForeground(self.display, self.gc, accent);
            XFillArc(
                self.display,
                self.window,
                self.gc,
                22,
                22,
                24,
                24,
                0,
                360 * 64,
            );

            // Draw text
            XSetForeground(self.display, self.gc, 0xf0f6fc);
            if let Ok(c_text) = CString::new(text) {
                XDrawString(
                    self.display,
                    self.window,
                    self.gc,
                    58,
                    41,
                    c_text.as_ptr(),
                    c_text.as_bytes().len() as c_int,
                );
            }

            XFlush(self.display);
        }
    }
}

impl Drop for X11Renderer {
    fn drop(&mut self) {
        unsafe {
            if !self.gc.is_null() {
                XFreeGC(self.display, self.gc);
            }
            if self.window != 0 {
                XDestroyWindow(self.display, self.window);
            }
            if !self.display.is_null() {
                XCloseDisplay(self.display);
            }
        }
    }
}

fn configure_window(display: *mut Display, window: Window) -> Result<()> {
    unsafe {
        // Set window type to notification (more compatible than dock)
        let window_type_atom = intern_atom(display, "_NET_WM_WINDOW_TYPE")?;
        let notification_atom = intern_atom(display, "_NET_WM_WINDOW_TYPE_NOTIFICATION")?;
        set_atom_property(display, window, window_type_atom, &[notification_atom]);

        // Set window state: always on top, sticky, skip taskbar/pager
        let state_atom = intern_atom(display, "_NET_WM_STATE")?;
        let above_atom = intern_atom(display, "_NET_WM_STATE_ABOVE")?;
        let sticky_atom = intern_atom(display, "_NET_WM_STATE_STICKY")?;
        let skip_taskbar_atom = intern_atom(display, "_NET_WM_STATE_SKIP_TASKBAR")?;
        let skip_pager_atom = intern_atom(display, "_NET_WM_STATE_SKIP_PAGER")?;
        let skip_focus_atom = intern_atom(display, "_NET_WM_STATE_SKIP_FOCUS")?;
        set_atom_property(
            display,
            window,
            state_atom,
            &[
                above_atom,
                sticky_atom,
                skip_taskbar_atom,
                skip_pager_atom,
                skip_focus_atom,
            ],
        );

        // Remove decorations (no title bar, borders)
        let motif_atom = intern_atom(display, "_MOTIF_WM_HINTS")?;
        let hints: [usize; 5] = [2, 0, 0, 0, 0]; // MWM_HINTS_DECORATIONS
        XChangeProperty(
            display,
            window,
            motif_atom,
            motif_atom,
            32,
            PropModeReplace,
            hints.as_ptr() as *const u8,
            hints.len() as c_int,
        );

        // Set window name
        let name = CString::new("Always Overlay").context("overlay window name contains NUL")?;
        XStoreName(display, window, name.as_ptr());

        // Set window class
        let class = CString::new("always-overlay").context("window class contains NUL")?;
        let mut class_hint = XClassHint {
            res_name: name.as_ptr() as *mut i8,
            res_class: class.as_ptr() as *mut i8,
        };
        XSetClassHint(display, window, &mut class_hint);

        // Set input hint to false (window should not receive keyboard focus)
        let mut wm_hints: XWMHints = std::mem::zeroed();
        wm_hints.flags = InputHint;
        wm_hints.input = 0; // False
        XSetWMHints(display, window, &mut wm_hints);
    }
    Ok(())
}

fn intern_atom(display: *mut Display, name: &str) -> Result<Atom> {
    let c_name = CString::new(name).with_context(|| format!("atom name contains NUL: {name}"))?;
    let atom = unsafe { XInternAtom(display, c_name.as_ptr(), 0) };
    if atom == 0 {
        anyhow::bail!("XInternAtom failed for {name}");
    }
    Ok(atom)
}

fn set_atom_property(display: *mut Display, window: Window, property: Atom, atoms: &[Atom]) {
    unsafe {
        XChangeProperty(
            display,
            window,
            property,
            XA_ATOM,
            32,
            PropModeReplace,
            atoms.as_ptr() as *const u8,
            atoms.len() as c_int,
        );
    }
}

fn accent_color(state: &OverlayState) -> u64 {
    match state {
        OverlayState::VoiceActivity => 0xff5f57,             // Red
        OverlayState::Transcribing => 0xbf8cff,              // Blue
        OverlayState::AutoEnterCountdown { .. } => 0xffd866, // Yellow
        OverlayState::Paused | OverlayState::IdleAutoPaused { .. } => 0xf4a261, // Orange
        OverlayState::Resumed | OverlayState::AutoEnterOn | OverlayState::GrammarCorrected => {
            0x50fa7b // Green
        }
        OverlayState::AutoEnterOff => 0x8b949e,    // Gray
        OverlayState::Filtered { .. } => 0xff79c6, // Pink
        OverlayState::TranscriptionFailed { .. } | OverlayState::LowMicrophoneVolume { .. } => {
            0xff5555 // Bright red
        }
        OverlayState::Hidden => 0x8b949e, // Gray
    }
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let mut out = String::new();
    for _ in 0..max_chars {
        let Some(ch) = chars.next() else {
            return out;
        };
        out.push(ch);
    }
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_text_short() {
        let text = "Hello";
        let result = truncate_text(text, 10);
        assert_eq!(result, "Hello");
    }

    #[test]
    fn test_truncate_text_exact() {
        let text = "Hello";
        let result = truncate_text(text, 5);
        assert_eq!(result, "Hello");
    }

    #[test]
    fn test_truncate_text_long() {
        let text = "Hello World";
        let result = truncate_text(text, 5);
        assert_eq!(result, "Hello...");
    }

    #[test]
    fn test_truncate_text_empty() {
        let text = "";
        let result = truncate_text(text, 10);
        assert_eq!(result, "");
    }

    #[test]
    fn test_truncate_text_unicode() {
        let text = "Hello 世界";
        let result = truncate_text(text, 8);
        assert_eq!(result, "Hello 世界");
    }

    #[test]
    fn test_truncate_text_unicode_long() {
        let text = "Hello 世界";
        let result = truncate_text(text, 7);
        assert_eq!(result, "Hello 世...");
    }
}
