//! Picks the right [`Transcriber`] for the daemon's current config —
//! the only file aware of *both* the remote Groq path and the local
//! `transcribe-rs` engines.
//!
//! Why a dispatch module: `vad.rs` and `event_loop.rs` should only see
//! a `dyn Transcriber`. Local-vs-remote selection happens once at
//! daemon start (and again on hot-swap when the user changes the
//! active model from the Settings UI), so the rest of the codebase
//! stays backend-agnostic.

use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::stream::Stream;
use parking_lot::{Condvar, Mutex};

use crate::always::AlwaysConfig;
use crate::managers::model_registry::ModelRegistry;
use crate::stt::{GroqTranscriber, SttError, StreamingTranscriptionResult, Transcriber, TranscriptionResult};

/// Placeholder installed at daemon boot so the UDS server can bind before
/// the (potentially seconds-long) model load completes. Unlike a plain
/// "always errors" stub, this one **blocks** the calling thread until the
/// real transcriber is installed via [`ReadySignal::set`]. That way the
/// first utterance after launch waits for init instead of being dropped
/// with a "still initializing" error — which the user would experience as
/// "I spoke and nothing happened."
pub struct PendingTranscriber {
    slot: ReadySlot,
    timeout: Duration,
}

type ReadySlot = Arc<(Mutex<Option<Arc<dyn Transcriber>>>, Condvar)>;

/// Handle used by the background init thread to install the real
/// transcriber and wake any callers blocked in [`PendingTranscriber::transcribe_from_bytes`].
pub struct ReadySignal {
    slot: ReadySlot,
}

impl ReadySignal {
    pub fn set(&self, transcriber: Arc<dyn Transcriber>) {
        let (lock, cv) = &*self.slot;
        *lock.lock() = Some(transcriber);
        cv.notify_all();
    }
}

impl PendingTranscriber {
    /// Build a placeholder + signal pair. The placeholder is what callers
    /// see via the shared `ActiveTranscriber` slot; the signal is held by
    /// the init thread that builds the real backend.
    pub fn new() -> (Arc<Self>, ReadySignal) {
        let slot: ReadySlot = Arc::new((Mutex::new(None), Condvar::new()));
        let placeholder = Arc::new(Self {
            slot: Arc::clone(&slot),
            // Local model loads are the slow case — 3–10s for Parakeet/Canary/
            // Whisper cold load. 30s is generous; a longer wait means something
            // is wrong upstream (disk I/O, missing files) and we should surface
            // the error rather than hang forever.
            timeout: Duration::from_secs(30),
        });
        (placeholder, ReadySignal { slot })
    }

    fn wait_for_ready(&self) -> Option<Arc<dyn Transcriber>> {
        let (lock, cv) = &*self.slot;
        let mut guard = lock.lock();
        if let Some(t) = guard.clone() {
            return Some(t);
        }
        let result = cv.wait_for(&mut guard, self.timeout);
        if result.timed_out() && guard.is_none() {
            return None;
        }
        guard.clone()
    }
}

impl Transcriber for PendingTranscriber {
    fn transcribe_from_bytes(&self, audio: Vec<u8>) -> Result<TranscriptionResult, SttError> {
        match self.wait_for_ready() {
            Some(real) => real.transcribe_from_bytes(audio),
            None => Err(SttError::Other(anyhow::anyhow!(
                "transcriber still initializing after timeout — model load may have failed"
            ))),
        }
    }

