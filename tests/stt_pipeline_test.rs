//! Focused integration tests for the STT/filter/hallucination pipeline.
//!
//! These exercise the public Rust APIs with realistic inputs (and a mocked Groq
//! HTTP endpoint via wiremock) without driving the full audio capture path —
//! VAD-driven recording requires a live microphone and is covered separately by
//! benchmarks.

use always::always::AlwaysConfig;
use always::always::filter::{FilterReason, should_accept_with_reason};
use always::always::hallucination::is_hallucination;
use always::stt::{TranscriptionResult, TranscriptionSegment, transcribe_from_bytes};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Minimal RIFF/WAVE bytes for a 1-frame, 16 kHz mono PCM file. Just enough to
/// satisfy multipart upload — wiremock doesn't validate the audio content.
fn dummy_wav_bytes() -> Vec<u8> {
    let mut data = Vec::with_capacity(44 + 2);
    data.extend_from_slice(b"RIFF");
    data.extend_from_slice(&36u32.to_le_bytes()); // chunk size (placeholder)
    data.extend_from_slice(b"WAVE");
    data.extend_from_slice(b"fmt ");
    data.extend_from_slice(&16u32.to_le_bytes()); // subchunk1 size
    data.extend_from_slice(&1u16.to_le_bytes()); // PCM
    data.extend_from_slice(&1u16.to_le_bytes()); // mono
    data.extend_from_slice(&16000u32.to_le_bytes()); // sample rate
    data.extend_from_slice(&32000u32.to_le_bytes()); // byte rate
    data.extend_from_slice(&2u16.to_le_bytes()); // block align
    data.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    data.extend_from_slice(b"data");
    data.extend_from_slice(&2u32.to_le_bytes()); // data size
    data.extend_from_slice(&0i16.to_le_bytes()); // 1 sample of silence
    data
}

#[tokio::test]
async fn transcribe_from_bytes_hits_mock_groq_endpoint() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/openai/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "text": "hello world",
            "duration": 1.0,
            "language": "en",
            "segments": []
        })))
        .mount(&server)
        .await;

    // Scope the env var override under the shared STT env lock. The Groq
    // base URL is process-global, so this must not overlap with other STT
    // tests that point the client at their own mock server.
    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let _env_guard = always::stt::GROQ_BASE_URL_ENV_LOCK
            .lock()
            .expect("GROQ_BASE_URL_ENV_LOCK poisoned");
        unsafe {
            std::env::set_var("ALWAYS_GROQ_BASE_URL", &base_url);
        }
        let result = transcribe_from_bytes(dummy_wav_bytes(), "test_key");
        unsafe {
            std::env::remove_var("ALWAYS_GROQ_BASE_URL");
        }
        result
    })
    .await
    .expect("spawn_blocking failed")
    .expect("transcribe_from_bytes failed");

    assert_eq!(result.text, "hello world");
    assert_eq!(result.language, "en");
    assert!((result.duration - 1.0).abs() < f64::EPSILON);
}

fn make_result(text: &str) -> TranscriptionResult {
    TranscriptionResult {
        text: text.to_string(),
        duration: 0.0,
        language: "en".to_string(),
        segments: Vec::new(),
    }
}

#[test]
fn hallucination_rejects_known_canned_phrases() {
    // Whisper's "watching" / outro family.
    assert!(is_hallucination(&make_result("Thank you for watching")).is_some());
    assert!(is_hallucination(&make_result("Thanks for watching!")).is_some());
    assert!(is_hallucination(&make_result("Please subscribe and like")).is_some());

    // "Bye." spammed on silence.
    assert!(is_hallucination(&make_result("Bye. Bye. Bye.")).is_some());

    // Mixed-script gibberish that Whisper sometimes emits on noise.
    assert!(is_hallucination(&make_result("ünkLöß I Nativuchy Dk")).is_some());

    // Subtitle credit hallucination.
    assert!(is_hallucination(&make_result("Subtitles by the Amara.org community")).is_some());
}

#[test]
fn hallucination_rejects_decoder_fallback_with_low_confidence() {
    let mut result = make_result("the quick brown fox jumps");
    result.duration = 2.0;
    result.segments.push(TranscriptionSegment {
        id: 0,
        start: 0.0,
        end: 2.0,
        text: "the quick brown fox jumps".to_string(),
        no_speech_prob: 0.1,
        avg_logprob: -1.5,
        compression_ratio: 1.5,
        temperature: 0.4,
    });
    assert!(is_hallucination(&result).is_some());
}

#[test]
fn hallucination_accepts_real_speech() {
    assert!(is_hallucination(&make_result("open the project folder please")).is_none());
    assert!(is_hallucination(&make_result("write a Rust function that parses JSON")).is_none());
}

#[test]
fn filter_accepts_normal_speech() {
    let cfg = AlwaysConfig::default();
    let r = should_accept_with_reason("open the file please", &cfg);
    assert!(matches!(r, FilterReason::None), "got {:?}", r);
}

#[test]
fn filter_rejects_when_text_matches_blocklist_phrase() {
    let cfg = AlwaysConfig::default();
    for phrase in ["thank you", "Mwah!", "Pfff.", "I'm going to go."] {
        let r = should_accept_with_reason(phrase, &cfg);
        assert!(
            !matches!(r, FilterReason::None),
            "expected {phrase:?} to be filtered, got {:?}",
            r
        );
    }
}
