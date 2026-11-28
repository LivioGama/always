//! Whistle (Cactus Compute) — a 17 MB multilingual ASR model run by the
//! Cactus C engine, linked statically by `build.rs` (`cfg(cactus)`).
//!
//! The C API keeps ONE loaded model in process-global state, so every call
//! goes through [`CACTUS`]. `cactus_load` copies the weights, so the file
//! bytes are dropped after loading.

use std::ffi::{CStr, CString, c_char, c_int, c_uchar, c_ulonglong};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};

unsafe extern "C" {
    fn cactus_load(cact: *const c_uchar, n: c_ulonglong) -> c_int;
    fn cactus_last_error() -> *const c_char;
    fn cactus_transcribe(
        pcm: *const f32,
        samples: c_int,
        language: *const c_char,
        keywords: *const c_char,
        word_timestamps: c_int,
        out: *mut c_char,
        out_capacity: c_int,
    ) -> c_int;
}

/// Languages the Whistle vocabulary carries; anything else → auto-detect.
pub const WHISTLE_LANGUAGES: [&str; 7] = ["en", "de", "fr", "es", "it", "nl", "pl"];

/// Output JSON budget, as the upstream Python binding sizes it.
const OUTPUT_BYTES_PER_SECOND: usize = 4096;
const MIN_OUTPUT_BYTES: usize = 1 << 16;

/// Path of the weights currently loaded into the engine.
static CACTUS: Mutex<Option<PathBuf>> = Mutex::new(None);

fn last_error() -> String {
    // SAFETY: returns a NUL-terminated static/thread-local string or null.
    let ptr = unsafe { cactus_last_error() };
    if ptr.is_null() {
        return "unknown cactus error".into();
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

fn ensure_loaded(slot: &mut Option<PathBuf>, path: &Path) -> Result<()> {
    if slot.as_deref() == Some(path) {
        return Ok(());
    }
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    // SAFETY: pointer/len describe a live buffer for the duration of the call.
    let rc = unsafe { cactus_load(bytes.as_ptr(), bytes.len() as c_ulonglong) };
    if rc < 0 {
        *slot = None;
        bail!("cactus_load failed: {}", last_error());
    }
    *slot = Some(path.to_path_buf());
    Ok(())
}

/// A loaded Whistle model. Cheap handle; the engine itself is global.
pub struct Whistle {
    path: PathBuf,
}

impl Whistle {
    pub fn load(path: &Path) -> Result<Self> {
        let mut slot = CACTUS
            .lock()
            .map_err(|_| anyhow::anyhow!("cactus mutex poisoned"))?;
        ensure_loaded(&mut slot, path)?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// Transcribe 16 kHz mono f32 samples. `language` outside
    /// [`WHISTLE_LANGUAGES`] (or `None`) lets the model detect it.
    pub fn transcribe(&self, samples: &[f32], language: Option<&str>) -> Result<String> {
        let mut slot = CACTUS
            .lock()
            .map_err(|_| anyhow::anyhow!("cactus mutex poisoned"))?;
        ensure_loaded(&mut slot, &self.path)?;

        let lang = language
            .filter(|l| WHISTLE_LANGUAGES.contains(l))
            .map(|l| CString::new(l).expect("ISO code has no NUL"));
        let lang_ptr = lang.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        let count = c_int::try_from(samples.len()).context("audio too long for cactus")?;
        let capacity = (samples.len() / 16_000 * OUTPUT_BYTES_PER_SECOND).max(MIN_OUTPUT_BYTES);
        let mut out = vec![0u8; capacity];
        // SAFETY: all pointers are valid for the call; `out` has `capacity` bytes.
        let rc = unsafe {
            cactus_transcribe(
                samples.as_ptr(),
                count,
                lang_ptr,
                std::ptr::null(),
                0,
                out.as_mut_ptr().cast(),
                capacity as c_int,
            )
        };
        if rc < 0 {
            bail!("cactus_transcribe failed: {}", last_error());
        }
        let end = out.iter().position(|&b| b == 0).unwrap_or(out.len());
        parse_text(&out[..end])
    }
}

/// Pull `text` out of `{"text":…,"language":…,"ttft_ms":…,"decode_tps":…}`.
fn parse_text(json: &[u8]) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_slice(json).context("cactus returned invalid JSON")?;
    Ok(value
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_engine_output() {
        let raw = br#"{"text":"Turn off the kitchen lights.","language":"en","ttft_ms":10.7,"decode_tps":1007.4}"#;
        assert_eq!(parse_text(raw).unwrap(), "Turn off the kitchen lights.");
    }

    #[test]
    fn silence_output_is_empty_text() {
        assert_eq!(parse_text(br#"{"text":"","language":""}"#).unwrap(), "");
    }

    /// Runs only when the weights are on disk (set `WHISTLE_CACT`).
    #[test]
    fn transcribes_silence_without_error() {
        let Some(path) = std::env::var_os("WHISTLE_CACT").map(PathBuf::from) else {
            return;
        };
        let w = Whistle::load(&path).expect("load");
        let text = w.transcribe(&vec![0.0; 16_000], None).expect("transcribe");
        assert!(text.is_empty(), "silence produced {text:?}");
    }
}