    fn transcribe_streaming(
        &self,
        audio: Vec<u8>,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamingTranscriptionResult, SttError>> + Send>> {
        let real = match self.wait_for_ready() {
            Some(r) => Arc::clone(&r),
            None => {
                let err = SttError::Other(anyhow::anyhow!(
                    "transcriber still initializing after timeout — model load may have failed"
                ));
                return Box::pin(futures::stream::once(async move { Err(err) }));
            }
        };
        real.transcribe_streaming(audio)
    }
}

/// Builds the local engine the first time the fallback engages. Boxed
/// closure (rather than a concrete `LocalTranscriber`) so the decorator
/// itself stays feature-independent and unit-testable without `local-stt`.
type LocalLoader = Box<dyn Fn() -> Result<Arc<dyn Transcriber>> + Send + Sync>;

enum LocalSlot {
    Ready(Arc<dyn Transcriber>),
    /// The load failed once. Stay failed until the next daemon start /
    /// backend hot-swap — retrying a broken model load on every
    /// utterance would add seconds of stall to each one.
    Failed,
}

/// Decorator that rides a remote primary (Groq) and, while its circuit
/// breaker is open ([`SttError::should_fall_back`]), transcribes with a
/// lazily-loaded local engine instead of dropping utterances for the
/// whole cool-down window.
///
/// The local engine is only loaded on the first fallback (3–10 s for a
/// cold model load — that first utterance pays the price, subsequent
/// ones reuse the engine). If the local path also fails, the *primary*
/// error is returned so the user-facing toast classification stays
/// accurate about the original outage.
pub struct FallbackTranscriber {
    primary: Arc<dyn Transcriber>,
    /// Lazily-initialized local engine (`None` = not yet attempted).
    /// The mutex is intentionally held across the multi-second model
    /// load: concurrent utterances should wait for the one load instead
    /// of racing a second.
    local: Mutex<Option<LocalSlot>>,
    loader: LocalLoader,
    model_id: String,
}

impl FallbackTranscriber {
    pub fn new(primary: Arc<dyn Transcriber>, model_id: String, loader: LocalLoader) -> Self {
        Self {
            primary,
            local: Mutex::new(None),
            loader,
            model_id,
        }
    }

    fn local_engine(&self) -> Option<Arc<dyn Transcriber>> {
        let mut slot = self.local.lock();
        match &*slot {
            Some(LocalSlot::Ready(t)) => return Some(Arc::clone(t)),
            Some(LocalSlot::Failed) => return None,
            None => {}
        }
        tracing::info!(model = %self.model_id, "stt_fallback_engaged, loading local model");
        match (self.loader)() {
            Ok(t) => {
                *slot = Some(LocalSlot::Ready(Arc::clone(&t)));
                // One-shot GUI notice (the engine stays loaded for the
                // rest of the daemon run): degradation must be visible.
                crate::always::event::global_broadcaster()
                    .stt_fallback_engaged(self.model_id.clone());
                Some(t)
            }
            Err(e) => {
                tracing::error!(model = %self.model_id, error = %e, "stt_fallback_load_failed");
                *slot = Some(LocalSlot::Failed);
                None
            }
        }
    }
}

/// Drop an echoed vocabulary bias prompt from a transcription result.
///
/// Applied here rather than at each call site because every transcript
/// the daemon produces — final, speculative, chunked, and the consume
/// mode preview — flows through this one wrapper, and the prompt echo
/// showed up in all of them. See [`crate::glossary::strip_bias_prompt_echo`].
fn strip_prompt_echo(mut result: TranscriptionResult) -> TranscriptionResult {
    let cleaned = crate::glossary::strip_bias_prompt_echo(&result.text);
    if cleaned != result.text {
        tracing::info!(
            before_chars = result.text.chars().count(),
            after_chars = cleaned.chars().count(),
            "stripped_echoed_bias_prompt"
        );
        result.text = cleaned;
    }
    result
}

impl Transcriber for FallbackTranscriber {
    fn transcribe_from_bytes(&self, audio: Vec<u8>) -> Result<TranscriptionResult, SttError> {
        // The primary consumes the buffer, so keep a copy in case the
        // breaker is open — ~1 MB per utterance, cheap next to the call.
        let copy = audio.clone();
        let primary_err = match self.primary.transcribe_from_bytes(audio) {
            Ok(r) => return Ok(strip_prompt_echo(r)),
            Err(e) if e.should_fall_back() => e,
            Err(e) => return Err(e),
        };
        let Some(local) = self.local_engine() else {
            return Err(primary_err);
        };
        match local.transcribe_from_bytes(copy) {
            Ok(r) => {
                tracing::info!(model = %self.model_id, "stt_fallback_used");
                Ok(strip_prompt_echo(r))
            }
            Err(local_err) => {
                tracing::warn!(
                    model = %self.model_id,
                    error = %local_err,
                    "stt_fallback_failed, surfacing primary error"
                );
                Err(primary_err)
            }
        }
    }

