//! Silero VAD wrapper. Replaces the previous `voice_activity_detector`
//! crate (pinned to `ort = =2.0.0-rc.10`) with `vad-rs` (pinned to
//! `ort = =2.0.0-rc.12`), so we can coexist with `transcribe-rs` 0.3.3+
//! which also requires rc.12.
//!
//! The Silero ONNX model is embedded at compile time via
//! `include_bytes!`, then materialised to a stable per-user cache path
//! on first use because `vad_rs::Vad::new` only accepts a filesystem
//! path. The materialised file is ~1.8 MB and is read-only after
//! creation.

use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use parking_lot::Mutex;

const SAMPLE_RATE: usize = 16_000;
/// Bytes of the embedded Silero v4 ONNX model. Copied verbatim from
/// Handy's `src-tauri/resources/models/silero_vad_v4.onnx`.
const SILERO_ONNX: &[u8] = include_bytes!("../../resources/models/silero_vad_v4.onnx");

/// Cached path of the materialised Silero model. Resolved once per
/// process; the underlying file lives under `~/Library/Caches/Always`
/// on macOS so it persists across runs without bloating the app
/// bundle's notarised payload.
fn silero_model_path() -> Result<PathBuf> {
    static MODEL_PATH: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    let entry = MODEL_PATH.get_or_init(|| {
        let dir = dirs::cache_dir()
            .ok_or_else(|| "no cache dir".to_string())?
            .join("always");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("silero_vad_v4.onnx");
        // Rewrite only if the file is missing or the wrong size. We
        // never overwrite a freshly-written valid file — that would
        // race with concurrent processes spawning the same daemon.
        let needs_write = match std::fs::metadata(&path) {
            Ok(meta) => meta.len() as usize != SILERO_ONNX.len(),
            Err(_) => true,
        };
        if needs_write {
            std::fs::write(&path, SILERO_ONNX).map_err(|e| e.to_string())?;
        }
        Ok(path)
    });
    entry
        .clone()
        .map_err(|e| anyhow::anyhow!("silero model setup failed: {e}"))
}

/// Thin compatibility wrapper around `vad_rs::Vad`.
///
/// API kept narrow on purpose: callers feed 480-sample i16 frames
/// (30 ms @ 16 kHz) — the only frame size the daemon's audio pipeline
/// produces — and receive Silero's raw speech probability. Threshold
/// comparison happens at the call site (the daemon applies its own
/// hysteresis on top), so we don't enforce a threshold here.
pub struct SileroVad {
    inner: Mutex<vad_rs::Vad>,
}

impl SileroVad {
    /// Load the embedded Silero model. Cheap to call once; not
    /// designed for repeated construction.
    pub fn new() -> Result<Self> {
        let path = silero_model_path()?;
        let vad = vad_rs::Vad::new(&path, SAMPLE_RATE)
            .map_err(|e| anyhow::anyhow!("vad_rs::Vad::new failed: {e}"))
            .with_context(|| format!("loading silero model from {}", path.display()))?;
        Ok(Self {
            inner: Mutex::new(vad),
        })
    }

    /// Run one inference. `samples` must be exactly 480 i16 samples
    /// (one 30 ms frame at 16 kHz). Returns Silero's speech
    /// probability in `[0.0, 1.0]`. Errors propagate up so the caller
    /// can decide whether to bail or treat as no-speech.
    pub fn predict(&self, samples: &[i16]) -> Result<f32> {
        if samples.len() != 480 {
            anyhow::bail!(
                "SileroVad::predict expects 480 samples (30ms @ 16kHz), got {}",
                samples.len()
            );
        }
        // vad-rs takes f32 in [-1.0, 1.0]; convert from i16 inline to
        // avoid a heap alloc per frame (called once per 30 ms = ~33×/s).
        let mut frame = [0f32; 480];
        for (i, s) in samples.iter().enumerate() {
            frame[i] = (*s as f32) / 32768.0;
        }
        let result = self
            .inner
            .lock()
            .compute(&frame)
            .map_err(|e| anyhow::anyhow!("Silero VAD compute error: {e}"))?;
        Ok(result.prob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_model_is_nonempty() {
        assert!(SILERO_ONNX.len() > 1_000_000, "model should be ~1.8MB");
    }

    #[test]
    fn silence_returns_low_probability() {
        let vad = SileroVad::new().expect("model loads");
        let silence = [0i16; 480];
        let prob = vad.predict(&silence).expect("inference succeeds");
        assert!(prob < 0.5, "pure silence should be well below threshold");
    }

    #[test]
    fn wrong_frame_size_errors() {
        let vad = SileroVad::new().expect("model loads");
        let bad = vec![0i16; 256];
        assert!(vad.predict(&bad).is_err());
    }
}
