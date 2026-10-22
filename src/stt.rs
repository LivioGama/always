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
/// UTC day (days since the epoch) the daily-quota notice was last sent.
static QUOTA_NOTICE_DAY: AtomicU64 = AtomicU64::new(u64::MAX);

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
    /// Engine-side signals about whether the audio was speech in the
    /// recognizer's language at all. Never sent over the wire.
    #[serde(skip)]
    pub evidence: RecognitionEvidence,
}

/// How an utterance's live (streaming) recognizer session ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LiveStreamOutcome {
    /// No live session ran for this utterance (other backend, chunked
    /// utterance, or the engine was unavailable) — no signal.
    #[default]
    NotRun,
    /// Session ended without a verdict (timed out before any result).
    Unknown,
    /// The live recognizer produced words.
    Heard,
    /// The live recognizer finished cleanly having recognized NO word of
    /// its locale's language across the whole utterance.
    NoWords,
    /// Recognition failed — says nothing about the audio.
    Failed,
}

/// Already-computed engine signals the foreign-speech gate
/// (`hallucination::foreign_speech`) reads. All optional: an engine that
/// does not report one simply leaves it at its default (= no signal).
#[derive(Debug, Clone, Default)]
pub struct RecognitionEvidence {
    /// Mean per-word acoustic confidence in `[0, 1]` (Apple
    /// SpeechTranscriber `.transcriptionConfidence`).
    pub confidence: Option<f32>,
    /// Outcome of the live Apple streaming session for this utterance.
    pub live_stream: LiveStreamOutcome,
    /// Final text of the English-only Parakeet preview engine, when it ran
    /// for this utterance (`Some("")` = it ran and heard nothing).
    pub preview_text: Option<String>,
    /// Parts of a chunked dictation whose speech could not be transcribed
    /// (audio saved to the failed-chunks dir): the text is incomplete, so
    /// the paste path cues the loss and does not auto-submit it.
    pub unrecovered_parts: usize,
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

/// Groq's `*-requests` headers always describe the per-day quota (RPD), so
/// none remaining on a 429 means the daily quota is used up, not a burst.
fn daily_quota_exhausted(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get("x-ratelimit-remaining-requests")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim() == "0")
}

/// True for the first caller on a given UTC day: the GUI asks once a day.
fn first_quota_notice_on(day: u64) -> bool {
    QUOTA_NOTICE_DAY.swap(day, Ordering::AcqRel) != day
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

/// Whisper `language` field for the user's configured language: a bare
/// ISO-639-1 code (`fr-FR` → `fr`). `auto`/unset resolves to the system
/// locale's language — what the Apple recognizer does for `auto` — and to
/// `en` (the pre-2026-10-16 behaviour) when that is unknown. Anything that
/// is not a 2-letter code (or `haw`) falls back to `en` rather than being
/// sent as an invalid value.
#[must_use]
pub fn whisper_language(lang: &str) -> String {
    resolve_whisper_language(lang, crate::always::preview_asr::system_locale_language())
}

/// [`whisper_language`] with the system locale's language injected.
#[must_use]
pub fn resolve_whisper_language(lang: &str, system_language: Option<String>) -> String {
    let lang = lang.trim().to_ascii_lowercase();
    let code = if lang.is_empty() || lang == "auto" {
        system_language.unwrap_or_default().to_ascii_lowercase()
    } else {
        lang.split(['-', '_']).next().unwrap_or("").to_string()
    };
    let valid = code == "haw" || (code.len() == 2 && code.chars().all(|c| c.is_ascii_lowercase()));
    if valid { code } else { "en".to_string() }
}

fn build_form(
    audio_data: Vec<u8>,
    language: &str,
) -> Result<blocking_multipart::Form, anyhow::Error> {
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
        .text("language", language.to_string());
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
    transcribe_from_bytes_in(audio_data, api_key, "en")
}

/// [`transcribe_from_bytes`] in `language` (a Whisper ISO code, see
/// [`whisper_language`]).
pub fn transcribe_from_bytes_in(
    audio_data: Vec<u8>,
    api_key: &str,
    language: &str,
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
        let form = match build_form(audio_data.clone(), language) {
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
                if status == StatusCode::TOO_MANY_REQUESTS
                    && daily_quota_exhausted(resp.headers())
                    // Spend today's notice only when an app can show it;
                    // otherwise the next exhausted 429 tries again.
                    && crate::always::uds_server::has_clients()
                    && first_quota_notice_on(now_ms() / 86_400_000)
                {
                    tracing::warn!("groq_daily_quota_exhausted");
                    crate::always::event::global_broadcaster().groq_quota_exhausted();
                }
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
}

/// Production [`Transcriber`] backed by the Groq Whisper API. Holds the
/// API key and delegates to [`transcribe_from_bytes`] with its retry +
/// circuit-breaker behavior.
pub struct GroqTranscriber {
    api_key: String,
    language: String,
}

impl GroqTranscriber {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            language: "en".to_string(),
        }
    }

    /// Transcribe in the user's language (`cfg.lang`); `auto` resolves to the
    /// system locale's language. See [`whisper_language`].
    #[must_use]
    pub fn with_language(mut self, lang: &str) -> Self {
        self.language = whisper_language(lang);
        self
    }

    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }
}

impl Transcriber for GroqTranscriber {
    fn transcribe_from_bytes(&self, audio: Vec<u8>) -> Result<TranscriptionResult, SttError> {
        transcribe_from_bytes_in(audio, &self.api_key, &self.language)
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
mod stt_unit_tests {
    use super::*;

    #[test]
    fn whisper_language_follows_the_user() {
        let r = |l: &str, sys: Option<&str>| resolve_whisper_language(l, sys.map(String::from));
        // Explicit codes are sent as given, normalized to the bare code.
        assert_eq!(r("fr", Some("en")), "fr");
        assert_eq!(r("fr-FR", None), "fr");
        assert_eq!(r("en_GB", Some("fr")), "en");
        assert_eq!(r(" PT-br ", None), "pt");
        assert_eq!(r("haw", None), "haw");
        // `auto`/unset follow the system locale, `en` when unknown.
        assert_eq!(r("auto", Some("fr")), "fr");
        assert_eq!(r("", Some("de")), "de");
        assert_eq!(r("Auto", Some("en")), "en");
        assert_eq!(r("auto", None), "en");
        // Never an invalid value.
        assert_eq!(r("xx1", Some("fr")), "en");
        assert_eq!(r("french", Some("fr")), "en");
        assert_eq!(r("auto", Some("fil")), "en");
        assert_eq!(r("auto", Some("")), "en");
        // Default and builder.
        assert_eq!(GroqTranscriber::new("k").language(), "en");
        assert_eq!(
            GroqTranscriber::new("k").with_language("fr").language(),
            "fr"
        );
    }

    #[test]
    fn daily_quota_notice_only_on_zero_remaining_once_a_day() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let h = |v: &'static str| {
            let mut m = HeaderMap::new();
            m.insert("x-ratelimit-remaining-requests", HeaderValue::from_static(v));
            m
        };
        assert!(daily_quota_exhausted(&h("0")));
        // Daily requests left: a per-minute burst, not the daily quota.
        assert!(!daily_quota_exhausted(&h("1840")));
        assert!(!daily_quota_exhausted(&HeaderMap::new()));
        // Far-future days so no real request shares the gate.
        assert!(first_quota_notice_on(1_000_000));
        assert!(!first_quota_notice_on(1_000_000));
        assert!(first_quota_notice_on(1_000_001));
    }

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
                ..Default::default()
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
