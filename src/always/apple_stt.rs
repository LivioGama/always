//! Safe Rust wrapper around the Apple on-device STT (SFSpeechRecognizer)
//! bridge. The Swift side (`swift/apple_stt.swift`) exposes C functions;
//! this module wraps them in safe Rust types.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::Path;

#[repr(C)]
struct AppleSttResponse {
    text: *mut c_char,
    success: c_int,
    error_message: *mut c_char,
}

unsafe extern "C" {
    fn is_apple_stt_available() -> c_int;
    fn transcribe_wav_with_apple_stt(
        wav_path: *const c_char,
        lang: *const c_char,
        phrases: *const c_char,
    ) -> *mut AppleSttResponse;
    fn free_apple_stt_result(response: *mut AppleSttResponse);
    fn apple_stt_stream_start(lang: *const c_char, phrases: *const c_char) -> *mut c_void;
    fn apple_stt_stream_feed(handle: *mut c_void, samples: *const i16, count: c_int);
    fn apple_stt_stream_partial(handle: *mut c_void, seen: *const c_char) -> *mut c_char;
    fn apple_stt_stream_finish(handle: *mut c_void, timeout_ms: c_int) -> *mut c_char;
    fn apple_stt_stream_free(handle: *mut c_void);
    fn free_apple_stt_string(ptr: *mut c_char);
}

/// True when on-device Apple speech recognition is available and
/// authorized on this device and build.
pub fn check_availability() -> bool {
    unsafe { is_apple_stt_available() == 1 }
}

/// Transcribe a WAV file using Apple's speech engine. `lang` is an optional
/// ISO 639-1 hint (e.g. "en"); `None` or empty lets the recognizer use the
/// current locale. `phrases` are domain terms joined into a newline-separated
/// `contextualStrings` bias list on the Swift side.
///
/// Returns the raw transcribed text on success, or a human-readable error
/// string on failure.
pub fn transcribe_wav(
    path: &Path,
    lang: Option<&str>,
    phrases: &[String],
) -> Result<String, String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| "Apple STT path is not valid UTF-8".to_string())?;
    let path_cstr = CString::new(path_str).map_err(|e| e.to_string())?;
    let lang_cstr = lang
        .filter(|l| !l.is_empty())
        .and_then(|l| CString::new(l).ok());
    let phrases_cstr = CString::new(phrases.join("\n")).ok();

    let lang_ptr = lang_cstr.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let phrases_ptr = phrases_cstr
        .as_ref()
        .map_or(std::ptr::null(), |c| c.as_ptr());

    let response_ptr =
        unsafe { transcribe_wav_with_apple_stt(path_cstr.as_ptr(), lang_ptr, phrases_ptr) };
    if response_ptr.is_null() {
        return Err("Apple STT returned a null response".to_string());
    }

    let result = unsafe {
        let response = &*response_ptr;
        if response.success == 1 {
            if response.text.is_null() {
                Ok(String::new())
            } else {
                let c_str = CStr::from_ptr(response.text);
                Ok(c_str.to_string_lossy().into_owned())
            }
        } else {
            let msg = if !response.error_message.is_null() {
                CStr::from_ptr(response.error_message)
                    .to_string_lossy()
                    .into_owned()
            } else {
                "Unknown Apple STT error".to_string()
            };
            Err(msg)
        }
    };

    unsafe { free_apple_stt_result(response_ptr) };
    result
}

/// One live dictation session backed by SFSpeechRecognizer's buffer
/// request. Partials arrive within ~200-400ms of speech; `finish` has
/// the final text ~0-300ms after end-of-utterance — no WAV round trip.
///
/// Lifecycle: `start` at speech onset, `feed` every frame while in
/// speech, `poll_partial`/`finish` from the same (event-loop) thread.
/// Drop calls `finish_handle(0)` + `free` — safe at any point.
pub struct StreamSession {
    handle: *mut std::os::raw::c_void,
    /// Last text handed out, so `poll_partial` only reports changes.
    last_seen: String,
}

// The Swift session is internally locked; the raw handle is only ever
// dereferenced from the daemon's single event-loop thread.
unsafe impl Send for StreamSession {}

impl StreamSession {
    /// Start a session, or `None` when recognition is unavailable,
    /// unauthorized, or has no recognizer for the locale — callers
    /// fall back to the file-based path.
    pub fn start(lang: Option<&str>, phrases: &[String]) -> Option<Self> {
        let lang_cstr = lang
            .filter(|l| !l.is_empty())
            .and_then(|l| CString::new(l).ok());
        let phrases_cstr = CString::new(phrases.join("\n")).ok();
        let lang_ptr = lang_cstr.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        let phrases_ptr = phrases_cstr
            .as_ref()
            .map_or(std::ptr::null(), |c| c.as_ptr());
        let handle = unsafe { apple_stt_stream_start(lang_ptr, phrases_ptr) };
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
                last_seen: String::new(),
            })
        }
    }

    pub fn feed(&mut self, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        unsafe {
            apple_stt_stream_feed(self.handle, samples.as_ptr(), samples.len() as c_int);
        }
    }

    /// New partial text since the last call, or `None` when unchanged.
    pub fn poll_partial(&mut self) -> Option<String> {
        let seen_cstr = CString::new(self.last_seen.clone()).ok()?;
        let ptr = unsafe { apple_stt_stream_partial(self.handle, seen_cstr.as_ptr()) };
        if ptr.is_null() {
            return None;
        }
        let text = unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() };
        unsafe { free_apple_stt_string(ptr) };
        self.last_seen = text.clone();
        Some(text)
    }

    /// End the audio stream and take the final text (best partial on
    /// timeout). `None` when recognition produced nothing.
    pub fn finish(mut self, timeout_ms: u32) -> Option<String> {
        self.finish_inner(timeout_ms)
    }

    fn finish_inner(&mut self, timeout_ms: u32) -> Option<String> {
        let ptr = unsafe { apple_stt_stream_finish(self.handle, timeout_ms as c_int) };
        if ptr.is_null() {
            return None;
        }
        let text = unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() };
        unsafe { free_apple_stt_string(ptr) };
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

impl Drop for StreamSession {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // Give the recognizer a moment to settle when the caller
            // never asked for a final result (drop after an error path).
            unsafe { apple_stt_stream_free(self.handle) };
            self.handle = std::ptr::null_mut();
        }
    }
}