    fn transcribe_streaming(
        &self,
        audio: Vec<u8>,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamingTranscriptionResult, SttError>> + Send>> {
        // For fallback, delegate to the primary's streaming implementation.
        // If the primary fails with a fallback-worthy error, we could try
        // the local engine, but for simplicity we just forward the primary's
        // stream and let the caller handle errors.
        Arc::clone(&self.primary).transcribe_streaming(audio)
    }
}

/// User's transcription backend pick. Serialises to a single TEXT
/// column in the prefs DB (`groq` or `local:<model_id>`). The
/// FromStr/Display impls own that wire format so the DB layer and the
/// UDS server stay in sync.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TranscriberBackendChoice {
    /// Remote Groq Whisper API. Requires `GROQ_API_KEY`.
    #[default]
    Groq,
    /// Local model identified by the registry id (e.g.
    /// `parakeet-tdt-0.6b-v3`). Must be downloaded before it can be
    /// activated — [`build_transcriber`] falls back to Groq when the
    /// model file is missing on disk.
    Local { model_id: String },
}

impl std::fmt::Display for TranscriberBackendChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Groq => f.write_str("groq"),
            Self::Local { model_id } => write!(f, "local:{model_id}"),
        }
    }
}

impl FromStr for TranscriberBackendChoice {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s == "groq" {
            return Ok(Self::Groq);
        }
        if let Some(rest) = s.strip_prefix("local:") {
            if rest.is_empty() {
                anyhow::bail!("local backend missing model id");
            }
            return Ok(Self::Local {
                model_id: rest.to_string(),
            });
        }
        anyhow::bail!("invalid transcriber backend: {s} (expected 'groq' or 'local:<id>')");
    }
}

/// Construct the [`Transcriber`] the daemon should use for the next
/// utterance, based on `cfg.transcriber_backend` and what's actually
/// downloaded.
///
/// Fallback policy:
///
/// * Local backend, model on disk → `LocalTranscriber`.
/// * Local backend, model missing or `local-stt` feature off →
///   Groq (with a warning). The alternative — refusing to start —
///   leaves users stranded if they delete a downloaded model.
/// * Groq backend → [`GroqTranscriber`] using the config's API key,
///   wrapped in a [`FallbackTranscriber`] when a verified local model
///   is on disk so an open circuit breaker degrades to offline
///   transcription instead of dropping utterances.
pub fn build_transcriber(
    cfg: &AlwaysConfig,
    registry: &ModelRegistry,
) -> Result<Arc<dyn Transcriber>> {
    match &cfg.transcriber_backend {
        TranscriberBackendChoice::Groq => {
            Ok(wrap_with_local_fallback(build_groq(cfg)?, cfg, registry))
        }
        TranscriberBackendChoice::Local { model_id } => {
            let info = registry
                .get(model_id)
                .with_context(|| format!("unknown local model id: {model_id}"))?;
            if !info.is_downloaded {
                tracing::warn!(
                    model = %model_id,
                    "local model not downloaded, falling back to remote Groq"
                );
                return Ok(wrap_with_local_fallback(build_groq(cfg)?, cfg, registry));
            }
            build_local(cfg, registry, &info)
        }
    }
}

/// Wrap a Groq primary in a [`FallbackTranscriber`] armed with the best
/// downloaded local model. Identity when nothing usable is on disk (the
/// outage then surfaces as an error, as before) or when `local-stt`
/// isn't compiled in.
#[cfg(feature = "local-stt")]
fn wrap_with_local_fallback(
    primary: Arc<dyn Transcriber>,
    cfg: &AlwaysConfig,
    registry: &ModelRegistry,
) -> Arc<dyn Transcriber> {
    use crate::always::stt_local::LocalTranscriber;

    let Some(info) = pick_fallback_model(registry) else {
        tracing::info!("stt_fallback_unarmed: no verified local model downloaded");
        return primary;
    };
    let Some(path) = registry.model_path(&info.id) else {
        return primary;
    };
    let lang = if cfg.lang == "auto" || cfg.lang.is_empty() {
        None
    } else {
        Some(cfg.lang.clone())
    };
    let engine = info.engine_type;
    let model_id = info.id.clone();
    let supports_streaming = info.supports_streaming;
    tracing::info!(model = %model_id, "stt_fallback_armed");
    let loader: LocalLoader = Box::new(move || {
        let t = LocalTranscriber::load(engine, &path, lang.clone(), supports_streaming)
            .with_context(|| format!("loading fallback local engine {engine:?}"))?;
        Ok(Arc::new(t) as Arc<dyn Transcriber>)
    });
    Arc::new(FallbackTranscriber::new(primary, model_id, loader))
}

