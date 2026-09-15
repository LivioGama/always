//! Apple on-device SFSpeechRecognizer [`Transcriber`] implementation.
//!
//! The audio path: callers pass WAV bytes (16 kHz mono i16, produced by
//! the daemon's audio pipeline). We write them to a temporary file and
//! ask the Swift SFSpeechRecognizer bridge to transcribe it. No streaming
//! support — the Speech framework returns a single final result for a
//! complete utterance.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Mutex;

use futures::stream::Stream;

use crate::stt::{
    StreamingTranscriptionResult, SttError, Transcriber, TranscriptionResult,
};

/// Serializes access to SFSpeechRecognizer. Concurrent recognition tasks
/// have caused the framework to return empty results for one of the callers.
static APPLE_STT_MUTEX: Mutex<()> = Mutex::new(());

pub struct AppleTranscriber {
    language: Option<String>,
}

impl AppleTranscriber {
    pub fn new(language: Option<String>) -> anyhow::Result<Self> {
        if !crate::always::apple_stt::check_availability() {
            anyhow::bail!("Apple STT is not available or not authorized on this device");
        }
        Ok(Self { language })
    }
}

impl Transcriber for AppleTranscriber {
    fn supports_streaming(&self) -> bool {
        false
    }

    fn transcribe_from_bytes(&self, mut audio: Vec<u8>) -> Result<TranscriptionResult, SttError> {
        // Boost quiet utterances before sending them to Apple STT. On-device
        // recognition is less tolerant of low-amplitude speech than Whisper.
        normalize_wav_samples(&mut audio);

        let temp_path = temp_wav_path().map_err(|e| {
            SttError::Other(anyhow::anyhow!("failed to create Apple STT temp path: {e}"))
        })?;

        std::fs::write(&temp_path, &audio).map_err(|e| {
            SttError::Other(anyhow::anyhow!(
                "failed to write Apple STT temp file: {e}"
            ))
        })?;

        let phrases = context_phrases();
        let result = {
            let _guard = APPLE_STT_MUTEX.lock().unwrap();
            crate::always::apple_stt::transcribe_wav(
                &temp_path,
                self.language.as_deref(),
                &phrases,
            )
            .map_err(|e| SttError::Other(anyhow::anyhow!("Apple STT failed: {e}")))
        };

        let _ = std::fs::remove_file(&temp_path);

        let text = result?;
        if text.is_empty() {
            tracing::warn!(language = ?self.language, "Apple STT returned empty text");
        } else {
            tracing::info!(language = ?self.language, "Apple STT result: {}", text);
        }
        Ok(TranscriptionResult {
            text: text.trim().to_string(),
            duration: 0.0,
            language: self.language.clone().unwrap_or_default(),
            segments: vec![],
        })
    }

    fn transcribe_streaming(
        &self,
        audio: Vec<u8>,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamingTranscriptionResult, SttError>> + Send>> {
        let result = match self.transcribe_from_bytes(audio) {
            Ok(r) => Ok(StreamingTranscriptionResult {
                text: r.text,
                is_final: true,
                is_interim: false,
            }),
            Err(e) => Err(e),
        };
        Box::pin(futures::stream::once(async move { result }))
    }
}

/// Domain phrases fed to `AnalysisContext.contextualStrings` for vocabulary
/// biasing. Default tech terms are merged with every canonical glossary term;
/// the engine only boosts, never rewrites, so the full glossary is safe to
/// include. Capped at 100 phrases (Apple's documented budget).
fn context_phrases() -> Vec<String> {
    const DEFAULTS: &[&str] = &[
        "Claude Code",
        "Claude",
        "Codex",
        "Devin",
        "git worktrees",
        "git worktree",
        "worktree",
        "Warp",
        "GitHub",
        "macOS",
    ];
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for phrase in DEFAULTS
        .iter()
        .map(|s| s.to_string())
        .chain(crate::glossary::all_terms())
    {
        if out.len() >= 100 {
            break;
        }
        let trimmed = phrase.trim();
        if !trimmed.is_empty() && seen.insert(trimmed.to_lowercase()) {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn temp_wav_path() -> anyhow::Result<PathBuf> {
    let id = uuid::Uuid::new_v4();
    let mut path = std::env::temp_dir();
    path.push(format!("always_apple_stt_{}.wav", id));
    Ok(path)
}

/// Find the offset of the PCM sample data in a WAV file by scanning for the
/// `data` chunk. Supports the canonical 44-byte header as well as larger
/// `fmt ` chunks with extra bytes.
fn wav_sample_offset(data: &[u8]) -> Option<usize> {
    let mut i = 12; // skip RIFF header
    while i + 8 <= data.len() {
        let chunk_id = std::str::from_utf8(&data[i..i + 4]).ok()?;
        let chunk_size = u32::from_le_bytes([data[i + 4], data[i + 5], data[i + 6], data[i + 7]]) as usize;
        if chunk_id == "data" {
            return Some(i + 8);
        }
        i += 8 + chunk_size;
    }
    None
}

/// Normalize mono 16-bit PCM samples to ~90% of full scale. Apple STT is less
/// tolerant of quiet speech than Whisper, and the daemon's audio pipeline does
/// not apply automatic gain control.
fn normalize_wav_samples(data: &mut [u8]) {
    let Some(samples_start) = wav_sample_offset(data) else { return };
    if samples_start >= data.len() || (data.len() - samples_start) % 2 != 0 {
        return;
    }
    let sample_bytes = &data[samples_start..];
    let mut max_abs = 0i32;
    for chunk in sample_bytes.chunks_exact(2) {
        let sample = i16::from_le_bytes([chunk[0], chunk[1]]) as i32;
        let abs = sample.unsigned_abs() as i32;
        if abs > max_abs {
            max_abs = abs;
        }
    }
    if max_abs == 0 {
        return;
    }

    // Target 90% of i16 max to leave headroom and avoid hard clipping.
    const TARGET: i32 = (i16::MAX as i32 * 9) / 10;
    let gain = TARGET as f32 / max_abs as f32;
    // Cap gain at 4x — if the audio needs more, it's probably noise.
    let gain = gain.min(4.0);

    let sample_bytes = &mut data[samples_start..];
    for chunk in sample_bytes.chunks_exact_mut(2) {
        let sample = i16::from_le_bytes([chunk[0], chunk[1]]) as i32;
        let scaled = (sample as f32 * gain).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        let bytes = scaled.to_le_bytes();
        chunk[0] = bytes[0];
        chunk[1] = bytes[1];
    }
}
