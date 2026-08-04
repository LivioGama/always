//! Offline transcription via `transcribe-rs` 0.3.3 — the same crate
//! Handy ships. Gated on the `local-stt` Cargo feature.
//!
//! [`LocalTranscriber`] holds a single loaded engine and implements
//! [`crate::stt::Transcriber`], so the daemon can swap remote Groq for
//! a local model without touching call sites. The engine dispatch
//! (Whisper / Parakeet / Moonshine / MoonshineStreaming / SenseVoice /
//! GigaAM / Canary / Cohere / Nemotron) is decided at load time from the catalog
//! entry — see [`crate::managers::model_registry::EngineType`].
//!
//! Audio path: callers hand us WAV bytes (16 kHz mono i16, produced by
//! [`crate::always::audio::create_wav_bytes_i16_mono_16k`]). We decode
//! the bytes to f32 in memory and hand them to the engine. No temp
//! files — earlier versions of this file wrote the WAV to disk per
//! utterance, which added ~10 ms of latency for no win since
//! transcribe-rs accepts samples directly.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use std::pin::Pin;

/// Warmup / keep-alive audio: 0.5s of silence @16kHz. Running one throwaway
/// inference forces the ONNX graph + Apple-Neural-Engine execution provider to
/// compile/warm so the user's next REAL utterance doesn't pay a multi-second
/// cold-start (measured: 5087ms on the first utterance after a restart / long
/// idle vs ~300ms warm).
const WARMUP_SAMPLES: usize = 8_000;
/// Master switch for the background keep-alive thread. Disabled: re-warming
/// grew the ONNX arena and latency climbed over a session. The boot warmup is
/// kept; frequent dictation keeps the model hot without a background warmer.
const KEEPALIVE_ENABLED: bool = false;
/// Keep the model warm this long after the last real utterance, then let it go
/// cold (the user has walked away — no point burning the ANE). While active,
/// a background thread re-warms on `KEEPALIVE_EVERY`.
const KEEPALIVE_ACTIVE_WINDOW_SECS: u64 = 600;
/// How often the keep-alive thread re-warms while within the active window.
const KEEPALIVE_EVERY_SECS: u64 = 20;

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

#[cfg(feature = "local-stt")]
use parakeet_rs::Nemotron;

use crate::managers::model_registry::EngineType;
use crate::stt::{SttError, Transcriber, TranscriptionResult, StreamingTranscriptionResult};
use futures::stream::Stream;

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
    #[cfg(feature = "local-stt")]
    Nemotron(Nemotron),
}

/// Local-model [`Transcriber`] implementation.
///
/// Constructed once per active-model selection (so a few hundred MB of
/// weights load exactly once). `&self` is enough on the trait but the
/// underlying engines need `&mut self` to transcribe, so we wrap in a
/// `Mutex` — only one utterance at a time is in flight in the daemon
/// today, so contention is zero in practice.
pub struct LocalTranscriber {
    engine: Arc<Mutex<LoadedEngine>>,
    /// Optional ISO 639-1 language hint passed to multi-lingual engines.
    /// `None` means "auto" — let the engine auto-detect (Whisper /
    /// SenseVoice) or, for engines that bake the language into the decode
    /// prompt (Canary / Cohere), fall back to the engine's own default.
    language: Option<String>,
    /// Wall-clock of the last REAL transcription. The keep-alive thread uses
    /// it to decide whether the user is still actively dictating (re-warm) or
    /// has walked away (let the model go cold).
    last_used: Arc<Mutex<Instant>>,
    /// Whether this model supports streaming transcription.
    supports_streaming: bool,
}

impl LocalTranscriber {
    /// Load a downloaded model from disk. `path` is the registry's
    /// `models_dir.join(model.filename)` — file for Whisper engines,
    /// directory for ONNX engines.
    pub fn load(
        engine_type: EngineType,
        path: &Path,
        language: Option<String>,
        supports_streaming: bool,
    ) -> Result<Self> {
        let mut engine = build_engine(engine_type, path)
            .with_context(|| format!("loading {engine_type:?} model from {}", path.display()))?;

        // Warm the freshly-loaded model NOW so the first real utterance is fast.
        let started = Instant::now();
        match run_engine(&mut engine, &vec![0.0f32; WARMUP_SAMPLES], language.clone()) {
            Ok(_) => tracing::info!(
                warm_ms = started.elapsed().as_millis() as u64,
                "local_stt_warmup_done"
            ),
            Err(e) => tracing::warn!(error = %e, "local_stt_warmup_failed"),
        }

        let engine = Arc::new(Mutex::new(engine));
        let last_used = Arc::new(Mutex::new(Instant::now()));
        // Keep-alive DISABLED: repeatedly warming grew the ONNX arena per
        // inference, so latency climbed over a session (703ms → 4400ms). The
        // one-shot boot warmup above already compiles the graph, and frequent
        // real dictation keeps the model hot on its own; only a multi-minute
        // idle risks a cold-ish first utterance, which is far better than a
        // progressive slowdown during active use.
        if KEEPALIVE_ENABLED {
            spawn_keepalive(
                Arc::clone(&engine),
                Arc::clone(&last_used),
                language.clone(),
            );
        }

        Ok(Self {
            engine,
            language,
            last_used,
            supports_streaming,
        })
    }
}

