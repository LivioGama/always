//! The microphone dictation is captured from — the system default input
//! device — as a stable identity (CoreAudio device UID) plus the name the
//! user sees in System Settings → Sound → Input.
//!
//! My Voice keeps one voiceprint per microphone (§6 of SPEC.md): the same
//! voice through a studio mic and through the laptop's built-in mic gives
//! embeddings far enough apart that a voiceprint from one rejects the user on
//! the other (2026-11-27 prod: median score 0.45 on the Elgato the profile
//! was recorded on, 0.16 on the MacBook Pro Microphone, 0 of 75 utterances
//! typed). The gate therefore needs to know which mic is live.
//!
//! The answer is cached for [`CACHE_TTL`], so the per-utterance gate pays a
//! CoreAudio property read at most once per TTL; [`refresh`] forces a re-read
//! when the GUI reports a default-input change.

use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// A capture device as the user knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    /// Stable across reboots and reconnects (CoreAudio `kAudioDevicePropertyDeviceUID`).
    pub uid: String,
    /// Display name, e.g. "MacBook Pro Microphone".
    pub name: String,
}

/// How long a read stays valid. A switch is picked up at once when the GUI
/// sends `RespawnRecorder`, and within this TTL even if it does not.
const CACHE_TTL: Duration = Duration::from_secs(2);

static CACHE: Mutex<Option<(Instant, Option<InputDevice>)>> = Mutex::new(None);

/// Serialises read-then-store, so a slow read started before a device switch
/// can never overwrite a newer forced [`refresh`] with the old device.
static READ: Mutex<()> = Mutex::new(());

/// Test/override hook: when set, [`current`] returns this instead of asking
/// CoreAudio. `Some(None)` simulates "device unknown".
#[cfg(test)]
static OVERRIDE: Mutex<Option<Option<InputDevice>>> = Mutex::new(None);

#[cfg(test)]
pub fn set_override(device: Option<Option<InputDevice>>) {
    *OVERRIDE.lock() = device;
    *CACHE.lock() = None;
}

/// The current default input device, or `None` when it cannot be read
/// (non-macOS, no input device, CoreAudio error).
pub fn current() -> Option<InputDevice> {
    #[cfg(test)]
    if let Some(dev) = OVERRIDE.lock().clone() {
        return dev;
    }
    {
        let cache = CACHE.lock();
        if let Some((at, dev)) = cache.as_ref()
            && at.elapsed() < CACHE_TTL
        {
            return dev.clone();
        }
    }
    refresh()
}

/// Re-read the default input device now and cache it.
pub fn refresh() -> Option<InputDevice> {
    #[cfg(test)]
    if let Some(dev) = OVERRIDE.lock().clone() {
        return dev;
    }
    let _read = READ.lock();
    let dev = read_default_input();
    let previous = CACHE.lock().replace((Instant::now(), dev.clone()));
    let changed = previous.map(|(_, d)| d) != Some(dev.clone());
    if changed {
        tracing::info!(
            uid = dev.as_ref().map(|d| d.uid.as_str()).unwrap_or(""),
            name = dev.as_ref().map(|d| d.name.as_str()).unwrap_or(""),
            "input_device_current"
        );
    }
    dev
}

#[cfg(target_os = "macos")]
fn read_default_input() -> Option<InputDevice> {
    coreaudio::default_input()
}

#[cfg(not(target_os = "macos"))]
fn read_default_input() -> Option<InputDevice> {
    None
}

#[cfg(target_os = "macos")]
mod coreaudio {
    use super::InputDevice;
    use std::ffi::c_void;

    #[repr(C)]
    struct AudioObjectPropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    const fn fourcc(b: &[u8; 4]) -> u32 {
        ((b[0] as u32) << 24) | ((b[1] as u32) << 16) | ((b[2] as u32) << 8) | (b[3] as u32)
    }

    const SYSTEM_OBJECT: u32 = 1; // kAudioObjectSystemObject
    const SCOPE_GLOBAL: u32 = fourcc(b"glob");
    const ELEMENT_MAIN: u32 = 0;
    const PROP_DEFAULT_INPUT: u32 = fourcc(b"dIn "); // kAudioHardwarePropertyDefaultInputDevice
    const PROP_DEVICE_UID: u32 = fourcc(b"uid "); // kAudioDevicePropertyDeviceUID
    const PROP_NAME: u32 = fourcc(b"lnam"); // kAudioObjectPropertyName
    const UNKNOWN_OBJECT: u32 = 0; // kAudioObjectUnknown
    const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectGetPropertyData(
            object_id: u32,
            address: *const AudioObjectPropertyAddress,
            qualifier_size: u32,
            qualifier_data: *const c_void,
            io_size: *mut u32,
            out_data: *mut c_void,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringGetCString(
            string: *const c_void,
            buffer: *mut u8,
            buffer_size: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(cf: *const c_void);
    }

    fn addr(selector: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            selector,
            scope: SCOPE_GLOBAL,
            element: ELEMENT_MAIN,
        }
    }

    fn default_input_id() -> Option<u32> {
        let address = addr(PROP_DEFAULT_INPUT);
        let mut id: u32 = UNKNOWN_OBJECT;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: reads one AudioObjectID into a correctly sized u32.
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                (&raw mut id).cast(),
            )
        };
        (status == 0 && id != UNKNOWN_OBJECT).then_some(id)
    }

    /// Read a CFString-valued property; the returned CFString is owned by
    /// the caller (CoreAudio "copy" semantics) and released here.
    fn string_property(object: u32, selector: u32) -> Option<String> {
        let address = addr(selector);
        let mut cf: *const c_void = std::ptr::null();
        let mut size = std::mem::size_of::<*const c_void>() as u32;
        // SAFETY: the property is a CFStringRef; we pass a pointer-sized slot.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                (&raw mut cf).cast(),
            )
        };
        if status != 0 || cf.is_null() {
            return None;
        }
        let mut buf = [0u8; 512];
        // SAFETY: `cf` is a valid CFString until released below.
        let ok = unsafe {
            CFStringGetCString(
                cf,
                buf.as_mut_ptr(),
                buf.len() as isize,
                CF_STRING_ENCODING_UTF8,
            )
        };
        // SAFETY: we own the copied CFString.
        unsafe { CFRelease(cf) };
        if ok == 0 {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        let s = String::from_utf8_lossy(&buf[..end]).into_owned();
        (!s.is_empty()).then_some(s)
    }

    pub fn default_input() -> Option<InputDevice> {
        let id = default_input_id()?;
        let uid = string_property(id, PROP_DEVICE_UID)?;
        let name = string_property(id, PROP_NAME).unwrap_or_else(|| uid.clone());
        Some(InputDevice { uid, name })
    }
}
