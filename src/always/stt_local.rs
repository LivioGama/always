//! Offline transcription via `transcribe-rs` 0.3.3 — the same crate
//! Handy ships. Gated on the `local-stt` Cargo feature.
//!
//! [`LocalTranscriber`] holds a single loaded engine and implements
//! [`crate::stt::Transcriber`], so the daemon can swap remote Groq for
//! a local model without touching call sites. The engine dispatch
//! (Whisper / Parakeet / Moonshine / MoonshineStreaming / SenseVoice /
//! GigaAM / Canary / Cohere) is decided at load time from the catalog
//! entry — see [`crate::managers::model_registry::EngineType`].
//!
//! Audio path: callers hand us WAV bytes (16 kHz mono i16, produced by
//! [`crate::always::audio::create_wav_bytes_i16_mono_16k`]). We decode
//! the bytes to f32 in memory and hand them to the engine. No temp
//! files — earlier versions of this file wrote the WAV to disk per
//! utterance, which added ~10 ms of latency for no win since
//! transcribe-rs accepts samples directly.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use transcribe_rs::{
    SpeechModel, TranscribeOptions,
    onnx::{
        Quantization,
        canary::CanaryModel,
        cohere::CohereModel,
        gigaam::GigaAMModel,
        moonshine::{MoonshineModel, MoonshineVariant, StreamingModel},
        parakeet::{ParakeetModel, ParakeetParams, TimestampGranularity},
        sense_voice::{SenseVoiceModel, SenseVoiceParams},
    },
    whisper_cpp::{WhisperEngine, WhisperInferenceParams},
};

use crate::managers::model_registry::EngineType;
use crate::stt::{SttError, Transcriber, TranscriptionResult};

/// One loaded local engine. The variants mirror Handy's
/// `managers/transcription.rs::LoadedEngine` byte-for-byte so we keep
/// the same per-engine quirks (Moonshine variant, GigaAM CTC head, etc.).
enum LoadedEngine {
    Whisper(WhisperEngine),
    Parakeet(ParakeetModel),
    Moonshine(MoonshineModel),
    MoonshineStreaming(StreamingModel),
    SenseVoice(SenseVoiceModel),
    GigaAM(GigaAMModel),
    Canary(CanaryModel),
    Cohere(CohereModel),
}

/// Local-model [`Transcriber`] implementation.
///
/// Constructed once per active-model selection (so a few hundred MB of
/// weights load exactly once). `&self` is enough on the trait but the
/// underlying engines need `&mut self` to transcribe, so we wrap in a
/// `Mutex` — only one utterance at a time is in flight in the daemon
/// today, so contention is zero in practice.
pub struct LocalTranscriber {
    engine: Mutex<LoadedEngine>,
    /// Optional ISO 639-1 language hint passed to multi-lingual engines.
    /// `None` means "let the engine auto-detect" (Whisper / Canary) or
    /// "engine is monolingual" (Parakeet V2 / Moonshine / GigaAM).
    language: Option<String>,
}

impl LocalTranscriber {
    /// Load a downloaded model from disk. `path` is the registry's
    /// `models_dir.join(model.filename)` — file for Whisper engines,
    /// directory for ONNX engines.
    pub fn load(
        engine_type: EngineType,
        path: &Path,
        language: Option<String>,
    ) -> Result<Self> {
        let engine = build_engine(engine_type, path)
            .with_context(|| format!("loading {engine_type:?} model from {}", path.display()))?;
        Ok(Self {
            engine: Mutex::new(engine),
            language,
        })
    }
}

