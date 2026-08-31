//! Speech-to-text with Groq Whisper API.
//!
//! Resilience contract:
//!
//! * 429 / 502 / 503 / 504 / network errors are retried up to 3 times with
//!   exponential backoff (200 ms → 400 ms → 800 ms) plus 0–25 % jitter.
//! * Other 4xx responses fail immediately — they indicate caller error
//!   (auth, payload) and are not transient.
//! * A circuit breaker opens for 60 s after 3 consecutive transcription
//!   failures and short-circuits further calls with [`SttError::Unavailable`].
//!   This turns slow, repeated failures into a fast failure for the duration
//!   of the cool-down — it does not itself perform any fallback. The dispatch
//!   layer opts into a local strategy via [`SttError::should_fall_back`]
//!   (see `stt_dispatch::FallbackTranscriber`); the breaker only spares the
//!   user from stalling on each call while Groq is down.

use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::stream::Stream;
use rand::Rng;
use reqwest::StatusCode;
use reqwest::blocking::multipart as blocking_multipart;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const GROQ_TRANSCRIPTIONS_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
pub const GROQ_MODEL_NAME: &str = "whisper-large-v3-turbo";
const WHISPER_MODEL: &str = GROQ_MODEL_NAME;

const MAX_ATTEMPTS: u32 = 3;
const BASE_BACKOFF_MS: u64 = 200;
const CIRCUIT_OPEN_FAILURES: u32 = 3;
const CIRCUIT_OPEN_DURATION: Duration = Duration::from_secs(60);

/// Process-wide guard for tests that mutate `ALWAYS_GROQ_BASE_URL`.
/// Environment variables are global, so unit and integration tests must
/// share one lock when pointing the Groq client at mock servers.
#[doc(hidden)]
pub static GROQ_BASE_URL_ENV_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

/// Consecutive transcription failures since the last success.
static CONSECUTIVE_FAILURES: AtomicU32 = AtomicU32::new(0);
/// Unix-millis until which the circuit remains open. `0` = closed.
static CIRCUIT_OPEN_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

/// Failure modes for [`transcribe_from_bytes`].
///
/// `RateLimited` and `ServerError` are *transient* — they may succeed on
/// retry and ultimately bubble up only after the retry budget is exhausted.
/// `Unavailable` indicates the circuit breaker is open: the call fails fast
/// for the remainder of the cool-down instead of stalling. It does not by
/// itself switch to any local strategy — callers may opt into one by checking
/// [`SttError::should_fall_back`].
#[derive(Debug, Error)]
pub enum SttError {
    #[error("Groq rate-limited the request after {attempts} attempts")]
    RateLimited { attempts: u32 },
    #[error("Groq returned a server error after {attempts} attempts (last status {status})")]
    ServerError { status: u16, attempts: u32 },
    #[error("network error talking to Groq after {attempts} attempts: {source}")]
    Network {
        attempts: u32,
        #[source]
        source: reqwest::Error,
    },
    #[error("STT provider returned an unrecoverable error (HTTP {status}): {body}")]
    ClientError { status: u16, body: String },
    #[error(
        "Groq transcription circuit breaker is open (cool-down {remaining_ms} ms remaining); \
         falling back to local heuristics"
    )]
    Unavailable { remaining_ms: u64 },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl SttError {
    /// Whether the daemon should bypass STT entirely on this error and go
    /// straight to the rule-based fallback. Currently only the circuit
    /// breaker triggers this — every other failure has already exhausted
    /// the retry budget and should bubble up to the user as a real error.
    pub fn should_fall_back(&self) -> bool {
        matches!(self, SttError::Unavailable { .. })
    }
}

fn auth_header(api_key: &str) -> String {
    // Derive the header per call from the key passed in. Previously memoized
    // in a process-wide OnceLock that silently ignored the argument after the
    // first call — a latent wrong-key bug once a runtime key-update lands.
    format!("Bearer {}", api_key)
}

