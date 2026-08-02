//! Resilience tests for the Groq Whisper client.
//!
//! These run against a wiremock server pointed at by `ALWAYS_GROQ_BASE_URL`
//! and assert the contract documented at the top of `src/stt.rs`:
//!
//! * 429 + 5xx are retried up to `MAX_ATTEMPTS = 3` with backoff.
//! * Other 4xx fail immediately as `ClientError`.
//! * After `CIRCUIT_OPEN_FAILURES = 3` consecutive failures the breaker
//!   opens and short-circuits subsequent calls with `Unavailable` for
//!   `CIRCUIT_OPEN_DURATION = 60s`.
//!
//! `serial_test` would be ideal here; absent it, each test resets the
//! breaker state up front and uses unique fixtures so tests do not race.

use std::pin::Pin;
use std::sync::LazyLock;

use futures::Stream;
use tokio::sync::Mutex;

use always::stt::{
    SttError, StreamingTranscriptionResult, TranscriptionResult, reset_circuit_breaker_for_test, transcribe_from_bytes,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Serialize the resilience tests — the circuit breaker is process-global.
static SERIAL: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn dummy_wav_bytes() -> Vec<u8> {
    // 44-byte WAV header + 4 silent samples — Groq doesn't see this in tests.
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36u32 + 8).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&32_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&8u32.to_le_bytes());
    wav.extend_from_slice(&[0u8; 8]);
    wav
}

fn ok_body() -> serde_json::Value {
    serde_json::json!({
        "text": "hello",
        "duration": 1.0,
        "language": "en",
        "segments": []
    })
}

async fn transcribe_with_mock_base_url(
    server: &MockServer,
    api_key: &str,
) -> Result<TranscriptionResult, SttError> {
    let base_url = server.uri();
    let api_key = api_key.to_string();
    tokio::task::spawn_blocking(move || {
        let _env_guard = always::stt::GROQ_BASE_URL_ENV_LOCK
            .lock()
            .expect("GROQ_BASE_URL_ENV_LOCK poisoned");
        unsafe { std::env::set_var("ALWAYS_GROQ_BASE_URL", base_url) };
        let result = transcribe_from_bytes(dummy_wav_bytes(), &api_key);
        unsafe { std::env::remove_var("ALWAYS_GROQ_BASE_URL") };
        result
    })
    .await
    .expect("blocking task panicked")
}

#[tokio::test]
async fn retries_on_429_then_succeeds() {
    let _guard = SERIAL.lock().await;
    reset_circuit_breaker_for_test();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/openai/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/openai/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .mount(&server)
        .await;

    let result = transcribe_with_mock_base_url(&server, "test_key")
        .await
        .expect("third attempt should succeed");

    assert_eq!(result.text, "hello");
}

#[tokio::test]
async fn rate_limit_exhaustion_returns_rate_limited() {
    let _guard = SERIAL.lock().await;
    reset_circuit_breaker_for_test();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/openai/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    let result = transcribe_with_mock_base_url(&server, "test_key").await;

    match result {
        Err(SttError::RateLimited { attempts }) => assert_eq!(attempts, 3),
        other => panic!("expected RateLimited, got {:?}", other.map(|_| "Ok")),
    }
}

#[tokio::test]
async fn server_error_500_returns_server_error_after_retries() {
    let _guard = SERIAL.lock().await;
    reset_circuit_breaker_for_test();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/openai/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let result = transcribe_with_mock_base_url(&server, "test_key").await;

    match result {
        Err(SttError::ServerError { status, attempts }) => {
            assert_eq!(status, 500);
            assert_eq!(attempts, 3);
        }
        other => panic!("expected ServerError, got {:?}", other.map(|_| "Ok")),
    }
}

#[tokio::test]
async fn unrecoverable_4xx_does_not_retry() {
    let _guard = SERIAL.lock().await;
    reset_circuit_breaker_for_test();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/openai/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
        .expect(1)
        .mount(&server)
        .await;

    let result = transcribe_with_mock_base_url(&server, "bad_key").await;

    match result {
        Err(SttError::ClientError { status, .. }) => assert_eq!(status, 401),
        other => panic!("expected ClientError(401), got {:?}", other.map(|_| "Ok")),
    }
}

