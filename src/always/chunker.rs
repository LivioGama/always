//! Rolling chunk transcription for long dictations.
//!
//! The VAD loop used to buffer an entire utterance and send it to Groq as
//! ONE request when the user finally stopped talking — with a 5-minute
//! hard cap that discarded everything past it, and a single point of
//! failure where one network error lost the whole monologue.
//!
//! Under chunking, the tentative-silence kickoff point in `vad.rs` flushes
//! the accumulated buffer as a **committed** chunk once it crosses the
//! target length: a background thread transcribes it (and pre-corrects it
//! with the grammar LLM) while the loop keeps recording into a fresh
//! buffer. At finalize, chunk texts are joined in flush order with the
//! final utterance tail.
//!
//! Failure policy: **speech is never lost.** A chunk's audio is retained
//! in memory until its transcription succeeds; a chunk that fails all
//! retries gets one synchronous retry at finalize, and if that also fails
//! its WAV is written to `~/.always/failed-chunks/` with a placeholder in
//! the joined text.

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use parking_lot::Mutex;

use crate::always::{audio, event};
use crate::stt::{Transcriber, TranscriptionResult};

/// Poll cadence while waiting on an in-flight chunk at finalize.
const FINALIZE_POLL_MS: u64 = 10;
/// Heartbeat cadence for the GUI's transcribing lease during finalize.
const HEARTBEAT_SECS: u64 = 2;

#[cfg(test)]
static TEST_PAUSE_AFTER_RAW_PUBLISH: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>> =
    std::sync::Mutex::new(None);

/// One committed chunk. `audio` is retained until the transcription
/// succeeds so a failed chunk can be retried / spilled, never dropped.
struct ChunkSlot {
    index: usize,
    /// `None` while the background transcription is in flight.
    result: Mutex<Option<Result<ChunkText, String>>>,
    audio: Mutex<Option<Vec<i16>>>,
}

/// Raw Whisper text plus the grammar-corrected variant when the per-chunk
/// warm succeeded. `finalize` prefers `corrected` — for long transcripts
/// the event loop skips its own blocking grammar pass (see
/// `GRAMMAR_MAX_CHARS`), so per-chunk correction is the only one applied.
struct ChunkText {
    raw: String,
    corrected: Option<String>,
    /// The per-chunk grammar attempt finished (success or failure), so
    /// this chunk's contribution to the final join can no longer change.
    /// Drives `settled_join` — the joined-text grammar warm must only
    /// fire on a join that `finalize` is guaranteed to reproduce.
    grammar_done: bool,
}

pub struct ChunkedTranscript {
    /// Committed chunks joined in flush order (single spaces). Empty when
    /// every chunk failed.
    pub text: String,
    pub chunk_count: usize,
    pub failed_chunks: usize,
}

#[derive(Default)]
pub struct ChunkAccumulator {
    /// Shared with the per-chunk transcription threads so a finished
    /// chunk can inspect ALL committed chunks and warm the joined-text
    /// grammar key (see `settled_join`).
    slots: Arc<Mutex<Vec<Arc<ChunkSlot>>>>,
}

/// Cheap read-only view of the committed chunks, safe to move into the
/// tail-speculation thread in `vad.rs` so its grammar warm can target
/// the prospective joined transcript instead of the tail alone.
#[derive(Clone)]
pub struct ChunkJoinHandle {
    slots: Arc<Mutex<Vec<Arc<ChunkSlot>>>>,
}

impl ChunkJoinHandle {
    pub fn chunk_count(&self) -> usize {
        self.slots.lock().len()
    }

    /// See [`settled_join`].
    pub fn settled_join(&self) -> Option<String> {
        let slots = self.slots.lock().clone();
        settled_join(&slots)
    }
}

