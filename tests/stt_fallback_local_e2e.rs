//! End-to-end proof that the Groq→local fallback chain can really
//! transcribe: a primary whose breaker is permanently open forces
//! [`FallbackTranscriber`] to load the best downloaded model (resolved
//! through the real [`ModelRegistry`], same policy as production's
//! `pick_fallback_model`) and decode actual speech audio.
//!
//! Ignored by default — it needs a verified local model on disk and the
//! macOS `say` binary to synthesize the fixture. Run locally with:
//!
//! ```sh
//! cargo test --features local-stt --test stt_fallback_local_e2e -- --ignored
//! ```
#![cfg(feature = "local-stt")]

use std::pin::Pin;
use std::process::Command;
use std::sync::Arc;

use always::stt::{SttError, StreamingTranscriptionResult, Transcriber, TranscriptionResult};
use always::stt_dispatch::FallbackTranscriber;
use futures::stream::Stream;

struct BreakerAlwaysOpen;

impl Transcriber for BreakerAlwaysOpen {
    fn transcribe_from_bytes(&self, _audio: Vec<u8>) -> Result<TranscriptionResult, SttError> {
        Err(SttError::Unavailable {
            remaining_ms: 60_000,
        })
    }

    fn transcribe_streaming(
        &self,
        _audio: Vec<u8>,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamingTranscriptionResult, SttError>> + Send>> {
        let err = SttError::Unavailable {
            remaining_ms: 60_000,
        };
        Box::pin(futures::stream::once(async move { Err(err) }))
    }
}

/// Synthesize a spoken WAV fixture without playing any audio —
/// `say -o` renders straight to the file.
fn spoken_wav(text: &str) -> Vec<u8> {
    let dir = std::env::temp_dir().join("always-fallback-e2e");
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    let path = dir.join("fixture.wav");
    let status = Command::new("say")
        .args([
            "-v",
            "Samantha",
            "--file-format=WAVE",
            "--data-format=LEI16@16000",
            "-o",
        ])
        .arg(&path)
        .arg(text)
        .status()
        .expect("run say");
    assert!(status.success(), "say failed to render fixture");
    std::fs::read(&path).expect("read fixture wav")
}

#[test]
#[ignore = "requires a downloaded+verified local model and macOS `say`"]
fn open_breaker_transcribes_with_real_local_model() {
    let registry = always::managers::model_registry::ModelRegistry::new().expect("model registry");

    // Same selection policy as stt_dispatch::pick_fallback_model.
    let info = registry
        .list()
        .into_iter()
        .filter(|m| m.is_downloaded && !m.is_downloading)
        .max_by(|a, b| {
            (a.is_recommended, a.speed_score + a.accuracy_score)
                .partial_cmp(&(b.is_recommended, b.speed_score + b.accuracy_score))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .expect("no verified local model downloaded — download one in Settings → Models first");
    let path = registry.model_path(&info.id).expect("model path");
    let engine = info.engine_type;

    let t = FallbackTranscriber::new(
        Arc::new(BreakerAlwaysOpen),
        info.id.clone(),
        Box::new(move || {
            let local = always::always::stt_local::LocalTranscriber::load(
                engine,
                &path,
                None,
                info.supports_streaming,
            )?;
            Ok(Arc::new(local) as Arc<dyn Transcriber>)
        }),
    );

    let audio = spoken_wav("The quick brown fox jumps over the lazy dog.");
    let result = t
        .transcribe_from_bytes(audio)
        .expect("fallback transcription should succeed");
    let lower = result.text.to_lowercase();
    assert!(
        lower.contains("quick brown fox"),
        "unexpected transcript from {}: {:?}",
        info.id,
        result.text
    );
    println!("fallback model {} transcribed: {:?}", info.id, result.text);
}