fn build_engine(engine_type: EngineType, path: &Path) -> Result<LoadedEngine> {
    Ok(match engine_type {
        EngineType::Whisper => LoadedEngine::Whisper(
            WhisperEngine::load(path).map_err(|e| anyhow::anyhow!("whisper load failed: {e}"))?,
        ),
        EngineType::Parakeet => LoadedEngine::Parakeet(
            ParakeetModel::load(path, &Quantization::Int8)
                .map_err(|e| anyhow::anyhow!("parakeet load failed: {e}"))?,
        ),
        EngineType::Moonshine => LoadedEngine::Moonshine(
            MoonshineModel::load(path, MoonshineVariant::Base, &Quantization::default())
                .map_err(|e| anyhow::anyhow!("moonshine load failed: {e}"))?,
        ),
        EngineType::MoonshineStreaming => LoadedEngine::MoonshineStreaming(
            StreamingModel::load(path, 0, &Quantization::default())
                .map_err(|e| anyhow::anyhow!("moonshine-streaming load failed: {e}"))?,
        ),
        EngineType::SenseVoice => LoadedEngine::SenseVoice(
            SenseVoiceModel::load(path, &Quantization::Int8)
                .map_err(|e| anyhow::anyhow!("sense_voice load failed: {e}"))?,
        ),
        EngineType::GigaAM => LoadedEngine::GigaAM(
            GigaAMModel::load(path, &Quantization::Int8)
                .map_err(|e| anyhow::anyhow!("gigaam load failed: {e}"))?,
        ),
        EngineType::Canary => LoadedEngine::Canary(
            CanaryModel::load(path, &Quantization::Int8)
                .map_err(|e| anyhow::anyhow!("canary load failed: {e}"))?,
        ),
        EngineType::Cohere => LoadedEngine::Cohere(
            CohereModel::load(path, &Quantization::Int8)
                .map_err(|e| anyhow::anyhow!("cohere load failed: {e}"))?,
        ),
    })
}

impl Transcriber for LocalTranscriber {
    fn transcribe_from_bytes(&self, audio: Vec<u8>) -> Result<TranscriptionResult, SttError> {
        let samples = decode_wav_to_f32(&audio).map_err(SttError::Other)?;
        if samples.is_empty() {
            return Ok(TranscriptionResult::default());
        }

        let mut engine = self
            .engine
            .lock()
            .map_err(|_| SttError::Other(anyhow::anyhow!("local engine mutex poisoned")))?;

        let lang_hint = self.language.clone();
        let started = std::time::Instant::now();
        let text = run_engine(&mut engine, &samples, lang_hint).map_err(SttError::Other)?;
        let duration = started.elapsed().as_secs_f64();
        tracing::debug!(elapsed_ms = (duration * 1000.0) as u64, "local_stt_done");

        Ok(TranscriptionResult {
            text: text.trim().to_string(),
            duration,
            language: self.language.clone().unwrap_or_default(),
            segments: vec![],
        })
    }
}

fn run_engine(engine: &mut LoadedEngine, samples: &[f32], language: Option<String>) -> Result<String> {
    let result = match engine {
        LoadedEngine::Whisper(w) => {
            let params = WhisperInferenceParams {
                language: language.clone(),
                ..Default::default()
            };
            w.transcribe_with(samples, &params)
                .map_err(|e| anyhow::anyhow!("whisper transcribe failed: {e}"))?
        }
        LoadedEngine::Parakeet(p) => {
            let params = ParakeetParams {
                timestamp_granularity: Some(TimestampGranularity::Segment),
                ..Default::default()
            };
            p.transcribe_with(samples, &params)
                .map_err(|e| anyhow::anyhow!("parakeet transcribe failed: {e}"))?
        }
        LoadedEngine::Moonshine(m) => m
            .transcribe(samples, &TranscribeOptions::default())
            .map_err(|e| anyhow::anyhow!("moonshine transcribe failed: {e}"))?,
        LoadedEngine::MoonshineStreaming(m) => m
            .transcribe(samples, &TranscribeOptions::default())
            .map_err(|e| anyhow::anyhow!("moonshine-streaming transcribe failed: {e}"))?,
        LoadedEngine::SenseVoice(sv) => {
            let params = SenseVoiceParams {
                language: language.clone(),
                use_itn: Some(true),
            };
            sv.transcribe_with(samples, &params)
                .map_err(|e| anyhow::anyhow!("sense_voice transcribe failed: {e}"))?
        }
        LoadedEngine::GigaAM(g) => g
            .transcribe(samples, &TranscribeOptions::default())
            .map_err(|e| anyhow::anyhow!("gigaam transcribe failed: {e}"))?,
        LoadedEngine::Canary(c) => {
            let options = TranscribeOptions {
                language: language.clone(),
                ..Default::default()
            };
            c.transcribe(samples, &options)
                .map_err(|e| anyhow::anyhow!("canary transcribe failed: {e}"))?
        }
        LoadedEngine::Cohere(c) => {
            let options = TranscribeOptions {
                language: language.clone(),
                ..Default::default()
            };
            c.transcribe(samples, &options)
                .map_err(|e| anyhow::anyhow!("cohere transcribe failed: {e}"))?
        }
    };
    Ok(result.text)
}

