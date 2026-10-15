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
    /// Mean per-word acoustic confidence, `-1` when not reported.
    confidence: f32,
}

unsafe extern "C" {
    fn is_apple_stt_available() -> c_int;
    fn apple_stt_default_capable() -> c_int;
    fn apple_stt_prepare(lang: *const c_char, timeout_secs: c_int) -> c_int;
    fn transcribe_wav_with_apple_stt(
        wav_path: *const c_char,
        lang: *const c_char,
        phrases: *const c_char,
    ) -> *mut AppleSttResponse;
    fn free_apple_stt_result(response: *mut AppleSttResponse);
    fn apple_stt_stream_start(lang: *const c_char, phrases: *const c_char) -> *mut c_void;
    fn apple_stt_stream_set_callback(
        handle: *mut c_void,
        cb: Option<unsafe extern "C" fn(*mut c_void, *const c_char)>,
        ctx: *mut c_void,
    );
    fn apple_stt_stream_feed(handle: *mut c_void, samples: *const i16, count: c_int);
    fn apple_stt_stream_partial(handle: *mut c_void, seen: *const c_char) -> *mut c_char;
    fn apple_stt_stream_finish(handle: *mut c_void, timeout_ms: c_int) -> *mut c_char;
    fn apple_stt_stream_outcome(handle: *mut c_void) -> c_int;
    fn apple_stt_stream_free(handle: *mut c_void);
    fn free_apple_stt_string(ptr: *mut c_char);
}

/// True when on-device Apple speech recognition is available and
/// authorized on this device and build.
pub fn check_availability() -> bool {
    unsafe { is_apple_stt_available() == 1 }
}

/// True when Apple's on-device engine may be the default for a user who
/// never picked a backend: SpeechAnalyzer present (macOS 26+) and Speech
/// Recognition not refused.
pub fn default_capable() -> bool {
    unsafe { apple_stt_default_capable() == 1 }
}

/// Outcome of [`prepare`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareOutcome {
    AlreadyInstalled,
    Installed,
    UnsupportedLocale,
    DownloadFailed,
    TimedOut,
    NeedsMacos26,
    /// SpeechTranscriber is unavailable on this hardware; the on-device
    /// legacy dictation model serves (nothing to install).
    LegacyOnDevice,
}

/// Install the on-device speech model for `lang` (None = current locale)
/// ahead of the first utterance, blocking up to `timeout_secs`. Download
/// progress goes to the Apple STT log.
pub fn prepare(lang: Option<&str>, timeout_secs: i32) -> PrepareOutcome {
    let lang_cstr = lang
        .filter(|l| !l.is_empty())
        .and_then(|l| CString::new(l).ok());
    let ptr = lang_cstr.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    match unsafe { apple_stt_prepare(ptr, timeout_secs) } {
        0 => PrepareOutcome::AlreadyInstalled,
        1 => PrepareOutcome::Installed,
        -1 => PrepareOutcome::UnsupportedLocale,
        -3 => PrepareOutcome::TimedOut,
        -4 => PrepareOutcome::NeedsMacos26,
        -5 => PrepareOutcome::LegacyOnDevice,
        _ => PrepareOutcome::DownloadFailed,
    }
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
    transcribe_wav_scored(path, lang, phrases).map(|t| t.text)
}

/// A file transcription plus the engine's mean per-word acoustic
/// confidence (`None` when the engine reported none).
#[derive(Debug, Clone, Default)]
pub struct ScoredTranscript {
    pub text: String,
    pub confidence: Option<f32>,
}

/// [`transcribe_wav`] that also returns the engine's acoustic confidence.
pub fn transcribe_wav_scored(
    path: &Path,
    lang: Option<&str>,
    phrases: &[String],
) -> Result<ScoredTranscript, String> {
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
            let text = if response.text.is_null() {
                String::new()
            } else {
                CStr::from_ptr(response.text).to_string_lossy().into_owned()
            };
            let confidence = (response.confidence >= 0.0 && response.confidence.is_finite())
                .then_some(response.confidence);
            Ok(ScoredTranscript { text, confidence })
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
    /// Boxed push target handed to Swift as the callback context. Freed in
    /// `Drop` only AFTER `apple_stt_stream_free` returned (Swift guarantees
    /// no callback runs after that point).
    sink: *mut std::sync::Arc<crate::always::live_preview::LivePreview>,
}

unsafe extern "C" fn partial_trampoline(ctx: *mut c_void, text: *const c_char) {
    if ctx.is_null() || text.is_null() {
        return;
    }
    let preview =
        unsafe { &*(ctx as *const std::sync::Arc<crate::always::live_preview::LivePreview>) };
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy();
    preview.on_apple(&text);
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
                sink: std::ptr::null_mut(),
            })
        }
    }

    /// Push every new partial into `preview` from the recognizer's own
    /// callback (no per-frame polling). Call at most once, right after
    /// `start`.
    pub fn set_partial_sink(
        &mut self,
        preview: std::sync::Arc<crate::always::live_preview::LivePreview>,
    ) {
        if !self.sink.is_null() {
            return;
        }
        self.sink = Box::into_raw(Box::new(preview));
        unsafe {
            apple_stt_stream_set_callback(
                self.handle,
                Some(partial_trampoline),
                self.sink as *mut c_void,
            );
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
    #[allow(dead_code)]
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

    /// [`finish`](Self::finish) plus HOW the session ended — the
    /// foreign-speech gate needs to tell "recognized nothing" (a clean
    /// no-speech end: no word of the locale's language was heard) apart
    /// from a failure or a timeout, which say nothing about the audio.
    pub fn finish_with_outcome(
        mut self,
        timeout_ms: u32,
    ) -> (Option<String>, crate::stt::LiveStreamOutcome) {
        let text = self.finish_inner(timeout_ms);
        let outcome = match unsafe { apple_stt_stream_outcome(self.handle) } {
            1 => crate::stt::LiveStreamOutcome::Heard,
            2 => crate::stt::LiveStreamOutcome::NoWords,
            3 => crate::stt::LiveStreamOutcome::Failed,
            _ => crate::stt::LiveStreamOutcome::Unknown,
        };
        (text, outcome)
    }

    fn finish_inner(&mut self, timeout_ms: u32) -> Option<String> {
        let ptr = unsafe { apple_stt_stream_finish(self.handle, timeout_ms as c_int) };
        if ptr.is_null() {
            return None;
        }
        let text = unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() };
        unsafe { free_apple_stt_string(ptr) };
        if text.is_empty() { None } else { Some(text) }
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
        if !self.sink.is_null() {
            drop(unsafe { Box::from_raw(self.sink) });
            self.sink = std::ptr::null_mut();
        }
    }
}
