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
/// `corrected` stays `None` until that correction lands, so the text a
/// chunk contributes can change once: raw first, corrected later.
struct ChunkText {
    raw: String,
    corrected: Option<String>,
    /// Engine acoustic confidence of `raw` (see `RecognitionEvidence`).
    confidence: Option<f32>,
}

pub struct ChunkedTranscript {
    /// Committed chunks joined in flush order (single spaces). Empty when
    /// every chunk failed.
    pub text: String,
    pub chunk_count: usize,
    pub failed_chunks: usize,
    /// Per committed chunk `(acoustic confidence, words)`, for
    /// [`weighted_confidence`]. Chunks that recognized no words are left
    /// out; a retried or failed chunk is `(None, 0)`, which makes the
    /// utterance's confidence unknown — never a guess.
    pub confidence_parts: Vec<(Option<f32>, usize)>,
}

/// Word-weighted mean of `(confidence, words)` parts; `None` when any part
/// lacks a confidence or there are no words at all.
pub fn weighted_confidence(parts: &[(Option<f32>, usize)]) -> Option<f32> {
    let mut sum = 0.0f64;
    let mut words = 0usize;
    for (c, n) in parts {
        let c = (*c)?;
        sum += c as f64 * *n as f64;
        words += n;
    }
    (words > 0).then(|| (sum / words as f64) as f32)
}

#[derive(Default)]
pub struct ChunkAccumulator {
    /// Shared with the per-chunk transcription threads so a finished
    /// chunk can inspect ALL committed chunks and warm the joined-text
    /// grammar key (see `current_join`).
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

    /// See [`current_join`].
    pub fn current_join(&self) -> Option<String> {
        let slots = self.slots.lock().clone();
        current_join(&slots)
    }
}

/// The exact chunk join `finalize` would assemble if it ran now, or
/// `None` while that is undetermined. Per chunk, `finalize` takes
/// `corrected.unwrap_or(raw)` — raw while the per-chunk correction is
/// still in flight, corrected once it lands — and that choice is
/// reproduced byte-for-byte here (same skip-empty, same single-space
/// join). An in-flight or failed chunk returns `None`: its final text
/// depends on the retry at finalize. Non-consuming.
///
/// The grammar warms target this join at each point it can change (raw
/// published, correction landed), so whichever state the utterance ends
/// in, the paste-path grammar key has already been warmed.
fn current_join(slots: &[Arc<ChunkSlot>]) -> Option<String> {
    let mut parts: Vec<String> = Vec::with_capacity(slots.len());
    for slot in slots {
        let guard = slot.result.lock();
        match guard.as_ref() {
            Some(Ok(text)) => {
                let chosen = text.corrected.clone().unwrap_or_else(|| text.raw.clone());
                if !chosen.is_empty() {
                    parts.push(chosen);
                }
            }
            _ => return None,
        }
    }
    Some(join_chunk_texts(&parts))
}

/// Strip a trailing ellipsis ("..." or "…", plus any whitespace around it)
/// left by the recognizer when the audio ended mid-phrase. Chunks end at a
/// hard-flush cut (`CHUNK_HARD_MAX_SECS`), not at a pause the speaker made,
/// so Apple's batch recognizer renders the cut-off phrase as "word..." —
/// observed 2026-10-13: "For the... but the UX", "why I... told you", each
/// exactly at a chunk boundary. The ellipsis is ours, not the speaker's.
pub fn strip_cut_ellipsis(text: &str) -> &str {
    let mut t = text.trim_end();
    loop {
        if let Some(rest) = t.strip_suffix("...") {
            t = rest.trim_end();
        } else if let Some(rest) = t.strip_suffix('…') {
            t = rest.trim_end();
        } else {
            return t;
        }
    }
}

/// Join committed chunk texts in flush order. Every chunk but the last ends
/// at a cut, so its trailing ellipsis is dropped (see `strip_cut_ellipsis`).
/// The last one is left alone here — whether IT ends at a cut depends on
/// whether a tail follows, which `vad::finalize_chunked` decides.
pub fn join_chunk_texts(parts: &[String]) -> String {
    let n = parts.len();
    let mut out = String::new();
    for (i, part) in parts.iter().enumerate() {
        let piece = if i + 1 < n { strip_cut_ellipsis(part) } else { part.as_str() };
        if piece.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(piece);
    }
    out
}

