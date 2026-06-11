//! X11 overlay renderer for Linux.
//!
//! The overlay is intentionally small and self-contained: a borderless,
//! always-on-top X11 window near the bottom center of the active display.
//! Wayland compositors need a layer-shell backend; this renderer targets X11.

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

/// Overlay renderer - handles visual rendering.
pub struct OverlayRenderer {
    current_state: Option<OverlayState>,
    visible: bool,
    display: *mut Display,
    window: Window,
    gc: GC,
    width: u32,
    height: u32,
}

impl OverlayRenderer {
    /// Create a new overlay renderer.
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
                0x1f2328,
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
                if event.get_type() == Expose {
                    if let Some(state) = self.current_state.clone() {
                        if !matches!(state, OverlayState::Hidden) {
                            self.draw(&state);
                        }
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(())
    }

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
            XSetForeground(self.display, self.gc, 0x1f2328);
            XFillRectangle(
                self.display,
                self.window,
                self.gc,
                0,
                0,
                self.width,
                self.height,
            );

            XSetForeground(self.display, self.gc, 0x2d333b);
            XFillRectangle(
                self.display,
                self.window,
                self.gc,
                6,
                6,
                self.width - 12,
                self.height - 12,
            );

            XSetForeground(self.display, self.gc, accent);
            XFillArc(
                self.display,
                self.window,
                self.gc,
                22,
                22,
                22,
                22,
                0,
                360 * 64,
            );

            XSetForeground(self.display, self.gc, 0xf6f8fa);
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

impl Drop for OverlayRenderer {
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
        let window_type_atom = intern_atom(display, "_NET_WM_WINDOW_TYPE")?;
        let notification_atom = intern_atom(display, "_NET_WM_WINDOW_TYPE_NOTIFICATION")?;
        set_atom_property(display, window, window_type_atom, &[notification_atom]);

        let state_atom = intern_atom(display, "_NET_WM_STATE")?;
        let above_atom = intern_atom(display, "_NET_WM_STATE_ABOVE")?;
        let sticky_atom = intern_atom(display, "_NET_WM_STATE_STICKY")?;
        let skip_taskbar_atom = intern_atom(display, "_NET_WM_STATE_SKIP_TASKBAR")?;
        let skip_pager_atom = intern_atom(display, "_NET_WM_STATE_SKIP_PAGER")?;
        set_atom_property(
            display,
            window,
            state_atom,
            &[above_atom, sticky_atom, skip_taskbar_atom, skip_pager_atom],
        );

        let motif_atom = intern_atom(display, "_MOTIF_WM_HINTS")?;
        let hints: [usize; 5] = [2, 0, 0, 0, 0];
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

        let name = CString::new("Always Overlay").context("overlay window name contains NUL")?;
        XStoreName(display, window, name.as_ptr());
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
        OverlayState::VoiceActivity => 0xff5f57,
        OverlayState::Transcribing => 0xbf8cff,
        OverlayState::AutoEnterCountdown { .. } => 0xffd866,
        OverlayState::Paused | OverlayState::IdleAutoPaused { .. } => 0xf4a261,
        OverlayState::Resumed | OverlayState::AutoEnterOn | OverlayState::GrammarCorrected => {
            0x50fa7b
        }
        OverlayState::AutoEnterOff => 0x8b949e,
        OverlayState::Filtered { .. } => 0xff79c6,
        OverlayState::TranscriptionFailed { .. } | OverlayState::LowMicrophoneVolume { .. } => {
            0xff5555
        }
        OverlayState::Hidden => 0x8b949e,
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
