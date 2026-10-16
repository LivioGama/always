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
//! its WAV is written to `~/.always/failed-chunks/` and the chunk is left
//! out of the joined text — counted in `failed_chunks` so the paste path
//! can cue the loss and hold back auto-Enter. No marker text ever enters
//! the transcript: it would be pasted into the user's app (2026-10-16,
//! a user got a literal `[audio saved: chunk 2]` in his message).

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
/// Floor of the per-chunk wait at finalize before the chunk is retried.
const FINALIZE_MIN_WAIT_SECS: f64 = 10.0;

#[cfg(test)]
thread_local! {
    /// Test override of [`FINALIZE_MIN_WAIT_SECS`] (finalize runs on the
    /// test's own thread, so parallel tests are unaffected).
    static TEST_MIN_WAIT_SECS: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
}

fn finalize_min_wait_secs() -> f64 {
    #[cfg(test)]
    if let Some(secs) = TEST_MIN_WAIT_SECS.with(|c| c.get()) {
        return secs;
    }
    FINALIZE_MIN_WAIT_SECS
}

#[cfg(test)]
static TEST_PAUSE_AFTER_RAW_PUBLISH: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>> =
    std::sync::Mutex::new(None);

/// One committed chunk. `audio` is retained until the transcription
/// succeeds so a failed chunk can be retried / spilled, never dropped.
struct ChunkSlot {
    index: usize,
    /// The chunk's audio starts with [`SEAM_CONTEXT_SAMPLES`] already
    /// committed in the previous chunk (it follows a hard cut), so its
    /// first words may repeat the previous chunk's last ones — the join
    /// drops that duplicate (see [`join_chunk_parts`]).
    overlaps_prev: bool,
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
    /// every chunk failed. Never contains a placeholder for a failed chunk.
    pub text: String,
    pub chunk_count: usize,
    /// Chunks whose words are missing from `text` (audio spilled to the
    /// failed-chunks dir instead).
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
    let mut parts: Vec<(String, bool)> = Vec::with_capacity(slots.len());
    for slot in slots {
        let guard = slot.result.lock();
        match guard.as_ref() {
            Some(Ok(text)) => {
                let chosen = text.corrected.clone().unwrap_or_else(|| text.raw.clone());
                if !chosen.is_empty() {
                    parts.push((chosen, slot.overlaps_prev));
                }
            }
            _ => return None,
        }
    }
    Some(join_chunk_parts(&parts))
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
    let flagged: Vec<(String, bool)> = parts.iter().map(|p| (p.clone(), false)).collect();
    join_chunk_parts(&flagged)
}

/// Audio from the end of a hard-cut chunk that is also fed, as leading
/// context, to the chunk after it. A recogniser handed audio that starts
/// right at a word onset drops that word: replayed 2026-10-14 through the
/// real VAD + chunker, cuts landing in an inter-word gap still lost "and"
/// ("very weird | annoying") and "ask but" ("should never | for a
/// reversible") at the start of the next chunk; with 0.5 s of the
/// previous audio in front, both were recognised. 0.6 s for margin.
/// The open tail never gets this context — it is transcribed on the paste
/// path, and the extra audio would add recognition time there.
pub const SEAM_CONTEXT_SAMPLES: usize = 9_600;
/// Longest run of words the seam merge will treat as a duplicate: 0.6 s
/// of context holds at most a few words.
const SEAM_MAX_DUP_WORDS: usize = 4;