#[cfg(not(feature = "local-stt"))]
fn wrap_with_local_fallback(
    primary: Arc<dyn Transcriber>,
    _cfg: &AlwaysConfig,
    _registry: &ModelRegistry,
) -> Arc<dyn Transcriber> {
    primary
}

/// Best downloaded model to degrade to: recommended catalog entries
/// first, then the highest combined speed+accuracy score. `is_downloaded`
/// already encodes integrity (SHA256 / `.verified` marker), so partial
/// or unverified downloads never qualify.
#[cfg(feature = "local-stt")]
fn pick_fallback_model(
    registry: &ModelRegistry,
) -> Option<crate::managers::model_registry::ModelInfo> {
    registry
        .list()
        .into_iter()
        .filter(|m| m.is_downloaded && !m.is_downloading)
        .max_by(|a, b| {
            (a.is_recommended, a.speed_score + a.accuracy_score)
                .partial_cmp(&(b.is_recommended, b.speed_score + b.accuracy_score))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn build_groq(cfg: &AlwaysConfig) -> Result<Arc<dyn Transcriber>> {
    let key = cfg
        .groq_stt_api_key
        .as_deref()
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Groq backend selected but GROQ_API_KEY is not set. \
                 Configure it via `always config set groq_api_key <key>` or \
                 select a downloaded local model in Settings → Models."
            )
        })?;
    let t = Arc::new(GroqTranscriber::new(key));
    tracing::info!(
        backend = "groq",
        model = crate::stt::GROQ_MODEL_NAME,
        "transcriber_ready"
    );
    Ok(t)
}

#[cfg(feature = "local-stt")]
fn build_local(
    cfg: &AlwaysConfig,
    registry: &ModelRegistry,
    info: &crate::managers::model_registry::ModelInfo,
) -> Result<Arc<dyn Transcriber>> {
    use crate::always::stt_local::LocalTranscriber;

    let path = registry
        .model_path(&info.id)
        .ok_or_else(|| anyhow::anyhow!("registry missing path for {}", info.id))?;
    // `auto` is the configured-language sentinel for "let the engine
    // decide". Whisper / Canary / SenseVoice all accept None for this;
    // monolingual engines (Parakeet V2 / Moonshine / GigaAM) ignore it.
    let lang = if cfg.lang == "auto" || cfg.lang.is_empty() {
        None
    } else {
        Some(cfg.lang.clone())
    };
    let t = LocalTranscriber::load(info.engine_type, &path, lang, info.supports_streaming)
        .with_context(|| format!("loading local engine for model {}", info.id))?;
    tracing::info!(
        backend = "local",
        model = %info.id,
        engine = ?info.engine_type,
        "transcriber_ready"
    );
    Ok(Arc::new(t))
}