/// Decode the daemon's canonical WAV format (16 kHz mono PCM i16) into
/// the f32 samples transcribe-rs expects. We trust the format because
/// [`crate::always::audio::create_wav_bytes_i16_mono_16k`] is the only
/// producer — but we still validate the RIFF header and 16-bit sample
/// width so a bad input fails loudly rather than silently transcribing
/// garbage.
fn decode_wav_to_f32(wav: &[u8]) -> Result<Vec<f32>> {
    if wav.len() < 44 || &wav[..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        anyhow::bail!("not a WAV file");
    }
    // Walk chunks instead of assuming a fixed 44-byte header so we
    // tolerate any extra "LIST"/"JUNK"/"INFO" chunks hound might emit.
    let mut i = 12;
    let mut bits_per_sample: u16 = 0;
    let mut data: Option<&[u8]> = None;
    while i + 8 <= wav.len() {
        let chunk_id = &wav[i..i + 4];
        let chunk_size =
            u32::from_le_bytes([wav[i + 4], wav[i + 5], wav[i + 6], wav[i + 7]]) as usize;
        let body_start = i + 8;
        let body_end = body_start + chunk_size;
        if body_end > wav.len() {
            anyhow::bail!("WAV chunk extends past end of buffer");
        }
        if chunk_id == b"fmt " {
            if chunk_size < 16 {
                anyhow::bail!("WAV fmt chunk too short");
            }
            bits_per_sample = u16::from_le_bytes([wav[body_start + 14], wav[body_start + 15]]);
        } else if chunk_id == b"data" {
            data = Some(&wav[body_start..body_end]);
            break;
        }
        // Chunks are word-aligned.
        i = body_end + (chunk_size & 1);
    }
    let data = data.ok_or_else(|| anyhow::anyhow!("WAV missing data chunk"))?;
    if bits_per_sample != 16 {
        anyhow::bail!("expected 16-bit WAV, got {bits_per_sample}");
    }

    let mut samples = Vec::with_capacity(data.len() / 2);
    for pair in data.chunks_exact(2) {
        let s = i16::from_le_bytes([pair[0], pair[1]]);
        samples.push(s as f32 / 32768.0);
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::always::audio::create_wav_bytes_i16_mono_16k;

    #[test]
    fn decode_round_trips_silence() {
        let pcm: Vec<i16> = vec![0; 16_000];
        let wav = create_wav_bytes_i16_mono_16k(&pcm).expect("encode");
        let samples = decode_wav_to_f32(&wav).expect("decode");
        assert_eq!(samples.len(), 16_000);
        assert!(samples.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn decode_round_trips_known_samples() {
        let pcm: Vec<i16> = vec![16384, -16384, 8192, -8192];
        let wav = create_wav_bytes_i16_mono_16k(&pcm).expect("encode");
        let samples = decode_wav_to_f32(&wav).expect("decode");
        assert_eq!(samples.len(), 4);
        assert!((samples[0] - 0.5).abs() < 1e-4);
        assert!((samples[1] + 0.5).abs() < 1e-4);
    }

    #[test]
    fn rejects_non_wav() {
        let bogus = vec![0u8; 64];
        assert!(decode_wav_to_f32(&bogus).is_err());
    }
}