/// Resolve the Groq transcription endpoint URL.
///
/// Honors the `ALWAYS_GROQ_BASE_URL` environment variable so integration tests
/// can point the client at a wiremock server. The override base URL is appended
/// with `/openai/v1/audio/transcriptions`. Defaults to the production Groq URL
/// when the variable is unset.
fn transcriptions_url() -> String {
    std::env::var("ALWAYS_GROQ_BASE_URL")
        .map(|base| {
            format!(
                "{}/openai/v1/audio/transcriptions",
                base.trim_end_matches('/')
            )
        })
        .unwrap_or_else(|_| GROQ_TRANSCRIPTIONS_URL.to_string())
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TranscriptionSegment {
    #[serde(default)]
    pub id: u32,
    #[serde(default)]
    pub start: f64,
    #[serde(default)]
    pub end: f64,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub no_speech_prob: f32,
    #[serde(default)]
    pub avg_logprob: f32,
    #[serde(default)]
    pub compression_ratio: f32,
    #[serde(default)]
    pub temperature: f32,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TranscriptionResult {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub segments: Vec<TranscriptionSegment>,
}

/// Streaming transcription result with progressive updates.
///
/// Streaming ASR returns interim (partial) and final results as audio chunks
/// are processed. This type represents a single update in the stream.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct StreamingTranscriptionResult {
    /// Current partial or final text.
    #[serde(default)]
    pub text: String,
    /// Whether this result is final (no further updates for this segment).
    #[serde(default)]
    pub is_final: bool,
    /// Whether this result is interim (partial, may change).
    #[serde(default)]
    pub is_interim: bool,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn circuit_block_remaining_ms() -> Option<u64> {
    let until = CIRCUIT_OPEN_UNTIL_MS.load(Ordering::Acquire);
    let now = now_ms();
    if until > now { Some(until - now) } else { None }
}

fn record_success() {
    CONSECUTIVE_FAILURES.store(0, Ordering::Release);
    CIRCUIT_OPEN_UNTIL_MS.store(0, Ordering::Release);
}

fn record_failure() {
    let prev = CONSECUTIVE_FAILURES.fetch_add(1, Ordering::AcqRel);
    if prev + 1 >= CIRCUIT_OPEN_FAILURES {
        let until = now_ms() + CIRCUIT_OPEN_DURATION.as_millis() as u64;
        CIRCUIT_OPEN_UNTIL_MS.store(until, Ordering::Release);
        tracing::warn!(
            failures = prev + 1,
            cool_down_ms = CIRCUIT_OPEN_DURATION.as_millis() as u64,
            "Groq STT circuit breaker opened"
        );
    }
}

/// Reset the global circuit breaker. Test-only — public so integration
/// tests in `tests/` (which link the lib without `cfg(test)`) can serialize
/// against a clean state.
#[doc(hidden)]
pub fn reset_circuit_breaker_for_test() {
    CONSECUTIVE_FAILURES.store(0, Ordering::Release);
    CIRCUIT_OPEN_UNTIL_MS.store(0, Ordering::Release);
}

fn backoff_with_jitter(attempt: u32) -> Duration {
    let base = BASE_BACKOFF_MS << attempt; // 200, 400, 800, ...
    let jitter = rand::thread_rng().gen_range(0..=base / 4);
    Duration::from_millis(base + jitter)
}

fn build_form(audio_data: Vec<u8>) -> Result<blocking_multipart::Form, anyhow::Error> {
    // Whisper's `prompt` field is intentionally NOT used. It was tried
    // historically and produced "I have an idea" → "I have an IntelliJ
    // IDEA" because the bias listed every app on disk. Voice-to-text
    // delivers what was said. Glossary corrections happen downstream
    // in the optional LLM postprocess pass (`postprocess_enabled`).
    let form = blocking_multipart::Form::new()
        .part(
            "file",
            blocking_multipart::Part::bytes(audio_data)
                .file_name("audio.wav")
                .mime_str("audio/wav")?,
        )
        .text("model", WHISPER_MODEL)
        .text("response_format", "verbose_json")
        .text("language", "en");
    Ok(form)
}

/// Transcribe audio via Groq Whisper.
///
/// Retries on transient failures (429 / 5xx / network) with exponential
/// backoff. Honors a process-wide circuit breaker that opens after 3
/// consecutive failures so callers can short-circuit to local heuristics
/// when Groq is down.
///
/// Note: the body is consumed once per attempt — the audio buffer is
/// cloned for retries. This is acceptable because utterances are O(MB),
/// not O(GB).
pub fn transcribe_from_bytes(
    audio_data: Vec<u8>,
    api_key: &str,
) -> Result<TranscriptionResult, SttError> {
    if let Some(remaining_ms) = circuit_block_remaining_ms() {
        return Err(SttError::Unavailable { remaining_ms });
    }

    let client = crate::http_client::blocking();
    let url = transcriptions_url();
    let auth = auth_header(api_key);

    let mut last_status: Option<StatusCode> = None;
    let mut last_network_err: Option<reqwest::Error> = None;
    let mut last_parse_err: Option<reqwest::Error> = None;

    for attempt in 0..MAX_ATTEMPTS {
        // Multipart bodies cannot be cheaply replayed, so rebuild per attempt.
        let form = match build_form(audio_data.clone()) {
            Ok(f) => f,
            Err(e) => {
                record_failure();
                return Err(SttError::Other(e));
            }
        };

        let response = client
            .post(&url)
            .header("Authorization", auth.clone())
            .multipart(form)
            .send();

        match response {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    match resp.json::<TranscriptionResult>() {
                        Ok(mut result) => {
                            result.text = result.text.trim().to_string();
                            record_success();
                            return Ok(result);
                        }
                        // A 2xx with an unparseable body (truncated / mid-stream
                        // drop, or an HTML error page returned as 200 by a
                        // proxy) is transient — retry like a network error
                        // instead of failing the utterance permanently.
                        Err(e) => {
                            tracing::warn!(
                                attempt = attempt + 1,
                                error = %e,
                                "Groq STT 2xx with unparseable body; will retry"
                            );
                            last_parse_err = Some(e);
                            if attempt + 1 < MAX_ATTEMPTS {
                                std::thread::sleep(backoff_with_jitter(attempt));
                            }
                            continue;
                        }
                    }
                }

                last_status = Some(status);
                if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    tracing::warn!(
                        status = status.as_u16(),
                        attempt = attempt + 1,
                        "Groq STT transient error; will retry"
                    );
                    if attempt + 1 < MAX_ATTEMPTS {
                        std::thread::sleep(backoff_with_jitter(attempt));
                    }
                    continue;
                }

                // 4xx (other than 429) is unrecoverable — caller must fix payload/auth.
                let body = resp.text().unwrap_or_default();
                record_failure();
                return Err(SttError::ClientError {
                    status: status.as_u16(),
                    body,
                });
            }
            Err(e) => {
                tracing::warn!(
                    attempt = attempt + 1,
                    error = %e,
                    "Groq STT network error; will retry"
                );
                last_network_err = Some(e);
                if attempt + 1 < MAX_ATTEMPTS {
                    std::thread::sleep(backoff_with_jitter(attempt));
                }
            }
        }
    }

    record_failure();
    if let Some(status) = last_status {
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(SttError::RateLimited {
                attempts: MAX_ATTEMPTS,
            });
        }
        return Err(SttError::ServerError {
            status: status.as_u16(),
            attempts: MAX_ATTEMPTS,
        });
    }

    if let Some(source) = last_network_err {
        return Err(SttError::Network {
            attempts: MAX_ATTEMPTS,
            source,
        });
    }

    if let Some(source) = last_parse_err {
        return Err(SttError::Other(anyhow::anyhow!(
            "failed to parse Groq Whisper response: {source}"
        )));
    }

    Err(SttError::Other(anyhow::anyhow!(
        "transcribe_from_bytes exhausted retries with no recorded outcome"
    )))
}

