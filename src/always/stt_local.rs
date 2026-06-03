//! Optional offline transcription via `transcribe-rs`. Mirrors the
//! Handy approach (whisper.cpp wrapper). Gated behind the `local-stt`
//! Cargo feature so default builds stay free of the heavy native
//! dependency.
//!
//! Caller contract is intentionally minimal:
//!
//! ```ignore
//! let engine = LocalEngine::load_default()?;
//! let text = engine.transcribe_wav_bytes(&wav)?;
//! ```
//!
//! Model is loaded from `ALWAYS_LOCAL_MODEL_PATH` or the canonical
//! cache directory. Same `TranscriptionResult` shape as the Groq path,
//! so callers can swap providers without changes.

use anyhow::{Context, Result};
use std::path::PathBuf;

use transcribe_rs::{
    SpeechModel, TranscribeOptions,
    whisper_cpp::{WhisperEngine, WhisperInferenceParams},
};

use crate::stt::TranscriptionResult;

pub struct LocalEngine {
    engine: WhisperEngine,
}

impl LocalEngine {
    /// Load the default Whisper model from the canonical cache path.
    /// Override with `ALWAYS_LOCAL_MODEL_PATH=/path/to/ggml-small.bin`.
    pub fn load_default() -> Result<Self> {
        let path = model_path()?;
        let mut engine = WhisperEngine::new();
        engine
            .load_model(&path)
            .with_context(|| format!("failed to load whisper model {}", path.display()))?;
        Ok(Self { engine })
    }

    /// Transcribe a WAV byte buffer (16k mono i16). Returns the daemon's
    /// canonical `TranscriptionResult` so callers can branch on
    /// provider without rebuilding the shape.
    pub fn transcribe_wav_bytes(&mut self, wav: &[u8]) -> Result<TranscriptionResult> {
        let tmp = std::env::temp_dir().join(format!(
            "always-local-stt-{}.wav",
            std::process::id()
        ));
        std::fs::write(&tmp, wav).context("write temp wav for local stt")?;

        let opts: TranscribeOptions<WhisperInferenceParams> = TranscribeOptions::default();
        let result = self
            .engine
            .transcribe_file(&tmp, Some(opts))
            .with_context(|| format!("local whisper transcribe failed: {}", tmp.display()))?;
        let _ = std::fs::remove_file(&tmp);

        Ok(TranscriptionResult {
            text: result.text.trim().to_string(),
            duration: result.duration.unwrap_or(0.0) as f64,
            language: "en".to_string(),
            segments: vec![],
        })
    }
}

fn model_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("ALWAYS_LOCAL_MODEL_PATH") {
        return Ok(PathBuf::from(p));
    }
    let dir = dirs::cache_dir()
        .ok_or_else(|| anyhow::anyhow!("no cache dir"))?
        .join("always")
        .join("models");
    Ok(dir.join("ggml-small.bin"))
}
