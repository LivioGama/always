//! Safe Rust wrapper around the Apple on-device STT (SFSpeechRecognizer)
//! bridge. The Swift side (`swift/apple_stt.swift`) exposes C functions;
//! this module wraps them in safe Rust types.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
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
