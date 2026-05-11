//! Clipboard + paste abstraction.
//!
//! The macOS implementation shells out to `pbcopy` for clipboard write and
//! posts synthetic Cmd+V / Return events via Core Graphics. The trait
//! ([`ClipboardProvider`]) exists so the `event_loop` can be exercised
//! end-to-end in tests without actually touching the user's pasteboard,
//! and so future Linux/Windows backends can plug in without rewriting
//! callers.
//!
//! [`MockClipboardProvider`] (gated behind `cfg(test)`) records every
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

pub fn copy_to_clipboard(text: String) -> Result<()> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let mut pbcopy = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .context("Failed to run pbcopy")?;
    pbcopy
        .stdin
        .as_mut()
        .context("Failed to open pbcopy stdin")?
        .write_all(text.as_bytes())
        .context("Failed to write transcript to clipboard")?;
    let status = pbcopy.wait().context("Failed waiting for pbcopy")?;
    if !status.success() {
        anyhow::bail!("pbcopy failed");
    }
    Ok(())
}

#[cfg(feature = "macos")]
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
        std::thread::sleep(std::time::Duration::from_millis(20));
        let enter_keycode: CGKeyCode = 36; // Return key
        let enter_down = CGEvent::new_keyboard_event(source.clone(), enter_keycode, true)
            .map_err(|_| anyhow::anyhow!("Failed to create enter key down event"))?;
        enter_down.post(CGEventTapLocation::HID);

        let enter_up = CGEvent::new_keyboard_event(source, enter_keycode, false)
            .map_err(|_| anyhow::anyhow!("Failed to create enter key up event"))?;
        enter_up.post(CGEventTapLocation::HID);
    }

    Ok(())
}

#[cfg(not(feature = "macos"))]
pub fn paste_text(_auto_enter: bool) -> Result<()> {
    anyhow::bail!(
        "paste_text is not implemented for this platform yet; \
         macOS uses Core Graphics, Linux/Windows are scheduled for a future release"
    )
}

pub fn paste(text: &str, auto_enter: bool) -> Result<()> {
    copy_to_clipboard(text.to_string())?;
    paste_text(auto_enter)?;
    Ok(())
}

/// Synthesize a single Return keypress in the focused app. Used to
/// commit auto-enter after a countdown overlay completes (vs.
/// inline in `paste_text(auto_enter=true)`).
#[cfg(feature = "macos")]
pub fn press_return() -> Result<()> {
    use core_graphics::event::{CGEvent, CGEventTapLocation, CGKeyCode};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| anyhow::anyhow!("Failed to create CGEventSource"))?;
    let enter_keycode: CGKeyCode = 36;
    let enter_down = CGEvent::new_keyboard_event(source.clone(), enter_keycode, true)
        .map_err(|_| anyhow::anyhow!("Failed to create enter key down event"))?;
    enter_down.post(CGEventTapLocation::HID);
    let enter_up = CGEvent::new_keyboard_event(source, enter_keycode, false)
        .map_err(|_| anyhow::anyhow!("Failed to create enter key up event"))?;
    enter_up.post(CGEventTapLocation::HID);
    Ok(())
}

#[cfg(not(feature = "macos"))]
pub fn press_return() -> Result<()> {
    anyhow::bail!("press_return not implemented for this platform")
}

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