/// The exact chunk join `finalize` will assemble, or `None` while it is
/// still undetermined. `Some` only when every committed chunk has a
/// stored successful result AND its grammar attempt has finished —
/// `finalize` then picks `corrected.unwrap_or(raw)` per chunk, which is
/// reproduced byte-for-byte here (same choice, same skip-empty, same
/// single-space join). A failed or in-flight chunk returns `None`: its
/// final text depends on the retry at finalize. Non-consuming.
fn settled_join(slots: &[Arc<ChunkSlot>]) -> Option<String> {
    let mut parts: Vec<String> = Vec::with_capacity(slots.len());
    for slot in slots {
        let guard = slot.result.lock();
        match guard.as_ref() {
            Some(Ok(text)) if text.grammar_done => {
                let chosen = text.corrected.clone().unwrap_or_else(|| text.raw.clone());
                if !chosen.is_empty() {
                    parts.push(chosen);
                }
            }
            _ => return None,
        }
    }
    Some(parts.join(" "))
}

impl ChunkAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.lock().is_empty()
    }

    pub fn chunk_count(&self) -> usize {
        self.slots.lock().len()
    }

    pub fn join_handle(&self) -> ChunkJoinHandle {
        ChunkJoinHandle {
            slots: Arc::clone(&self.slots),
        }
    }

    /// Commit `audio_chunk` for background transcription. Never discarded
    /// (unlike the speculative slot): the loop keeps recording while this
    /// runs. `grammar` is the post-processor for the per-chunk correction
    /// warm; pass `None` when grammar correction is disabled.
    pub fn flush(
        &mut self,
        audio_chunk: Vec<i16>,
        transcriber: &Arc<dyn Transcriber>,
        grammar: Option<Arc<crate::always::postprocess::PostProcessor>>,
        rt: &tokio::runtime::Handle,
    ) {
        let index = self.slots.lock().len();
        let secs = audio_chunk.len() as f64 / 16_000.0;
        let slot = Arc::new(ChunkSlot {
            index,
            result: Mutex::new(None),
            audio: Mutex::new(Some(audio_chunk.clone())),
        });
        self.slots.lock().push(Arc::clone(&slot));
        tracing::info!(chunk = index, secs, "chunk_flush");

        let transcriber = Arc::clone(transcriber);
        let rt = rt.clone();
        let all_slots = Arc::clone(&self.slots);
        std::thread::spawn(move || {
            // Same catch_unwind rationale as the speculation thread in
            // `vad.rs`: a panic must surface as a stored failure, not a
            // silently missing slot that stalls finalize.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || -> Result<TranscriptionResult> {
                    let wav = audio::create_wav_bytes_i16_mono_16k(&audio_chunk)?;
                    transcriber
                        .transcribe_from_bytes(wav)
                        .map_err(anyhow::Error::from)
                },
            ))
            .unwrap_or_else(|panic_payload| {
                let msg = panic_payload
                    .downcast_ref::<&'static str>()
                    .copied()
                    .or_else(|| panic_payload.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("chunk transcription panicked");
                Err(anyhow::anyhow!("chunk transcription panicked: {msg}"))
            });

            // An empty result is NOT a success, and it must never be
            // treated as one: the audio is dropped a few lines below and
            // there is no other copy, so an engine that returns "" silently
            // DESTROYS the user's utterance. Measured on real dictation:
            // ~13% of chunks (Parakeet) and ~17% (Nemotron) came back empty,
            // including chunks whose speech the speaker gate had positively
            // verified — i.e. the user demonstrably spoke and the words were
            // thrown away with no error, no overlay, and nothing pasted.
            //
            // Retry once before believing it. A genuinely silent chunk costs
            // one extra decode (sub-second locally) and still resolves to
            // empty; a transient engine failure gets its words back.
            let outcome = match outcome {
                Ok(ref result) if result.text.trim().is_empty() => {
                    tracing::warn!(chunk = index, "chunk_empty_retrying");
                    let retry = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                        || -> Result<TranscriptionResult> {
                            let wav = audio::create_wav_bytes_i16_mono_16k(&audio_chunk)?;
                            transcriber
                                .transcribe_from_bytes(wav)
                                .map_err(anyhow::Error::from)
                        },
                    ));
                    match retry {
                        Ok(Ok(r)) if !r.text.trim().is_empty() => {
                            tracing::info!(
                                chunk = index,
                                chars = r.text.trim().chars().count(),
                                "chunk_empty_retry_recovered"
                            );
                            Ok(r)
                        }
                        // Still empty, or the retry itself failed: keep the
                        // ORIGINAL outcome so behaviour is unchanged from
                        // before this retry existed.
                        _ => {
                            tracing::warn!(chunk = index, "chunk_empty_after_retry");
                            outcome
                        }
                    }
                }
                other => other,
            };

            match outcome {
                Ok(result) => {
                    let raw = result.text.trim().to_string();
                    if !raw.is_empty() {
                        // Rolling preview on the overlay while the user
                        // keeps talking.
                        event::global_broadcaster().transcript_chunk(raw.clone());
                    }
                    // Publish raw text immediately. Per-chunk grammar can
                    // take longer than the user keeps speaking; waiting for
                    // it before setting `result` made finalize time out and
                    // paste `[audio saved: chunk N]` even though Whisper had
                    // already returned usable text.
                    let will_correct = grammar.is_some() && !raw.is_empty();
                    *slot.result.lock() = Some(Ok(ChunkText {
                        raw: raw.clone(),
                        corrected: None,
                        grammar_done: !will_correct,
                    }));
                    // Success: the audio has served its purpose. Drop it only
                    // after a usable raw result is visible to finalize.
                    *slot.audio.lock() = None;
                    #[cfg(test)]
                    if let Some(rx) = TEST_PAUSE_AFTER_RAW_PUBLISH.lock().unwrap().take() {
                        let _ = rx.recv_timeout(Duration::from_secs(2));
                    }
                    // Per-chunk grammar correction, paid for during the
                    // recording instead of at finalize. Best-effort: any
                    // failure falls back to the raw Whisper text.
                    let grammar_for_warm = grammar.clone();
                    let corrected = grammar.and_then(|pp| {
                        if raw.is_empty() {
                            return None;
                        }
                        let req = crate::always::correction_request::build(&raw, pp.can_correct());
                        rt.block_on(pp.process_request(&req))
                            .ok()
                            .map(|(text, _cache_hit)| text)
                    });
                    tracing::info!(
                        chunk = index,
                        chars = raw.chars().count(),
                        corrected = corrected.is_some(),
                        "chunk_transcribed"
                    );
                    {
                        let mut result = slot.result.lock();
                        if let Some(Ok(text)) = result.as_mut() {
                            text.corrected = corrected;
                            text.grammar_done = true;
                        }
                    }
                    // Joined-transcript grammar warm. The paste path's
                    // blocking grammar call is keyed on the JOIN of the
                    // corrected chunks (+ tail), not on any chunk's raw
                    // text — a key no per-chunk correction ever touched,
                    // which is why chunked dictations (the vast majority,
                    // measured 469/518 pastes) always paid a cold
                    // ~600-1400ms LLM call at paste. As soon as this
                    // chunk settles the join, start that call in the
                    // background so the paste-path request lands as a
                    // cache hit / joins the in-flight single-flight cell.
                    // If more chunks follow, the superseded warm is a
                    // wasted-but-cached call, bounded per chunk and by
                    // GRAMMAR_MAX_CHARS (above it the paste path skips
                    // blocking grammar entirely).
                    let slots_snapshot = all_slots.lock().clone();
                    if let Some(pp) = grammar_for_warm
                        && let Some(join) = settled_join(&slots_snapshot)
                        && !crate::always::event_loop::is_short_utterance(&join)
                        && join.chars().count() <= crate::always::event_loop::GRAMMAR_MAX_CHARS
                    {
                        let req = crate::always::correction_request::build(&join, pp.can_correct());
                        rt.spawn(async move {
                            let _ = pp.process_request(&req).await;
                        });
                    }
                }
                Err(err) => {
                    tracing::error!(chunk = index, error = %err, "chunk_transcription_failed");
                    *slot.result.lock() = Some(Err(format!("{err:#}")));
                }
            }
        });
    }

    /// Wait for every committed chunk (oldest first) and join their texts
    /// in flush order. A chunk that failed gets one synchronous retry;
    /// if that fails too its WAV is spilled to `~/.always/failed-chunks/`
    /// and an `[audio saved: chunk N]` placeholder keeps its position.
    ///
    /// Emits the GUI transcribing heartbeat while waiting so the overlay
    /// lease can't expire during a long straggler.
    pub fn finalize(&self, transcriber: &Arc<dyn Transcriber>) -> ChunkedTranscript {
        let slots: Vec<Arc<ChunkSlot>> = self.slots.lock().clone();
        let mut parts: Vec<String> = Vec::with_capacity(slots.len());
        let mut failed = 0usize;
        for slot in &slots {
            // Each chunk has had at least a full chunk-duration of head
            // start; scale the residual wait like the speculation wait in
            // `vad.rs` does.
            let audio_secs = slot
                .audio
                .lock()
                .as_ref()
                .map(|a| a.len() as f64 / 16_000.0)
                // Audio already dropped == transcription succeeded; the
                // result is (or is about to be) stored. Tiny wait budget.
                .unwrap_or(5.0);
            let max_wait = Duration::from_secs_f64((audio_secs * 0.5).clamp(10.0, 60.0));
            let started = Instant::now();
            let mut last_heartbeat = Instant::now();
            let outcome = loop {
                if let Some(r) = slot.result.lock().take() {
                    break Some(r);
                }
                if started.elapsed() >= max_wait {
                    break None;
                }
                if last_heartbeat.elapsed() >= Duration::from_secs(HEARTBEAT_SECS) {
                    last_heartbeat = Instant::now();
                    event::global_broadcaster().transcribing_started();
                }
                std::thread::sleep(Duration::from_millis(FINALIZE_POLL_MS));
            };

            match outcome {
                Some(Ok(text)) => {
                    let chosen = text.corrected.unwrap_or(text.raw);
                    if !chosen.is_empty() {
                        parts.push(chosen);
                    }
                }
                Some(Err(err)) => {
                    tracing::warn!(chunk = slot.index, error = %err, "chunk_retry_sync");
                    match self.retry_sync(slot, transcriber) {
                        Some(text) => parts.push(text),
                        None => {
                            failed += 1;
                            parts.push(self.spill_placeholder(slot));
                        }
                    }
                }
                None => {
                    // Still in flight past the budget (wedged network,
                    // open circuit breaker). Do NOT retry concurrently —
                    // spill so the audio is safe, and let the straggler
                    // finish into the abandoned slot harmlessly.
                    tracing::warn!(chunk = slot.index, "chunk_wait_timeout");
                    failed += 1;
                    parts.push(self.spill_placeholder(slot));
                }
            }
        }

        ChunkedTranscript {
            text: parts.join(" "),
            chunk_count: slots.len(),
            failed_chunks: failed,
        }
    }

    fn retry_sync(&self, slot: &ChunkSlot, transcriber: &Arc<dyn Transcriber>) -> Option<String> {
        let audio_copy = slot.audio.lock().clone()?;
        let wav = audio::create_wav_bytes_i16_mono_16k(&audio_copy).ok()?;
        match transcriber.transcribe_from_bytes(wav) {
            Ok(result) => {
                *slot.audio.lock() = None;
                let raw = result.text.trim().to_string();
                tracing::info!(chunk = slot.index, "chunk_retry_succeeded");
                (!raw.is_empty()).then_some(raw)
            }
            Err(err) => {
                tracing::error!(chunk = slot.index, error = %err, "chunk_retry_failed");
                None
            }
        }
    }

    /// Write the chunk's WAV to the failed-chunks dir and return the
    /// placeholder that keeps its position in the joined transcript.
    fn spill_placeholder(&self, slot: &ChunkSlot) -> String {
        let placeholder = format!("[audio saved: chunk {}]", slot.index + 1);
        let Some(audio_samples) = slot.audio.lock().clone() else {
            // Transcription succeeded but the result was consumed by a
            // racing path — nothing to spill.
            return placeholder;
        };
        let dir = failed_chunks_dir();
        if let Err(err) = create_private_dir(&dir) {
            tracing::error!(error = %err, "failed_chunks_dir_create_failed");
            return placeholder;
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = dir.join(format!("chunk-{stamp}-{}.wav", slot.index + 1));
        match audio::create_wav_bytes_i16_mono_16k(&audio_samples)
            .and_then(|wav| write_private_file(&path, &wav))
        {
            Ok(()) => {
                tracing::warn!(chunk = slot.index, path = %path.display(), "chunk_audio_spilled");
            }
            Err(err) => {
                tracing::error!(chunk = slot.index, error = %err, "chunk_audio_spill_failed");
            }
        }
        placeholder
    }
}