/// Speech-to-text capability.
///
/// Trait so the daemon can be exercised end-to-end with a deterministic
/// in-memory implementation in tests, and so future backends (offline
/// Whisper, Deepgram, etc.) can plug in without touching `event_loop`.
pub trait Transcriber: Send + Sync {
    fn transcribe_from_bytes(&self, audio: Vec<u8>) -> Result<TranscriptionResult, SttError>;

    /// Stream transcription results progressively.
    ///
    /// Returns a stream of [`StreamingTranscriptionResult`] updates.
    /// Backends that don't support streaming should yield a single final
    /// result with `is_final = true`.
    ///
    /// This is an async method to support backends that may need to
    /// perform I/O or yield control while processing audio chunks.
    ///
    /// The returned stream must be 'static (not borrow from self).
    fn transcribe_streaming(
        &self,
        audio: Vec<u8>,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamingTranscriptionResult, SttError>> + Send>>;

    /// Whether this transcriber genuinely streams partial results — i.e.
    /// `transcribe_streaming` yields more than one chunk with real interim
    /// text, not just a single `is_final` result. Used to gate the live
    /// overlay preview loop (`vad.rs`) so it only fires continuously during
    /// active speech for engines cheap/fast enough to afford it (local
    /// cache-aware streaming engines), not for cloud backends where that
    /// would mean a network round trip every ~200ms. Defaults to `false`;
    /// override only where `transcribe_streaming` is a real implementation.
    fn supports_streaming(&self) -> bool {
        false
    }

    /// Open a PERSISTENT cache-aware transcription session for ONE utterance.
    ///
    /// This is the difference between "streaming" as a presentation trick and
    /// streaming as the actual decode. [`Self::transcribe_streaming`] takes a
    /// COMPLETE utterance and merely emits it in pieces — every call decodes
    /// the whole clip from scratch. The session returned here is fed audio as
    /// it is captured and keeps encoder/decoder state across chunks, so the
    /// transcript is built WHILE the user speaks and finalization costs one
    /// flush chunk (~50 ms) instead of a full re-decode (~0.10x realtime, i.e.
    /// 4 s for a 40 s utterance).
    ///
    /// Returns `None` for engines with no cache-aware streaming mode; callers
    /// must keep their one-shot path as the fallback.
    fn open_live_stream(&self) -> Option<Box<dyn LiveTranscriptionStream>> {
        None
    }
}

/// One persistent, cache-aware decode session covering a single utterance.
///
/// Contract:
/// - [`Self::push_chunk`] is called with EXACTLY [`Self::chunk_samples`]
///   f32 samples (16 kHz mono, `-1.0..=1.0`), in capture order, once each.
///   Shorter slices are zero-padded by the implementation, but doing that
///   mid-utterance injects silence into the decode — only pad the tail.
/// - Every accessor returns the FULL transcript so far, never a delta.
///   Concatenating per-chunk deltas splits words at chunk boundaries
///   ("whe ther", "finali zes") because the tokenizer emits sub-word pieces.
/// - [`Self::finish`] drains the decoder and returns the final transcript.
///   The session must not be used afterwards.
pub trait LiveTranscriptionStream: Send {
    /// Samples per [`Self::push_chunk`] call. Nemotron's cache-aware encoder
    /// window is 560 ms = 8960 samples at 16 kHz.
    fn chunk_samples(&self) -> usize;