#[tokio::test]
async fn circuit_breaker_opens_after_three_consecutive_failures() {
    let _guard = SERIAL.lock().await;
    reset_circuit_breaker_for_test();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/openai/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    // Three failed transcriptions = three consecutive failures.
    for _ in 0..3 {
        let _ = transcribe_with_mock_base_url(&server, "test_key").await;
    }

    // The next call MUST be short-circuited rather than waiting for retries.
    let started = std::time::Instant::now();
    let outcome = transcribe_with_mock_base_url(&server, "test_key").await;
    let elapsed = started.elapsed();

    match outcome {
        Err(SttError::Unavailable { remaining_ms }) => {
            assert!(remaining_ms > 0);
            assert!(remaining_ms <= 60_000);
        }
        other => panic!("expected Unavailable, got {:?}", other.map(|_| "Ok")),
    }
    // Short-circuit must be much faster than three retry rounds (~1.5s+).
    assert!(
        elapsed.as_millis() < 200,
        "circuit breaker did not short-circuit; took {} ms",
        elapsed.as_millis()
    );

    reset_circuit_breaker_for_test();
}

#[tokio::test]
async fn should_fall_back_only_for_circuit_open() {
    assert!(SttError::Unavailable { remaining_ms: 1 }.should_fall_back());
    assert!(!SttError::RateLimited { attempts: 3 }.should_fall_back());
    assert!(
        !SttError::ServerError {
            status: 503,
            attempts: 3
        }
        .should_fall_back()
    );
}

#[tokio::test]
async fn open_breaker_degrades_to_local_fallback_instead_of_dropping() {
    use std::sync::Arc;

    use always::stt::{GroqTranscriber, Transcriber};
    use always::stt_dispatch::FallbackTranscriber;

    let _guard = SERIAL.lock().await;
    reset_circuit_breaker_for_test();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/openai/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    struct StubLocal;
    impl Transcriber for StubLocal {
        fn transcribe_from_bytes(&self, _audio: Vec<u8>) -> Result<TranscriptionResult, SttError> {
            Ok(TranscriptionResult {
                text: "offline transcript".into(),
                ..Default::default()
            })
        }

        fn transcribe_streaming(
            &self,
            _audio: Vec<u8>,
        ) -> Pin<Box<dyn Stream<Item = Result<StreamingTranscriptionResult, SttError>> + Send>> {
            let result = Ok(StreamingTranscriptionResult {
                text: "offline transcript".into(),
                is_final: true,
                is_interim: false,
            });
            Box::pin(futures::stream::once(async move { result }))
        }
    }

    let base_url = server.uri();
    let result = tokio::task::spawn_blocking(move || {
        let _env_guard = always::stt::GROQ_BASE_URL_ENV_LOCK
            .lock()
            .expect("GROQ_BASE_URL_ENV_LOCK poisoned");
        unsafe { std::env::set_var("ALWAYS_GROQ_BASE_URL", base_url) };

        let wrapped = FallbackTranscriber::new(
            Arc::new(GroqTranscriber::new("test_key")),
            "stub-model".into(),
            Box::new(|| Ok(Arc::new(StubLocal) as Arc<dyn Transcriber>)),
        );

        // Three real failures (retried internally) trip the breaker; the
        // wrapped transcriber must NOT fall back for these — they are
        // ServerError, not Unavailable.
        for _ in 0..3 {
            let err = wrapped
                .transcribe_from_bytes(dummy_wav_bytes())
                .expect_err("500s must surface while the breaker is closed");
            assert!(matches!(err, SttError::ServerError { .. }));
        }

        // Breaker is now open: the fourth utterance degrades to the
        // local engine instead of erroring for the whole cool-down.
        let result = wrapped.transcribe_from_bytes(dummy_wav_bytes());

        unsafe { std::env::remove_var("ALWAYS_GROQ_BASE_URL") };
        result
    })
    .await
    .expect("blocking task panicked");

    assert_eq!(
        result.expect("fallback should transcribe").text,
        "offline transcript"
    );
    reset_circuit_breaker_for_test();
}
