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

    fn apple_stt_stream_supported() -> c_int;
    fn apple_stt_stream_start(lang: *const c_char, phrases: *const c_char) -> *mut c_void;
    fn apple_stt_stream_push(
        session: *mut c_void,
        samples: *const f32,
        count: usize,
    ) -> *mut c_char;
    fn apple_stt_stream_finish(session: *mut c_void) -> *mut AppleSttResponse;
    fn apple_stt_stream_cancel(session: *mut c_void);
    fn free_apple_stt_string(s: *mut c_char);
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
pub fn transcribe_wav(path: &Path, lang: Option<&str>, phrases: &[String]) -> Result<String, String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| "Apple STT path is not valid UTF-8".to_string())?;
    let path_cstr = CString::new(path_str).map_err(|e| e.to_string())?;
    let lang_cstr = lang
        .filter(|l| !l.is_empty())
        .and_then(|l| CString::new(l).ok());
    let phrases_cstr = CString::new(phrases.join("\n")).ok();

    let lang_ptr = lang_cstr
        .as_ref()
        .map_or(std::ptr::null(), |c| c.as_ptr());
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

/// True when the incremental streaming path (SpeechAnalyzer push API) can
/// run on this device — macOS 26+ with speech recognition available.
pub fn stream_supported() -> bool {
    unsafe { apple_stt_stream_supported() == 1 }
}

/// A live incremental Apple STT session. Owns the Swift-side session pointer;
/// `finish`/`cancel` consume it.
pub struct AppleStreamSession {
    ptr: *mut c_void,
}

// The Swift session is self-synchronized (NSLock on the segment table,
// continuations are Sendable); the pointer is only ever used by the one
// live-stream worker thread that owns this struct.
unsafe impl Send for AppleStreamSession {}

impl AppleStreamSession {
    /// Start a session, or `None` when streaming cannot begin now (not
    /// authorized, unsupported locale, or the speech model is not installed
    /// yet — the one-shot path covers those and installs the model).
    ///
    /// Blocks up to ~4 s on async setup; normally far less.
    pub fn start(lang: Option<&str>, phrases: &[String]) -> Option<Self> {
        let lang_cstr = lang
            .filter(|l| !l.is_empty())
            .and_then(|l| CString::new(l).ok());
        let phrases_cstr = CString::new(phrases.join("\n")).ok();
        let ptr = unsafe {
            apple_stt_stream_start(
                lang_cstr.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                phrases_cstr
                    .as_ref()
                    .map_or(std::ptr::null(), |c| c.as_ptr()),
            )
        };
        (!ptr.is_null()).then_some(Self { ptr })
    }

    /// Push 16 kHz mono float32 samples; returns the cumulative transcript
    /// decoded so far (possibly empty).
    pub fn push(&mut self, samples: &[f32]) -> Result<String, String> {
        if samples.is_empty() {
            return Ok(String::new());
        }
        let text_ptr =
            unsafe { apple_stt_stream_push(self.ptr, samples.as_ptr(), samples.len()) };
        if text_ptr.is_null() {
            return Err("Apple STT stream push failed".to_string());
        }
        let text = unsafe { CStr::from_ptr(text_ptr) }
            .to_string_lossy()
            .into_owned();
        unsafe { free_apple_stt_string(text_ptr) };
        Ok(text)
    }

    /// Close input, flush the decoder, return the final transcript.
    /// Consumes the session.
    pub fn finish(mut self) -> Result<String, String> {
        let ptr = std::mem::replace(&mut self.ptr, std::ptr::null_mut());
        let response_ptr = unsafe { apple_stt_stream_finish(ptr) };
        if response_ptr.is_null() {
            return Err("Apple STT stream finish returned null".to_string());
        }
        let result = unsafe {
            let response = &*response_ptr;
            if response.success == 1 {
                Ok(if response.text.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(response.text).to_string_lossy().into_owned()
                })
            } else {
                Err(if !response.error_message.is_null() {
                    CStr::from_ptr(response.error_message)
                        .to_string_lossy()
                        .into_owned()
                } else {
                    "Unknown Apple STT stream error".to_string()
                })
            }
        };
        unsafe { free_apple_stt_result(response_ptr) };
        result
    }
}

impl Drop for AppleStreamSession {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { apple_stt_stream_cancel(self.ptr) };
        }
    }
}