/// Background thread that keeps the ANE/ONNX graph warm between utterances
/// while the user is actively dictating, so intermittent speech (pause to
/// think, glance away, come back) doesn't hit a cold-start on every resume.
/// Uses `try_lock` so it NEVER blocks a real transcription, and stops warming
/// once the user has been idle past the active window (they walked away).
fn spawn_keepalive(
    engine: Arc<Mutex<LoadedEngine>>,
    last_used: Arc<Mutex<Instant>>,
    language: Option<String>,
) {
    std::thread::Builder::new()
        .name("stt-keepalive".into())
        .spawn(move || {
            let warm = vec![0.0f32; WARMUP_SAMPLES];
            loop {
                std::thread::sleep(std::time::Duration::from_secs(KEEPALIVE_EVERY_SECS));
                let idle = last_used
                    .lock()
                    .map(|t| t.elapsed().as_secs())
                    .unwrap_or(u64::MAX);
                if idle > KEEPALIVE_ACTIVE_WINDOW_SECS {
                    continue; // user walked away — let it go cold, save the ANE
                }
                // try_lock: skip silently if a real transcription holds the
                // engine — the keep-alive must never add contention.
                if let Ok(mut guard) = engine.try_lock() {
                    let _ = run_engine(&mut guard, &warm, language.clone());
                }
            }
        })
        .ok();
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
        #[cfg(feature = "local-stt")]
        EngineType::Nemotron => LoadedEngine::Nemotron(
            Nemotron::from_pretrained(path, None)
                .map_err(|e| anyhow::anyhow!("nemotron load failed: {e}"))?,
        ),
        #[cfg(not(feature = "local-stt"))]
        EngineType::Nemotron => {
            return Err(anyhow::anyhow!("Nemotron requires the local-stt feature"))
        }
    })
}

/// Hard ceiling on a single offline utterance. The daemon is always-on
/// and a stuck-open VAD (continuous noise/feedback) could otherwise drive
/// an unbounded WAV → multi-GB f32/tensor allocation and OOM-kill the
/// process. 10 minutes is far longer than any real dictation; beyond it we
/// reject the input rather than try to transcribe it.
const MAX_LOCAL_STT_SECS: usize = 600;
/// 16 kHz mono i16 = 32 000 bytes/sec. The `+ 4096` absorbs RIFF header and
/// any extra LIST/JUNK/INFO chunks so a legitimately-capped clip isn't
/// rejected for header overhead.
const MAX_WAV_BYTES: usize = 16_000 * 2 * MAX_LOCAL_STT_SECS;

impl Transcriber for LocalTranscriber {
    fn transcribe_from_bytes(&self, audio: Vec<u8>) -> Result<TranscriptionResult, SttError> {
        if audio.len() > MAX_WAV_BYTES + 4096 {
            return Err(SttError::Other(anyhow::anyhow!(
                "utterance too long for local STT ({} bytes > {} cap)",
                audio.len(),
                MAX_WAV_BYTES
            )));
        }

        let mut samples = decode_wav_to_f32(&audio).map_err(SttError::Other)?;
        if samples.is_empty() {
            return Ok(TranscriptionResult::default());
        }
        // Belt-and-suspenders: cap decoded length too, so even a WAV whose
        // header understates its data chunk can't push the engine past the
        // duration ceiling.
        samples.truncate(MAX_LOCAL_STT_SECS * 16_000);

        // Mark activity so the keep-alive thread keeps the model warm for the
        // user's next utterance instead of letting it go cold between phrases.
        if let Ok(mut t) = self.last_used.lock() {
            *t = Instant::now();
        }

        let mut engine = self
            .engine
            .lock()
            .map_err(|_| SttError::Other(anyhow::anyhow!("local engine mutex poisoned")))?;

        // Audio duration is what the hallucination filter needs — NOT processing
        // time. Processing time (how fast the engine ran) is logged separately.
        let audio_duration_secs = samples.len() as f64 / 16_000.0;
        let started = std::time::Instant::now();
        let text =
            run_engine(&mut engine, &samples, self.language.clone()).map_err(SttError::Other)?;
        let elapsed = started.elapsed().as_secs_f64();
        tracing::debug!(
            elapsed_ms = (elapsed * 1000.0) as u64,
            audio_secs = audio_duration_secs,
            "local_stt_done"
        );

        Ok(TranscriptionResult {
            text: text.trim().to_string(),
            duration: audio_duration_secs,
            language: self.language.clone().unwrap_or_default(),
            segments: vec![],
        })
    }

    fn transcribe_streaming(
        &self,
        audio: Vec<u8>,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamingTranscriptionResult, SttError>> + Send>> {
        if self.supports_streaming {
            tracing::info!("local_stt_streaming_enabled");
            // TODO: Implement actual streaming for streaming-capable models.
            // For now, fall back to non-streaming behavior with a single final result.
            // This is a placeholder that can be extended to use transcribe-rs's
            // streaming API when available for the specific engine type.
        } else {
            tracing::debug!("local_stt_streaming_not_supported");
        }

        // For now, all models use non-streaming behavior.
        // Streaming models (MoonshineStreaming, etc.) can be extended here
        // to use their streaming APIs.
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

fn run_engine(
    engine: &mut LoadedEngine,
    samples: &[f32],
    language: Option<String>,
) -> Result<String> {
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
        #[cfg(feature = "local-stt")]
        LoadedEngine::Nemotron(n) => {
            // Nemotron is a streaming model, but for non-streaming transcription
            // we transcribe the entire audio at once using transcribe_audio.
            // It returns a String directly, so we wrap it in a TranscriptionResult.
            let text = n.transcribe_audio(samples)
                .map_err(|e| anyhow::anyhow!("nemotron transcribe failed: {e}"))?;
            transcribe_rs::TranscriptionResult { text, segments: None }
        }
        #[cfg(not(feature = "local-stt"))]
        LoadedEngine::Nemotron(_) => {
            anyhow::bail!("Nemotron requires the local-stt feature")
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