fn word_core(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// How many leading words of `next` repeat the trailing words of `prev`
/// exactly (case/punctuation-insensitive), up to `SEAM_MAX_DUP_WORDS`.
/// Only an exact suffix==prefix match counts: a word that is not a
/// verbatim repeat is never removed, so the merge cannot lose speech —
/// at worst a genuinely repeated word at the seam ("very, very") is
/// collapsed, which `local_cleanup` does to adjacent repeats anyway.
pub fn seam_duplicate_words(prev: &str, next: &str) -> usize {
    let p: Vec<String> = prev.split_whitespace().map(word_core).collect();
    let n: Vec<String> = next.split_whitespace().map(word_core).collect();
    let max = SEAM_MAX_DUP_WORDS.min(p.len()).min(n.len());
    (1..=max)
        .rev()
        .find(|&k| {
            let tail = &p[p.len() - k..];
            let head = &n[..k];
            tail.iter().all(|w| !w.is_empty()) && tail == head
        })
        .unwrap_or(0)
}

/// Join committed chunk texts in flush order; `overlaps_prev` marks a part
/// whose audio began with [`SEAM_CONTEXT_SAMPLES`] of the previous chunk,
/// and its duplicated leading words are dropped (see
/// [`seam_duplicate_words`]). Every part but the last ends at a cut, so
/// its trailing ellipsis is dropped too (see `strip_cut_ellipsis`).
pub fn join_chunk_parts(parts: &[(String, bool)]) -> String {
    let n = parts.len();
    let mut out = String::new();
    for (i, (part, overlaps_prev)) in parts.iter().enumerate() {
        let mut piece = if i + 1 < n {
            strip_cut_ellipsis(part)
        } else {
            part.as_str()
        };
        if *overlaps_prev && !out.is_empty() {
            let dup = seam_duplicate_words(&out, piece);
            if dup > 0 {
                piece = skip_words(piece, dup);
            }
        }
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

/// `text` without its first `k` whitespace-separated words.
fn skip_words(text: &str, k: usize) -> &str {
    let mut rest = text.trim_start();
    for _ in 0..k {
        match rest.find(char::is_whitespace) {
            Some(end) => rest = rest[end..].trim_start(),
            None => return "",
        }
    }
    rest
}

/// Energy window and hop for [`quiet_cut_point`]: 30 ms windows every 10 ms
/// at 16 kHz.
const QUIET_WIN: usize = 480;
const QUIET_HOP: usize = 160;

/// Where to cut an oversized chunk that never reached a natural pause.
///
/// The hard flush used to cut at whatever sample the 15 s ceiling landed on
/// — mid-word, with no overlap — so the word straddling the cut was split
/// across two independent recognitions and garbled or lost on both sides.
/// Observed 2026-10-14 14:48:33Z: "irreversible" became chunk 1's last word
/// "Eriver." and chunk 2's first word "Yes,"; 14:44:35Z "you can surely
/// see|in the logs" became "surely she," + "See in the locks".
///
/// Returns the centre of the quietest 30 ms window within the last
/// `search_samples` of `samples` — in fluent speech that is the gap between
/// two words (or at worst a stop-consonant closure), never a vowel. Every
/// sample is kept: the caller commits `[..cut]` and carries `[cut..]` into
/// the next chunk, so no audio is dropped or duplicated and no text merge
/// is needed. Returns `samples.len()` when there is nothing to search.
pub fn quiet_cut_point(samples: &[i16], search_samples: usize) -> usize {
    let len = samples.len();
    if len < QUIET_WIN {
        return len;
    }
    let energy = |start: usize| -> u64 {
        samples[start..start + QUIET_WIN]
            .iter()
            .map(|&s| (s as i64 * s as i64) as u64)
            .sum()
    };
    let search_start = len.saturating_sub(search_samples.max(QUIET_WIN));
    let mut best_start = len - QUIET_WIN;
    let mut best_energy = u64::MAX;
    let mut start = search_start;
    while start + QUIET_WIN <= len {
        let e = energy(start);
        // `<=` prefers the LATEST of equally quiet windows: the longest
        // chunk, the shortest carry-over.
        if e <= best_energy {
            best_energy = e;
            best_start = start;
        }
        start += QUIET_HOP;
    }
    // A minimum on the window's leading edge is the tail of a slope that
    // continues outside it — cutting there clips the onset of the next
    // word (replayed 2026-10-14: a cut 15 ms into the window dropped
    // "and"). Follow the slope back to the real gap, bounded by the same
    // distance again so a chunk never shrinks below ~9 s.
    if best_start < search_start + 2 * QUIET_HOP {
        let floor = search_start.saturating_sub(search_samples);
        while best_start >= floor + QUIET_HOP {
            let e = energy(best_start - QUIET_HOP);
            if e > best_energy {
                break;
            }
            best_energy = e;
            best_start -= QUIET_HOP;
        }
    }
    best_start + QUIET_WIN / 2
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

    /// Raw text of the most recently committed chunk once its background
    /// transcription has landed (`None` while in flight, on failure, or
    /// with no chunks). Non-consuming. The end-of-utterance decision reads
    /// it at the pause a chunk was flushed in: that pause has no
    /// speculative transcript of its own, so this is the only text that
    /// can tell "finished" from "uh, ..." there.
    pub fn last_raw(&self) -> Option<String> {
        let last = self.slots.lock().last().cloned()?;
        let guard = last.result.lock();
        match guard.as_ref() {
            Some(Ok(text)) => Some(text.raw.clone()),
            _ => None,
        }
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
        self.flush_with_context(Vec::new(), audio_chunk, transcriber, grammar, rt);
    }

    /// [`flush`](Self::flush) with `context` — the end of the previous,
    /// hard-cut chunk — transcribed in front of `audio_chunk` so the
    /// recogniser hears the chunk's first word with lead-in (see
    /// [`SEAM_CONTEXT_SAMPLES`]). Empty `context` is a plain flush.
    pub fn flush_with_context(
        &mut self,
        context: Vec<i16>,
        audio_chunk: Vec<i16>,
        transcriber: &Arc<dyn Transcriber>,
        grammar: Option<Arc<crate::always::postprocess::PostProcessor>>,
        rt: &tokio::runtime::Handle,
    ) {
        let index = self.slots.lock().len();
        let secs = audio_chunk.len() as f64 / 16_000.0;
        let overlaps_prev = !context.is_empty() && index > 0;
        let context_ms = context.len() / 16;
        let mut audio_chunk = audio_chunk;
        if overlaps_prev {
            let mut with_context = context;
            with_context.extend_from_slice(&audio_chunk);
            audio_chunk = with_context;
        }
        let slot = Arc::new(ChunkSlot {
            index,
            overlaps_prev,
            result: Mutex::new(None),
            audio: Mutex::new(Some(audio_chunk.clone())),
        });
        self.slots.lock().push(Arc::clone(&slot));
        tracing::info!(chunk = index, secs, context_ms, "chunk_flush");

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
                    // already returned usable text. (No placeholder is
                    // pasted any more — the chunk would be missing.)
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
    /// and the chunk is left out of the text (counted in `failed_chunks`).
    /// A chunk still in flight past its wait budget gets the same retry
    /// (a local engine — Apple On-Device, Parakeet — that stalled on one
    /// call usually answers a fresh one), then a last look at the
    /// straggler's own result before it is given up.
    ///
    /// Emits the GUI transcribing heartbeat while waiting so the overlay
    /// lease can't expire during a long straggler.
    pub fn finalize(&self, transcriber: &Arc<dyn Transcriber>) -> ChunkedTranscript {
        let slots: Vec<Arc<ChunkSlot>> = self.slots.lock().clone();
        let mut parts: Vec<(String, bool)> = Vec::with_capacity(slots.len());
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
            let min_wait = finalize_min_wait_secs();
            let max_wait =
                Duration::from_secs_f64((audio_secs * 0.5).clamp(min_wait, min_wait.max(60.0)));
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
                        parts.push((chosen, slot.overlaps_prev));
                    }
                }
                Some(Err(err)) => {
                    tracing::warn!(chunk = slot.index, error = %err, "chunk_retry_sync");
                    scored.push((None, 0));
                    match self.retry_sync(slot, transcriber) {
                        Some(text) => parts.push((text, slot.overlaps_prev)),
                        None => {
                            failed += 1;
                            self.spill(slot);
                        }
                    }
                }
                None => {
                    // Still in flight past the budget. One fresh attempt;
                    // the straggler finishes into the slot harmlessly, and
                    // if it landed meanwhile its text is used.
                    tracing::warn!(chunk = slot.index, "chunk_wait_timeout");
                    scored.push((None, 0));
                    let recovered = self.retry_sync(slot, transcriber).or_else(|| {
                        match slot.result.lock().take() {
                            Some(Ok(text)) => {
                                let chosen = text.corrected.unwrap_or(text.raw);
                                (!chosen.is_empty()).then_some(chosen)
                            }
                            _ => None,
                        }
                    });
                    match recovered {
                        Some(text) => parts.push((text, slot.overlaps_prev)),
                        None => {
                            failed += 1;
                            self.spill(slot);
                        }
                    }
                }
            }
        }

        ChunkedTranscript {
            text: join_chunk_parts(&parts),
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

    /// Write a lost chunk's WAV to the failed-chunks dir so its speech
    /// stays recoverable. Nothing is added to the transcript.
    fn spill(&self, slot: &ChunkSlot) {
        let Some(audio_samples) = slot.audio.lock().clone() else {
            // Transcription succeeded but the result was consumed by a
            // racing path — nothing to spill.
            return;
        };
        if let Some(path) = spill_audio(&audio_samples, &format!("{}", slot.index + 1)) {
            tracing::warn!(chunk = slot.index, path = %path.display(), "chunk_audio_spilled");
        }
    }
}

/// Write `samples` as a WAV into the failed-chunks recovery dir
/// (`~/.always/failed-chunks/chunk-<nanos>-<label>.wav`, owner-only).
/// `None` (logged) when the write failed.
pub(crate) fn spill_audio(samples: &[i16], label: &str) -> Option<std::path::PathBuf> {
    let dir = failed_chunks_dir();
    if let Err(err) = create_private_dir(&dir) {
        tracing::error!(error = %err, "failed_chunks_dir_create_failed");
        return None;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = dir.join(format!("chunk-{stamp}-{label}.wav"));
    match audio::create_wav_bytes_i16_mono_16k(samples)
        .and_then(|wav| write_private_file(&path, &wav))
    {
        Ok(()) => Some(path),
        Err(err) => {
            tracing::error!(label, error = %err, "chunk_audio_spill_failed");
            None
        }
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
    // Unit tests spill silent fixture chunks; never into the user's real
    // recovery folder (they used to land in ~/.always/failed-chunks).
    if cfg!(test) {
        return std::env::temp_dir().join("always-test-failed-chunks");
    }
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
        assert_eq!(
            join_chunk_texts(&parts),
            "For the but the UX and the end..."
        );
    }

    /// Synthetic "speech": loud tone with a short inter-word gap.
    fn tone(n: usize, amp: f32) -> Vec<i16> {
        (0..n)
            .map(|i| ((i as f32 * 0.07).sin() * amp) as i16)
            .collect()
    }

    #[test]
    fn hard_cut_lands_in_the_inter_word_gap_not_mid_word() {
        // 13.0 s word-stream, an 80 ms gap at 13.0 s, then 1.92 s of a word
        // still being spoken when the 15 s ceiling fires. The old cut was
        // `len` (mid-word); the quiet cut must land inside the gap.
        let mut s = tone(13 * 16_000, 12_000.0);
        let gap_start = s.len();
        s.extend(tone(1_280, 40.0)); // 80 ms near-silence
        let gap_end = s.len();
        s.extend(tone(15 * 16_000 - s.len(), 12_000.0));
        assert_eq!(s.len(), 15 * 16_000);
        let cut = quiet_cut_point(&s, 3 * 16_000);
        assert!(
            cut > gap_start && cut < gap_end,
            "cut {cut} not inside gap {gap_start}..{gap_end}"
        );
        // No sample is lost or duplicated: head + carry-over == original.
        let (head, rest) = s.split_at(cut);
        assert_eq!(head.len() + rest.len(), 15 * 16_000);
    }

    #[test]
    fn hard_cut_only_searches_the_tail_window() {
        // Gap at 5 s (outside the 3 s window) must be ignored.
        let mut s = tone(5 * 16_000, 12_000.0);
        s.extend(vec![0i16; 1_600]);
        s.extend(tone(15 * 16_000 - s.len(), 12_000.0));
        let cut = quiet_cut_point(&s, 3 * 16_000);
        assert!(cut >= 12 * 16_000, "cut {cut} left the search window");
        assert!(cut <= 15 * 16_000);
    }

    #[test]
    fn hard_cut_follows_a_slope_back_to_a_gap_just_outside_the_window() {
        // Gap at 11.9 s, 100 ms BEFORE the 12 s search start, with the
        // next word fading in across the window edge. The edge window is
        // the in-window minimum but sits on the onset of a word.
        let mut s = tone(11 * 16_000 + 14_400, 12_000.0); // to 11.9 s
        let gap_start = s.len();
        s.extend(std::iter::repeat_n(0i16, 1_600)); // 11.9–12.0 s silence
        let gap_end = s.len();
        // 12.0–12.3 s: onset ramp (quiet → loud), then loud speech.
        s.extend((0..4_800).map(|i| ((i as f32 * 0.07).sin() * (i as f32 * 2.5)) as i16));
        s.extend(tone(15 * 16_000 - s.len(), 12_000.0));
        let cut = quiet_cut_point(&s, 3 * 16_000);
        assert!(
            cut >= gap_start && cut <= gap_end,
            "cut {cut} not in gap {gap_start}..{gap_end}"
        );
    }

    #[test]
    fn hard_cut_on_short_buffers_is_the_end() {
        assert_eq!(quiet_cut_point(&[], 48_000), 0);
        assert_eq!(quiet_cut_point(&[1, 2, 3], 48_000), 3);
    }

    #[test]
    fn seam_merge_drops_only_verbatim_repeats_from_the_context() {
        // Replayed 2026-10-14: 1.0 s of context made the next chunk start
        // "very weird and annoying" after a chunk ending "which is very
        // weird" — the repeat goes, nothing else does.
        let parts = vec![
            ("any action which is very weird".to_string(), false),
            (
                "very weird and annoying and must be fixed".to_string(),
                true,
            ),
        ];
        assert_eq!(
            join_chunk_parts(&parts),
            "any action which is very weird and annoying and must be fixed"
        );
        // Punctuation / case differences are still a repeat.
        assert_eq!(
            seam_duplicate_words("she should never,", "Never ask, but for"),
            1
        );
        // No repeat (context words not recognised): nothing removed.
        let parts = vec![
            ("she should never".to_string(), false),
            ("ask but for irreversible actions".to_string(), true),
        ];
        assert_eq!(
            join_chunk_parts(&parts),
            "she should never ask but for irreversible actions"
        );
        // A non-overlapping part is never deduplicated.
        let parts = vec![
            ("go to the".to_string(), false),
            ("the store".to_string(), false),
        ];
        assert_eq!(join_chunk_parts(&parts), "go to the the store");
        // Bounded: a long coincidental match beyond the context is kept.
        assert_eq!(seam_duplicate_words("a b c d e f", "a b c d e f g"), 0);
    }

    #[test]
    fn flush_with_context_marks_the_seam_and_join_dedupes() {
        let rt = rt();
        let t = mock(
            vec!["which is very weird", "very weird and annoying"],
            vec![],
        );
        let mut acc = ChunkAccumulator::new();
        acc.flush(vec![0i16; 16_000], &t, None, rt.handle());
        std::thread::sleep(Duration::from_millis(50));
        acc.flush_with_context(
            vec![0i16; SEAM_CONTEXT_SAMPLES],
            vec![0i16; 16_000],
            &t,
            None,
            rt.handle(),
        );
        let joined = acc.finalize(&t);
        assert_eq!(joined.text, "which is very weird and annoying");
        // current_join reproduces finalize's text byte-for-byte.
        let mut acc2 = ChunkAccumulator::new();
        let t2 = mock(
            vec!["which is very weird", "very weird and annoying"],
            vec![],
        );
        acc2.flush(vec![0i16; 16_000], &t2, None, rt.handle());
        std::thread::sleep(Duration::from_millis(50));
        acc2.flush_with_context(
            vec![0i16; SEAM_CONTEXT_SAMPLES],
            vec![0i16; 16_000],
            &t2,
            None,
            rt.handle(),
        );
        let handle = acc2.join_handle();
        let started = Instant::now();
        let settled = loop {
            if let Some(j) = handle.current_join() {
                break j;
            }
            assert!(started.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(settled, "which is very weird and annoying");
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
        /// Calls that hang for 3 s before answering (a stalled engine).
        stall_calls: Vec<usize>,
    }

    impl Transcriber for MockTranscriber {
        fn transcribe_from_bytes(&self, _audio: Vec<u8>) -> Result<TranscriptionResult, SttError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if self.stall_calls.contains(&call) {
                std::thread::sleep(Duration::from_secs(3));
            }
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
            stall_calls: vec![],
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
    fn last_raw_is_the_newest_chunk_once_transcribed() {
        let rt = rt();
        let t = mock(vec!["first part,", "and then, uh,"], vec![]);
        let mut acc = ChunkAccumulator::new();
        assert_eq!(acc.last_raw(), None, "no chunks yet");
        acc.flush(vec![0i16; 16_000], &t, None, rt.handle());
        acc.flush(vec![0i16; 16_000], &t, None, rt.handle());
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while acc.last_raw().is_none() && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let last = acc.last_raw().expect("chunk transcribed");
        assert!(
            last == "and then, uh," || last == "first part,",
            "got {last}"
        );
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
    fn doubly_failed_chunk_is_spilled_never_a_marker() {
        let rt = rt();
        // Background AND sync retry fail → spilled WAV, no text at all.
        let t = mock(vec!["unused"], vec![0, 1]);
        let mut acc = ChunkAccumulator::new();
        acc.flush(vec![0i16; 16_000], &t, None, rt.handle());
        let joined = acc.finalize(&t);
        assert_eq!(joined.text, "");
        assert_eq!(joined.failed_chunks, 1);
    }

    /// 2026-10-16 (user report, long sentence): `[audio saved: chunk 2]`
    /// was pasted into his message. A lost middle chunk leaves its
    /// neighbours joined and is only counted.
    #[test]
    fn lost_middle_chunk_leaves_no_marker_in_the_paste() {
        let rt = rt();
        // Calls: 0 = chunk 1 ok, 1 = chunk 2 fails, 2 = chunk 3 ok,
        // 3 = chunk 2's sync retry fails.
        let t = mock(vec!["first part", "unused", "third part"], vec![1, 3]);
        let mut acc = ChunkAccumulator::new();
        for _ in 0..3 {
            acc.flush(vec![0i16; 16_000], &t, None, rt.handle());
            std::thread::sleep(Duration::from_millis(50));
        }
        let joined = acc.finalize(&t);
        assert_eq!(joined.text, "first part third part");
        assert_eq!(joined.chunk_count, 3);
        assert_eq!(joined.failed_chunks, 1);
        assert!(!joined.text.contains('['), "{:?}", joined.text);
    }

    /// A local engine (Apple On-Device / Parakeet) stalls on one chunk
    /// past the finalize budget: a fresh call recovers its words instead
    /// of giving the chunk up.
    #[test]
    fn stalled_chunk_is_retried_after_the_wait_budget() {
        let rt = rt();
        let t: Arc<dyn Transcriber> = Arc::new(MockTranscriber {
            calls: AtomicUsize::new(0),
            texts: vec!["unused", "stalled words recovered"],
            fail_calls: vec![],
            stall_calls: vec![0],
        });
        let mut acc = ChunkAccumulator::new();
        acc.flush(vec![0i16; 16_000], &t, None, rt.handle());
        std::thread::sleep(Duration::from_millis(20));
        TEST_MIN_WAIT_SECS.with(|c| c.set(Some(0.2)));
        let started = Instant::now();
        let joined = acc.finalize(&t);
        TEST_MIN_WAIT_SECS.with(|c| c.set(None));
        assert_eq!(joined.text, "stalled words recovered");
        assert_eq!(joined.failed_chunks, 0);
        assert!(started.elapsed() < Duration::from_secs(2));
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
            overlaps_prev: false,
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
