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
    slots: Vec<Arc<ChunkSlot>>,
}

impl ChunkAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn chunk_count(&self) -> usize {
        self.slots.len()
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
        let index = self.slots.len();
        let secs = audio_chunk.len() as f64 / 16_000.0;
        let slot = Arc::new(ChunkSlot {
            index,
            result: Mutex::new(None),
            audio: Mutex::new(Some(audio_chunk.clone())),
        });
        self.slots.push(Arc::clone(&slot));
        tracing::info!(chunk = index, secs, "chunk_flush");

        let transcriber = Arc::clone(transcriber);
        let rt = rt.clone();
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
                    *slot.result.lock() = Some(Ok(ChunkText {
                        raw: raw.clone(),
                        corrected: None,
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
                    if let Some(corrected) = corrected {
                        let mut result = slot.result.lock();
                        if let Some(Ok(text)) = result.as_mut() {
                            text.corrected = Some(corrected);
                        }
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
        let mut parts: Vec<String> = Vec::with_capacity(self.slots.len());
        let mut failed = 0usize;
        for slot in &self.slots {
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
            chunk_count: self.slots.len(),
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
        if let Err(err) = std::fs::create_dir_all(&dir) {
            tracing::error!(error = %err, "failed_chunks_dir_create_failed");
            return placeholder;
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = dir.join(format!("chunk-{stamp}-{}.wav", slot.index + 1));
        match audio::create_wav_bytes_i16_mono_16k(&audio_samples)
            .and_then(|wav| std::fs::write(&path, wav).map_err(Into::into))
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

fn failed_chunks_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(home)
        .join(".always")
        .join("failed-chunks")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::SttError;
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
}