    /// Feed one chunk. Returns the full transcript accumulated so far.
    fn push_chunk(&mut self, samples: &[f32]) -> Result<String, SttError>;

    /// Flush the decoder (one trailing silent window) and return the final
    /// full transcript.
    fn finish(&mut self) -> Result<String, SttError>;
}

/// Production [`Transcriber`] backed by the Groq Whisper API. Holds the
/// API key and delegates to [`transcribe_from_bytes`] with its retry +
/// circuit-breaker behavior.
pub struct GroqTranscriber {
    api_key: String,
}

impl GroqTranscriber {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
        }
    }
}

impl Transcriber for GroqTranscriber {
    fn transcribe_from_bytes(&self, audio: Vec<u8>) -> Result<TranscriptionResult, SttError> {
        transcribe_from_bytes(audio, &self.api_key)
    }

    fn transcribe_streaming(
        &self,
        audio: Vec<u8>,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamingTranscriptionResult, SttError>> + Send>> {
        // Groq doesn't support streaming, so we wrap the non-streaming result
        // in a single-element stream with is_final = true.
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

#[cfg(test)]
mod live_stream_trait_tests {
    use super::*;

    struct PlainTranscriber;

    impl Transcriber for PlainTranscriber {
        fn transcribe_from_bytes(&self, _audio: Vec<u8>) -> Result<TranscriptionResult, SttError> {
            Ok(TranscriptionResult::default())
        }
        fn transcribe_streaming(
            &self,
            _audio: Vec<u8>,
        ) -> Pin<Box<dyn Stream<Item = Result<StreamingTranscriptionResult, SttError>> + Send>>
        {
            Box::pin(futures::stream::empty())
        }
    }

    /// Every backend that has not opted in must report "no live session", so
    /// the capture loop keeps its untouched speculative/one-shot path. A
    /// default of `Some` would silently route Groq dictation through a
    /// session it cannot serve.
    #[test]
    fn transcribers_have_no_live_session_by_default() {
        assert!(PlainTranscriber.open_live_stream().is_none());
        assert!(!PlainTranscriber.supports_streaming());
    }

    #[test]
    fn groq_has_no_live_session() {
        assert!(GroqTranscriber::new("k").open_live_stream().is_none());
    }
}

#[cfg(test)]
mod stt_unit_tests {
    use super::*;

    /// The circuit-breaker counters are process-global atomics; the two
    /// tests below interleave under the parallel test runner and trip
    /// each other's thresholds. Serialize them.
    static CIRCUIT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn transcriptions_url_default() {
        let _guard = GROQ_BASE_URL_ENV_LOCK
            .lock()
            .expect("GROQ_BASE_URL_ENV_LOCK poisoned");
        unsafe { std::env::remove_var("ALWAYS_GROQ_BASE_URL") };
        assert_eq!(transcriptions_url(), GROQ_TRANSCRIPTIONS_URL);
    }

    #[test]
    fn transcriptions_url_honors_override() {
        let _guard = GROQ_BASE_URL_ENV_LOCK
            .lock()
            .expect("GROQ_BASE_URL_ENV_LOCK poisoned");
        unsafe { std::env::set_var("ALWAYS_GROQ_BASE_URL", "http://localhost:9999/") };
        assert_eq!(
            transcriptions_url(),
            "http://localhost:9999/openai/v1/audio/transcriptions"
        );
        unsafe { std::env::remove_var("ALWAYS_GROQ_BASE_URL") };
    }

    #[test]
    fn backoff_grows_exponentially() {
        // jitter is 0..=base/4 INCLUSIVE, so attempt-N delay is in
        // [base, 1.25 * base] — the ranges must be inclusive at the top
        // or this test fails 1-in-51 runs when the jitter rolls its max.
        let d0 = backoff_with_jitter(0).as_millis();
        let d1 = backoff_with_jitter(1).as_millis();
        let d2 = backoff_with_jitter(2).as_millis();
        assert!((200..=250).contains(&d0));
        assert!((400..=500).contains(&d1));
        assert!((800..=1000).contains(&d2));
    }

    #[test]
    fn record_failure_opens_circuit_after_threshold() {
        let _guard = CIRCUIT_TEST_LOCK
            .lock()
            .expect("circuit test lock poisoned");
        reset_circuit_breaker_for_test();
        for _ in 0..CIRCUIT_OPEN_FAILURES {
            record_failure();
        }
        assert!(circuit_block_remaining_ms().is_some());
        reset_circuit_breaker_for_test();
        assert!(circuit_block_remaining_ms().is_none());
    }

    #[test]
    fn record_success_resets_failure_count() {
        let _guard = CIRCUIT_TEST_LOCK
            .lock()
            .expect("circuit test lock poisoned");
        reset_circuit_breaker_for_test();
        record_failure();
        record_failure();
        record_success();
        // Two more failures should NOT trip the breaker because count was reset.
        record_failure();
        record_failure();
        assert!(circuit_block_remaining_ms().is_none());
        reset_circuit_breaker_for_test();
    }
}

#[cfg(test)]
pub mod mock {
    //! Test double for [`Transcriber`].

    use super::{StreamingTranscriptionResult, SttError, Transcriber, TranscriptionResult};
    use futures::stream::Stream;
    use parking_lot::Mutex;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::sync::Arc;

    /// A queueing transcriber. Returns successive scripted outcomes; once
    /// exhausted it returns the trailing `default` outcome (or an error
    /// if none was supplied).
    #[derive(Default)]
    pub struct MockTranscriber {
        queue: Arc<Mutex<VecDeque<Result<TranscriptionResult, SttError>>>>,
        invocations: Arc<Mutex<u32>>,
    }

    impl MockTranscriber {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn enqueue(&self, outcome: Result<TranscriptionResult, SttError>) {
            self.queue.lock().push_back(outcome);
        }
        pub fn enqueue_text(&self, text: &str) {
            self.enqueue(Ok(TranscriptionResult {
                text: text.to_string(),
                duration: 1.0,
                language: "en".to_string(),
                segments: vec![],
            }));
        }
        pub fn invocations(&self) -> u32 {
            *self.invocations.lock()
        }
    }

    impl Transcriber for MockTranscriber {
        fn transcribe_from_bytes(&self, _audio: Vec<u8>) -> Result<TranscriptionResult, SttError> {
            *self.invocations.lock() += 1;
            self.queue.lock().pop_front().unwrap_or_else(|| {
                Err(SttError::Other(anyhow::anyhow!(
                    "MockTranscriber queue exhausted"
                )))
            })
        }

        fn transcribe_streaming(
            &self,
            audio: Vec<u8>,
        ) -> Pin<Box<dyn Stream<Item = Result<StreamingTranscriptionResult, SttError>> + Send>>
        {
            // MockTranscriber doesn't support streaming, so we wrap the non-streaming result.
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

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn enqueue_text_returns_in_fifo_order() {
            let m = MockTranscriber::new();
            m.enqueue_text("first");
            m.enqueue_text("second");
            assert_eq!(m.transcribe_from_bytes(vec![]).unwrap().text, "first");
            assert_eq!(m.transcribe_from_bytes(vec![]).unwrap().text, "second");
            assert_eq!(m.invocations(), 2);
        }

        #[test]
        fn empty_queue_returns_other_error() {
            let m = MockTranscriber::new();
            match m.transcribe_from_bytes(vec![]) {
                Err(SttError::Other(_)) => {}
                other => panic!("expected SttError::Other, got {:?}", other.map(|_| "Ok")),
            }
        }

        #[test]
        fn provider_is_object_safe() {
            let _: Box<dyn Transcriber> = Box::new(MockTranscriber::new());
        }
    }
}
