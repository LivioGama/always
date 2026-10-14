//! Native text-editing primitives for voice editing: Accessibility (AX)
//! reads/writes on the focused text element, plus synthesised keystrokes
//! as the fallback for fields that expose no AX text (terminals, some
//! Electron views).
//!
//! AX ranges are UTF-16 code units. Everything here speaks UTF-16 offsets;
//! [`u16_to_byte`] / [`byte_to_u16`] convert at the edges.
//!
//! The FFI is declared locally (ApplicationServices + CoreFoundation are
//! system frameworks) so no new crate dependency is needed.

/// Convert a byte offset in `s` to a UTF-16 offset.
pub fn byte_to_u16(s: &str, byte: usize) -> usize {
    s[..byte.min(s.len())].encode_utf16().count()
}

/// Convert a UTF-16 offset in `s` to a byte offset (clamped, char-aligned).
pub fn u16_to_byte(s: &str, u16_off: usize) -> usize {
    let mut n = 0usize;
    for (b, ch) in s.char_indices() {
        if n >= u16_off {
            return b;
        }
        n += ch.len_utf16();
    }
    s.len()
}

pub fn len_u16(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Where synthesised keystrokes go. The daemon uses `Hid` (the focused
/// app — same path as the existing ⌘V paste); tests aim at one pid so a
/// focus change can never leak keys into another window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyTarget {
    Hid,
    Pid(i32),
}

pub mod keys {
    pub const DELETE: u16 = 51; // Backspace
    pub const RETURN: u16 = 36;
    pub const LEFT: u16 = 123;
    pub const UP: u16 = 126;
    pub const DOWN: u16 = 125;
    pub const Z: u16 = 6;
    pub const A: u16 = 0;
    pub const E: u16 = 14;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Mods {
    pub const NONE: Mods = Mods { cmd: false, shift: false, alt: false, ctrl: false };
    pub const CMD: Mods = Mods { cmd: true, shift: false, alt: false, ctrl: false };
    pub const SHIFT: Mods = Mods { cmd: false, shift: true, alt: false, ctrl: false };
    pub const ALT: Mods = Mods { cmd: false, shift: false, alt: true, ctrl: false };
    pub const CTRL: Mods = Mods { cmd: false, shift: false, alt: false, ctrl: true };
    pub const CMD_SHIFT: Mods = Mods { cmd: true, shift: true, alt: false, ctrl: false };
    pub const ALT_SHIFT: Mods = Mods { cmd: false, shift: true, alt: true, ctrl: false };
}

/// One keystroke step of a fallback plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stroke {
    Key { code: u16, mods: Mods, repeat: usize },
    Text(String),
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use std::ffi::c_void;

    type CFTypeRef = *const c_void;
    type CFIndex = isize;

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    struct CFRange {
        location: CFIndex,
        length: CFIndex,
    }

    const K_AX_VALUE_CFRANGE_TYPE: u32 = 4;
    const AX_SUCCESS: i32 = 0;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXUIElementCreateSystemWide() -> CFTypeRef;
        fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
        fn AXUIElementCopyAttributeValue(e: CFTypeRef, attr: CFTypeRef, out: *mut CFTypeRef) -> i32;
        fn AXUIElementSetAttributeValue(e: CFTypeRef, attr: CFTypeRef, value: CFTypeRef) -> i32;
        fn AXUIElementIsAttributeSettable(e: CFTypeRef, attr: CFTypeRef, out: *mut u8) -> i32;
        fn AXUIElementGetPid(e: CFTypeRef, out: *mut i32) -> i32;
        fn AXUIElementSetMessagingTimeout(e: CFTypeRef, seconds: f32) -> i32;
        fn AXValueCreate(ty: u32, ptr: *const c_void) -> CFTypeRef;
        fn AXValueGetValue(v: CFTypeRef, ty: u32, ptr: *mut c_void) -> u8;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: CFTypeRef);
        fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
        fn CFEqual(a: CFTypeRef, b: CFTypeRef) -> u8;
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFStringCreateWithCharacters(alloc: *const c_void, chars: *const u16, n: CFIndex) -> CFTypeRef;
        fn CFStringGetLength(s: CFTypeRef) -> CFIndex;
        fn CFStringGetCharacters(s: CFTypeRef, range: CFRange, buf: *mut u16);
    }

    /// Owned CF object (released on drop).
    struct Cf(CFTypeRef);
    impl Drop for Cf {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) }
            }
        }
    }

    fn cfstr(s: &str) -> Cf {
        let u: Vec<u16> = s.encode_utf16().collect();
        Cf(unsafe { CFStringCreateWithCharacters(std::ptr::null(), u.as_ptr(), u.len() as CFIndex) })
    }

    fn string_of(cf: CFTypeRef) -> Option<String> {
        if cf.is_null() || unsafe { CFGetTypeID(cf) != CFStringGetTypeID() } {
            return None;
        }
        let n = unsafe { CFStringGetLength(cf) };
        let mut buf = vec![0u16; n as usize];
        unsafe { CFStringGetCharacters(cf, CFRange { location: 0, length: n }, buf.as_mut_ptr()) };
        Some(String::from_utf16_lossy(&buf))
    }

    /// A retained `AXUIElementRef`. `Send` because AX element refs are
    /// plain CF objects safe to message from any thread.
    pub struct AxElem(CFTypeRef);
    unsafe impl Send for AxElem {}
    unsafe impl Sync for AxElem {}

    impl Clone for AxElem {
        fn clone(&self) -> Self {
            AxElem(unsafe { CFRetain(self.0) })
        }
    }
    impl Drop for AxElem {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) }
        }
    }
    impl std::fmt::Debug for AxElem {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "AxElem(pid={:?}, role={:?})", self.pid(), self.role())
        }
    }

    fn copy_attr(e: CFTypeRef, attr: &str) -> Option<Cf> {
        let a = cfstr(attr);
        let mut out: CFTypeRef = std::ptr::null();
        let err = unsafe { AXUIElementCopyAttributeValue(e, a.0, &mut out) };
        if err != AX_SUCCESS || out.is_null() {
            return None;
        }
        Some(Cf(out))
    }

    /// Focused UI element system-wide (what the user is typing into).
    pub fn focused_element() -> Option<AxElem> {
        let sys = Cf(unsafe { AXUIElementCreateSystemWide() });
        unsafe { AXUIElementSetMessagingTimeout(sys.0, 0.25) };
        let f = copy_attr(sys.0, "AXFocusedUIElement")?;
        let e = AxElem(unsafe { CFRetain(f.0) });
        unsafe { AXUIElementSetMessagingTimeout(e.0, 0.25) };
        Some(e)
    }

    /// Focused element inside one app (used by the live test so it never
    /// depends on which app is frontmost).
    pub fn focused_element_of(pid: i32) -> Option<AxElem> {
        let app = Cf(unsafe { AXUIElementCreateApplication(pid) });
        unsafe { AXUIElementSetMessagingTimeout(app.0, 0.25) };
        let f = copy_attr(app.0, "AXFocusedUIElement")?;
        Some(AxElem(unsafe { CFRetain(f.0) }))
    }

    impl AxElem {
        pub fn same_as(&self, other: &AxElem) -> bool {
            unsafe { CFEqual(self.0, other.0) != 0 }
        }
        pub fn pid(&self) -> Option<i32> {
            let mut pid = 0;
            (unsafe { AXUIElementGetPid(self.0, &mut pid) } == AX_SUCCESS).then_some(pid)
        }
        fn str_attr(&self, attr: &str) -> Option<String> {
            copy_attr(self.0, attr).and_then(|v| string_of(v.0))
        }
        pub fn role(&self) -> Option<String> {
            self.str_attr("AXRole")
        }
        pub fn subrole(&self) -> Option<String> {
            self.str_attr("AXSubrole")
        }
        /// Password / secure fields: never read, never edited.
        pub fn is_secure(&self) -> bool {
            self.subrole().as_deref() == Some("AXSecureTextField")
                || self.role().as_deref() == Some("AXSecureTextField")
        }
        pub fn value(&self) -> Option<String> {
            self.str_attr("AXValue")
        }
        /// Title of the window containing this element.
        pub fn window_title(&self) -> Option<String> {
            let w = copy_attr(self.0, "AXWindow")?;
            copy_attr(w.0, "AXTitle").and_then(|t| string_of(t.0))
        }
        pub fn selected_text(&self) -> Option<String> {
            self.str_attr("AXSelectedText")
        }
        /// (location, length) in UTF-16 units.
        pub fn selected_range(&self) -> Option<(usize, usize)> {
            let v = copy_attr(self.0, "AXSelectedTextRange")?;
            let mut r = CFRange::default();
            let ok = unsafe {
                AXValueGetValue(v.0, K_AX_VALUE_CFRANGE_TYPE, (&mut r as *mut CFRange).cast())
            };
            (ok != 0 && r.location >= 0 && r.length >= 0)
                .then_some((r.location as usize, r.length as usize))
        }
        pub fn settable(&self, attr: &str) -> bool {
            let a = cfstr(attr);
            let mut out: u8 = 0;
            unsafe { AXUIElementIsAttributeSettable(self.0, a.0, &mut out) == AX_SUCCESS && out != 0 }
        }
        pub fn set_selected_range(&self, loc: usize, len: usize) -> bool {
            let r = CFRange { location: loc as CFIndex, length: len as CFIndex };
            let v = Cf(unsafe {
                AXValueCreate(K_AX_VALUE_CFRANGE_TYPE, (&r as *const CFRange).cast())
            });
            if v.0.is_null() {
                return false;
            }
            let a = cfstr("AXSelectedTextRange");
            unsafe { AXUIElementSetAttributeValue(self.0, a.0, v.0) == AX_SUCCESS }
        }
        /// Replace the current selection with `text` — one undoable edit in
        /// Cocoa text views.
        pub fn set_selected_text(&self, text: &str) -> bool {
            let a = cfstr("AXSelectedText");
            let s = cfstr(text);
            unsafe { AXUIElementSetAttributeValue(self.0, a.0, s.0) == AX_SUCCESS }
        }
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceCreate(state: i32) -> CFTypeRef;
        fn CGEventCreateKeyboardEvent(src: CFTypeRef, key: u16, down: bool) -> CFTypeRef;
        fn CGEventSetFlags(ev: CFTypeRef, flags: u64);
        fn CGEventKeyboardSetUnicodeString(ev: CFTypeRef, len: usize, s: *const u16);
        fn CGEventPost(tap: u32, ev: CFTypeRef);
        fn CGEventPostToPid(pid: i32, ev: CFTypeRef);
    }
    const HID_SYSTEM_STATE: i32 = 1;
    const HID_EVENT_TAP: u32 = 0;
    const FLAG_SHIFT: u64 = 0x0002_0000;
    const FLAG_CTRL: u64 = 0x0004_0000;
    const FLAG_ALT: u64 = 0x0008_0000;
    const FLAG_CMD: u64 = 0x0010_0000;

    pub fn post_strokes(target: KeyTarget, strokes: &[Stroke]) -> anyhow::Result<()> {
        let src = Cf(unsafe { CGEventSourceCreate(HID_SYSTEM_STATE) });
        if src.0.is_null() {
            anyhow::bail!("CGEventSourceCreate failed");
        }
        let post = |key: u16, down: bool, flags: u64, text: Option<&[u16]>| -> anyhow::Result<()> {
            let ev = Cf(unsafe { CGEventCreateKeyboardEvent(src.0, key, down) });
            if ev.0.is_null() {
                anyhow::bail!("CGEventCreateKeyboardEvent failed");
            }
            unsafe {
                CGEventSetFlags(ev.0, flags);
                if let Some(t) = text {
                    CGEventKeyboardSetUnicodeString(ev.0, t.len(), t.as_ptr());
                }
                match target {
                    KeyTarget::Hid => CGEventPost(HID_EVENT_TAP, ev.0),
                    KeyTarget::Pid(pid) => CGEventPostToPid(pid, ev.0),
                }
            }
            Ok(())
        };
        for s in strokes {
            match s {
                Stroke::Key { code, mods, repeat } => {
                    let mut flags = 0u64;
                    if mods.cmd {
                        flags |= FLAG_CMD;
                    }
                    if mods.shift {
                        flags |= FLAG_SHIFT;
                    }
                    if mods.alt {
                        flags |= FLAG_ALT;
                    }
                    if mods.ctrl {
                        flags |= FLAG_CTRL;
                    }
                    for _ in 0..*repeat {
                        post(*code, true, flags, None)?;
                        post(*code, false, flags, None)?;
                    }
                }
                Stroke::Text(t) => {
                    // Unicode keyboard events carry ≤ 20 UTF-16 units each.
                    let u: Vec<u16> = t.encode_utf16().collect();
                    for chunk in u.chunks(20) {
                        post(0, true, 0, Some(chunk))?;
                        post(0, false, 0, Some(chunk))?;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub use mac::{AxElem, focused_element, focused_element_of, post_strokes};

#[cfg(not(target_os = "macos"))]
mod stub {
    use super::*;
    #[derive(Debug, Clone)]
    pub struct AxElem;
    impl AxElem {
        pub fn same_as(&self, _: &AxElem) -> bool { false }
        pub fn pid(&self) -> Option<i32> { None }
        pub fn role(&self) -> Option<String> { None }
        pub fn is_secure(&self) -> bool { false }
        pub fn value(&self) -> Option<String> { None }
        pub fn window_title(&self) -> Option<String> { None }
        pub fn selected_text(&self) -> Option<String> { None }
        pub fn selected_range(&self) -> Option<(usize, usize)> { None }
        pub fn settable(&self, _: &str) -> bool { false }
        pub fn set_selected_range(&self, _: usize, _: usize) -> bool { false }
        pub fn set_selected_text(&self, _: &str) -> bool { false }
    }
    pub fn focused_element() -> Option<AxElem> { None }
    pub fn focused_element_of(_: i32) -> Option<AxElem> { None }
    pub fn post_strokes(_: KeyTarget, _: &[Stroke]) -> anyhow::Result<()> {
        anyhow::bail!("keystrokes not implemented on this platform")
    }
}
#[cfg(not(target_os = "macos"))]
pub use stub::{AxElem, focused_element, focused_element_of, post_strokes};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_offsets_round_trip() {
        let s = "a😀b é";
        assert_eq!(len_u16(s), 6);
        assert_eq!(byte_to_u16(s, 5), 3); // after the emoji
        assert_eq!(u16_to_byte(s, 3), 5);
        assert_eq!(u16_to_byte(s, 99), s.len());
    }
}