fn create_private_dir(dir: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn write_private_file(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, bytes)?;
    Ok(())
}

fn failed_chunks_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(home)
        .join(".always")
        .join("failed-chunks")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::{StreamingTranscriptionResult, SttError};
    use futures::Stream;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Deterministic transcriber: returns canned texts round-robin, or
    /// fails for indices in `fail_calls`.
    struct MockTranscriber {
        calls: AtomicUsize,
        texts: Vec<&'static str>,
        fail_calls: Vec<usize>,
    }

    impl Transcriber for MockTranscriber {
        fn transcribe_from_bytes(&self, _audio: Vec<u8>) -> Result<TranscriptionResult, SttError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_calls.contains(&call) {
                return Err(SttError::ClientError {
                    status: 500,
                    body: "mock failure".into(),
                });
            }
            let text = self.texts.get(call % self.texts.len()).unwrap_or(&"");
            Ok(TranscriptionResult {
                text: (*text).to_string(),
                ..Default::default()
            })
        }

        fn transcribe_streaming(
            &self,
            _audio: Vec<u8>,
        ) -> Pin<Box<dyn Stream<Item = Result<StreamingTranscriptionResult, SttError>> + Send>>
        {
            let result = match self.transcribe_from_bytes(_audio) {
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

    fn mock(texts: Vec<&'static str>, fail_calls: Vec<usize>) -> Arc<dyn Transcriber> {
        Arc::new(MockTranscriber {
            calls: AtomicUsize::new(0),
            texts,
            fail_calls,
        })
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn failed_audio_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir =
            std::env::temp_dir().join(format!("always-chunker-private-{}", std::process::id()));
        let path = dir.join("failed.wav");
        create_private_dir(&dir).unwrap();
        write_private_file(&path, b"private audio").unwrap();

        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn chunks_join_in_flush_order() {
        let rt = rt();
        let t = mock(vec!["first part", "second part"], vec![]);
        let mut acc = ChunkAccumulator::new();
        acc.flush(vec![0i16; 16_000], &t, None, rt.handle());
        // Give the first thread a head start so call order matches index
        // order (the mock is call-ordered, not content-aware).
        std::thread::sleep(Duration::from_millis(50));
        acc.flush(vec![0i16; 16_000], &t, None, rt.handle());
        let joined = acc.finalize(&t);
        assert_eq!(joined.text, "first part second part");
        assert_eq!(joined.chunk_count, 2);
        assert_eq!(joined.failed_chunks, 0);
    }

    #[test]
    fn failed_chunk_retries_synchronously() {
        let rt = rt();
        // Call 0 (background) fails; call 1 (sync retry) succeeds.
        let t = mock(vec!["recovered text", "recovered text"], vec![0]);
        let mut acc = ChunkAccumulator::new();
        acc.flush(vec![0i16; 16_000], &t, None, rt.handle());
        let joined = acc.finalize(&t);
        assert_eq!(joined.text, "recovered text");
        assert_eq!(joined.failed_chunks, 0);
    }

    #[test]
    fn raw_chunk_is_available_before_postprocess_finishes() {
        let rt = rt();
        let t = mock(vec!["raw text ready"], vec![]);
        let mut acc = ChunkAccumulator::new();
        let (tx, rx) = std::sync::mpsc::channel();
        *TEST_PAUSE_AFTER_RAW_PUBLISH.lock().unwrap() = Some(rx);

        acc.flush(vec![0i16; 16_000], &t, None, rt.handle());

        let started = Instant::now();
        let joined = acc.finalize(&t);
        let _ = tx.send(());

        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(joined.text, "raw text ready");
        assert_eq!(joined.failed_chunks, 0);
    }

    #[test]
    fn doubly_failed_chunk_spills_placeholder() {
        let rt = rt();
        // Background AND sync retry fail → placeholder + spilled WAV.
        let t = mock(vec!["unused"], vec![0, 1]);
        let mut acc = ChunkAccumulator::new();
        acc.flush(vec![0i16; 16_000], &t, None, rt.handle());
        let joined = acc.finalize(&t);
        assert_eq!(joined.text, "[audio saved: chunk 1]");
        assert_eq!(joined.failed_chunks, 1);
    }

    #[test]
    fn empty_accumulator_is_empty() {
        let acc = ChunkAccumulator::new();
        assert!(acc.is_empty());
        assert_eq!(acc.chunk_count(), 0);
    }

    fn slot_with(result: Option<Result<ChunkText, String>>) -> Arc<ChunkSlot> {
        Arc::new(ChunkSlot {
            index: 0,
            result: Mutex::new(result),
            audio: Mutex::new(None),
        })
    }

    #[test]
    fn settled_join_prefers_corrected_and_matches_finalize_choice() {
        let slots = vec![
            slot_with(Some(Ok(ChunkText {
                raw: "furst part".into(),
                corrected: Some("first part".into()),
                grammar_done: true,
            }))),
            slot_with(Some(Ok(ChunkText {
                raw: "second part".into(),
                corrected: None,
                grammar_done: true,
            }))),
        ];
        assert_eq!(
            settled_join(&slots).as_deref(),
            Some("first part second part")
        );
    }

    #[test]
    fn settled_join_is_none_while_grammar_pending_or_chunk_failed() {
        let pending = vec![slot_with(Some(Ok(ChunkText {
            raw: "text".into(),
            corrected: None,
            grammar_done: false,
        })))];
        assert!(settled_join(&pending).is_none());

        let in_flight = vec![slot_with(None)];
        assert!(settled_join(&in_flight).is_none());

        let failed = vec![slot_with(Some(Err("boom".into())))];
        assert!(settled_join(&failed).is_none());
    }

    #[test]
    fn no_grammar_flush_settles_immediately() {
        let rt = rt();
        let t = mock(vec!["hello there"], vec![]);
        let mut acc = ChunkAccumulator::new();
        acc.flush(vec![0i16; 16_000], &t, None, rt.handle());
        let handle = acc.join_handle();
        // The background thread stores the raw result with grammar_done
        // (no post-processor was supplied); poll briefly for it.
        let started = Instant::now();
        let join = loop {
            if let Some(j) = handle.settled_join() {
                break j;
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "join never settled"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(join, "hello there");
        // And finalize assembles the identical string.
        assert_eq!(acc.finalize(&t).text, "hello there");
    }
}