#[cfg(not(feature = "local-stt"))]
fn build_local(
    cfg: &AlwaysConfig,
    _registry: &ModelRegistry,
    info: &crate::managers::model_registry::ModelInfo,
) -> Result<Arc<dyn Transcriber>> {
    tracing::error!(
        model = %info.id,
        "local-stt feature not compiled in; falling back to Groq"
    );
    build_groq(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_groq() {
        assert_eq!(
            "groq".parse::<TranscriberBackendChoice>().unwrap(),
            TranscriberBackendChoice::Groq
        );
    }

    #[test]
    fn parse_local() {
        let parsed: TranscriberBackendChoice = "local:parakeet-tdt-0.6b-v3".parse().unwrap();
        assert!(matches!(
            parsed,
            TranscriberBackendChoice::Local { ref model_id } if model_id == "parakeet-tdt-0.6b-v3"
        ));
    }

    #[test]
    fn display_round_trips() {
        let g = TranscriberBackendChoice::Groq;
        assert_eq!(g.to_string(), "groq");
        let l = TranscriberBackendChoice::Local {
            model_id: "small".into(),
        };
        assert_eq!(l.to_string(), "local:small");
        assert_eq!(
            l.to_string().parse::<TranscriberBackendChoice>().unwrap(),
            l
        );
    }

    #[test]
    fn rejects_bad_inputs() {
        assert!("".parse::<TranscriberBackendChoice>().is_err());
        assert!("local:".parse::<TranscriberBackendChoice>().is_err());
        assert!("openai".parse::<TranscriberBackendChoice>().is_err());
    }

    // -- FallbackTranscriber ------------------------------------------------

    use std::sync::atomic::{AtomicU32, Ordering};

    fn ok_result(text: &str) -> TranscriptionResult {
        TranscriptionResult {
            text: text.to_string(),
            ..Default::default()
        }
    }

    /// Scripted primary: errors with the given constructor per call, or
    /// succeeds when `error` is `None`.
    struct ScriptedPrimary {
        error: Option<fn() -> SttError>,
        calls: AtomicU32,
    }

    impl Transcriber for ScriptedPrimary {
        fn transcribe_from_bytes(&self, _audio: Vec<u8>) -> Result<TranscriptionResult, SttError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.error {
                Some(make) => Err(make()),
                None => Ok(ok_result("remote text")),
            }
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

    struct StaticLocal(&'static str);

    impl Transcriber for StaticLocal {
        fn transcribe_from_bytes(&self, _audio: Vec<u8>) -> Result<TranscriptionResult, SttError> {
            Ok(ok_result(self.0))
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

    fn breaker_open() -> SttError {
        SttError::Unavailable { remaining_ms: 1000 }
    }

    fn counting_loader(
        counter: Arc<AtomicU32>,
        result: fn() -> Result<Arc<dyn Transcriber>>,
    ) -> LocalLoader {
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            result()
        })
    }

    #[test]
    fn fallback_used_when_breaker_open_and_engine_cached() {
        let loads = Arc::new(AtomicU32::new(0));
        let t = FallbackTranscriber::new(
            Arc::new(ScriptedPrimary {
                error: Some(breaker_open),
                calls: AtomicU32::new(0),
            }),
            "test-model".into(),
            counting_loader(Arc::clone(&loads), || {
                Ok(Arc::new(StaticLocal("local text")) as Arc<dyn Transcriber>)
            }),
        );

        let first = t.transcribe_from_bytes(vec![1, 2, 3]).unwrap();
        assert_eq!(first.text, "local text");
        let second = t.transcribe_from_bytes(vec![4, 5, 6]).unwrap();
        assert_eq!(second.text, "local text");
        // The engine loaded once, then was reused.
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn primary_success_never_touches_local() {
        let loads = Arc::new(AtomicU32::new(0));
        let t = FallbackTranscriber::new(
            Arc::new(ScriptedPrimary {
                error: None,
                calls: AtomicU32::new(0),
            }),
            "test-model".into(),
            counting_loader(Arc::clone(&loads), || {
                Ok(Arc::new(StaticLocal("local text")) as Arc<dyn Transcriber>)
            }),
        );

        let r = t.transcribe_from_bytes(vec![1]).unwrap();
        assert_eq!(r.text, "remote text");
        assert_eq!(loads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn non_breaker_errors_bubble_without_fallback() {
        let loads = Arc::new(AtomicU32::new(0));
        let t = FallbackTranscriber::new(
            Arc::new(ScriptedPrimary {
                error: Some(|| SttError::RateLimited { attempts: 3 }),
                calls: AtomicU32::new(0),
            }),
            "test-model".into(),
            counting_loader(Arc::clone(&loads), || {
                Ok(Arc::new(StaticLocal("local text")) as Arc<dyn Transcriber>)
            }),
        );

        let err = t.transcribe_from_bytes(vec![1]).unwrap_err();
        assert!(matches!(err, SttError::RateLimited { .. }));
        assert_eq!(loads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn load_failure_surfaces_primary_error_and_never_retries() {
        let loads = Arc::new(AtomicU32::new(0));
        let t = FallbackTranscriber::new(
            Arc::new(ScriptedPrimary {
                error: Some(breaker_open),
                calls: AtomicU32::new(0),
            }),
            "test-model".into(),
            counting_loader(Arc::clone(&loads), || anyhow::bail!("model file corrupt")),
        );

        let err = t.transcribe_from_bytes(vec![1]).unwrap_err();
        assert!(matches!(err, SttError::Unavailable { .. }));
        let err2 = t.transcribe_from_bytes(vec![2]).unwrap_err();
        assert!(matches!(err2, SttError::Unavailable { .. }));
        // Failed load is sticky — no per-utterance retry stall.
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }
}