/// Start the joined-transcript grammar call in the background so the
/// paste-path request for the same text lands as a cache hit or joins
/// the in-flight single-flight cell. Skipped when the join is not yet
/// determined or the paste path would skip grammar for it anyway.
fn warm_current_join(
    pp: &Arc<crate::always::postprocess::PostProcessor>,
    slots: &Arc<Mutex<Vec<Arc<ChunkSlot>>>>,
    rt: &tokio::runtime::Handle,
) {
    let snapshot = slots.lock().clone();
    if let Some(join) = current_join(&snapshot)
        && !crate::always::event_loop::is_short_utterance(&join)
        && join.chars().count() <= crate::always::event_loop::GRAMMAR_MAX_CHARS
    {
        let req = crate::always::correction_request::build(&join, pp.can_correct());
        let pp = Arc::clone(pp);
        rt.spawn(async move {
            let _ = pp.process_request(&req).await;
        });
    }
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
                        confidence: result.evidence.confidence,
                    }));
                    // Success: the audio has served its purpose. Drop it only
                    // after a usable raw result is visible to finalize.
                    *slot.audio.lock() = None;
                    // Warm the join as it stands NOW, with this chunk raw.
                    // If the user stops here, the final cut (silence
                    // window) usually comes before this chunk's own
                    // correction below returns, so finalize pastes exactly
                    // this join — and the grammar call for it is already
                    // running.
                    if let Some(pp) = &grammar {
                        warm_current_join(pp, &all_slots, &rt);
                    }
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
                    let corrected_landed = corrected.is_some();
                    {
                        let mut result = slot.result.lock();
                        if let Some(Ok(text)) = result.as_mut() {
                            text.corrected = corrected;
                        }
                    }
                    // Joined-transcript grammar warm, second point: this
                    // chunk's correction changed the join (raw → corrected),
                    // so warm the new join too — the paste path asks for
                    // whichever state the utterance ends in. The paste
                    // path's blocking call is keyed on the JOIN (+ tail),
                    // a key no per-chunk correction ever touches. If more
                    // chunks follow, a superseded warm is a wasted-but-
                    // cached call, bounded per chunk and by
                    // GRAMMAR_MAX_CHARS (above it the paste path skips
                    // blocking grammar entirely).
                    if corrected_landed && let Some(pp) = &grammar_for_warm {
                        warm_current_join(pp, &all_slots, &rt);
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
        let mut scored: Vec<(Option<f32>, usize)> = Vec::with_capacity(slots.len());
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
                    // A chunk that recognized no words says nothing either way.
                    let words = text.raw.split_whitespace().count();
                    if words > 0 {
                        scored.push((text.confidence, words));
                    }
                    let chosen = text.corrected.unwrap_or(text.raw);
                    if !chosen.is_empty() {
                        parts.push(chosen);
                    }
                }
                Some(Err(err)) => {
                    tracing::warn!(chunk = slot.index, error = %err, "chunk_retry_sync");
                    scored.push((None, 0));
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
                    scored.push((None, 0));
                    failed += 1;
                    parts.push(self.spill_placeholder(slot));
                }
            }
        }

        ChunkedTranscript {
            text: join_chunk_texts(&parts),
            chunk_count: slots.len(),
            failed_chunks: failed,
            confidence_parts: scored,
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
    #[test]
    fn cut_ellipsis_is_stripped_between_chunks_only() {
        let parts = vec![
            "For the...".to_string(),
            "but the UX…".to_string(),
            "and the end...".to_string(),
        ];
        assert_eq!(join_chunk_texts(&parts), "For the but the UX and the end...");
    }

    #[test]
    fn strip_cut_ellipsis_handles_spacing_and_repeats() {
        assert_eq!(strip_cut_ellipsis("why I... "), "why I");
        assert_eq!(strip_cut_ellipsis("so … ..."), "so");
        assert_eq!(strip_cut_ellipsis("done."), "done.");
        assert_eq!(strip_cut_ellipsis(""), "");
    }

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
    fn weighted_confidence_is_word_weighted_and_unknown_if_any_part_is() {
        assert_eq!(weighted_confidence(&[]), None);
        let c = weighted_confidence(&[(Some(0.9), 30), (Some(0.1), 10)]).unwrap();
        assert!((c - 0.7).abs() < 1e-6, "{c}");
        assert_eq!(weighted_confidence(&[(Some(0.9), 30), (None, 0)]), None);
    }

    #[test]
    fn current_join_prefers_corrected_and_matches_finalize_choice() {
        let slots = vec![
            slot_with(Some(Ok(ChunkText {
                raw: "furst part".into(),
                corrected: Some("first part".into()),
                confidence: None,
            }))),
            // Correction still in flight (or failed): raw, as finalize
            // would paste it right now.
            slot_with(Some(Ok(ChunkText {
                raw: "second part".into(),
                corrected: None,
                confidence: None,
            }))),
        ];
        assert_eq!(
            current_join(&slots).as_deref(),
            Some("first part second part")
        );
    }

    #[test]
    fn current_join_is_none_while_a_chunk_is_in_flight_or_failed() {
        let in_flight = vec![slot_with(None)];
        assert!(current_join(&in_flight).is_none());

        let failed = vec![slot_with(Some(Err("boom".into())))];
        assert!(current_join(&failed).is_none());
    }

    #[test]
    fn finalize_pastes_the_raw_join_when_correction_has_not_landed() {
        // The warm at raw-publish time targets this exact string; finalize
        // must assemble the same one when the cut beats the correction.
        let acc = ChunkAccumulator::new();
        acc.slots.lock().push(slot_with(Some(Ok(ChunkText {
            raw: "first part".into(),
            corrected: Some("First part.".into()),
            confidence: None,
        }))));
        acc.slots.lock().push(slot_with(Some(Ok(ChunkText {
            raw: "second part".into(),
            corrected: None,
            confidence: None,
        }))));
        let warmed = acc.join_handle().current_join().expect("determined");
        let t = mock(vec![], vec![]);
        assert_eq!(acc.finalize(&t).text, warmed);
    }

    #[test]
    fn no_grammar_flush_settles_immediately() {
        let rt = rt();
        let t = mock(vec!["hello there"], vec![]);
        let mut acc = ChunkAccumulator::new();
        acc.flush(vec![0i16; 16_000], &t, None, rt.handle());
        let handle = acc.join_handle();
        // The background thread stores the raw result (no post-processor
        // was supplied, so it never changes); poll briefly for it.
        let started = Instant::now();
        let join = loop {
            if let Some(j) = handle.current_join() {
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
