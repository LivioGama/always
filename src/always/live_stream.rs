//! Persistent cache-aware decode session, driven from the capture loop.
//!
//! # Why this exists
//!
//! Nemotron is a *streaming* model: [`crate::stt::LiveTranscriptionStream`]
//! keeps encoder/decoder state across 560 ms windows, so the transcript is
//! built WHILE the user talks. Until this module existed the daemon threw that
//! away — at end-of-speech it ran a full from-scratch decode of the whole
//! utterance, and mid-speech "previews" ran *another* full decode of the last
//! 3 s every 1.4 s, all serialised on the one shared ONNX model mutex. The
//! final decode queued behind the previews it was competing with.
//!
//! Measured on the shipped int8 model (`examples/nemotron_stream_bench.rs`):
//!
//! | audio  | one-shot decode | live per-chunk | live flush |
//! |--------|-----------------|----------------|------------|
//! | 4.7 s  | 514 ms          | 55 ms          | 54 ms      |
//! | 39.6 s | 4200 ms         | 54 ms          | 51 ms      |
//! | 119.7 s| 11512 ms        | 54 ms          | 52 ms      |
//!
//! Per-chunk cost is FLAT — 0.10x realtime, ~10x headroom — so the worker
//! never falls behind and end-of-speech costs one flush, not a re-decode.
//!
//! # Threading
//!
//! `transcribe_chunk` costs ~54 ms; the capture loop must read a frame every
//! 30 ms, so the decode runs on a dedicated worker thread fed over a channel.
//! The capture loop only ever does an O(n) i16→f32 copy and a channel send.
//!
//! # Ordering contract
//!
//! Audio must reach the session exactly once, in capture order. The caller
//! tracks how much of its buffer has been fed via [`LiveStream::fed`], and
//! must call [`LiveStream::reset`] whenever it discards or re-bases that
//! buffer (chunk flush, speaker-gate truncation) — a session fed a
//! discontinuity is poisoned, and [`LiveStream::finish`] would return a
//! transcript that does not match the audio the rest of the pipeline holds.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, sync_channel};
use std::time::Duration;

use parking_lot::Mutex;

use crate::stt::Transcriber;

/// How many chunks may sit unprocessed before we declare the worker unable to
/// keep up and stop trusting it. At the measured 0.10x realtime this is
/// unreachable; it exists so a pathologically slow machine degrades to the
/// one-shot path instead of accumulating unbounded audio.
const MAX_QUEUE_LAG_CHUNKS: usize = 16;

/// How many per-chunk transcript snapshots to retain for [`LiveStream::transcript_through`].
///
/// A speaker-gate tail cut re-bases the buffer backwards by the gate's own
/// pause tolerance — measured at 1.02 s median and 2.46 s worst over 524 real
/// cuts, i.e. under five 560 ms chunks. 32 covers ~18 s of rollback, which is
/// far past anything the gate can ask for, and costs one cumulative transcript
/// string per chunk. A cut deeper than this returns `None` and the caller
/// falls back to its one-shot decode, exactly as before.
const TRUNCATION_HISTORY_CHUNKS: usize = 32;

/// Upper bound on the finalization wait. The flush is one encoder window
/// (~54 ms measured) plus whatever is still queued; 5 s is "the worker is
/// wedged", not "the worker is slow".
pub const FINISH_TIMEOUT: Duration = Duration::from_secs(5);

enum Msg {
    Chunk {
        generation: u64,
        samples: Vec<f32>,
        /// Caller's `fed` count AFTER this chunk — the sample offset this
        /// chunk's cumulative transcript describes. Recorded so a later
        /// truncation can roll the transcript back to a chunk boundary.
        fed_after: usize,
    },
    /// Drop the current decode state. Channel order already guarantees this
    /// lands between the two segments' chunks, so it carries no generation.
    Reset,
    Finish {
        generation: u64,
        reply: SyncSender<Option<String>>,
    },
    Stop,
}

#[derive(Default)]
struct Shared {
    /// Latest full transcript, tagged with the generation that produced it so
    /// a straggler write from a discarded segment can never be read as
    /// current. Mirrors the `SpeculationSlot` generation trick in `vad.rs`.
    transcript: Mutex<(u64, String)>,
    /// Bounded trail of `(generation, fed_after, cumulative transcript)`, one
    /// entry per decoded chunk. Lets a caller that truncates its audio buffer
    /// recover the transcript as it stood at a chunk boundary at or before the
    /// cut, instead of throwing the whole session away.
    history: Mutex<VecDeque<(u64, usize, String)>>,
    queued: AtomicUsize,
    processed: AtomicUsize,
    /// Latched on the first decode error: the session is unusable from then
    /// on and the caller must fall back to a one-shot decode.
    failed: AtomicBool,
}

/// Handle to a live decode session running on its own worker thread.
pub struct LiveStream {
    tx: Sender<Msg>,
    shared: Arc<Shared>,
    worker: Option<std::thread::JoinHandle<()>>,
    chunk_samples: usize,
    /// Samples of the CURRENT segment already handed to the worker.
    fed: usize,
    generation: u64,
}

impl LiveStream {
    /// Open a session on `transcriber`, or `None` if the engine has no
    /// cache-aware streaming mode (every cloud backend, and every local
    /// engine except Nemotron today).
    pub fn start(transcriber: &Arc<dyn Transcriber>) -> Option<Self> {
        let first = transcriber.open_live_stream()?;
        let chunk_samples = first.chunk_samples();
        if chunk_samples == 0 {
            return None;
        }
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        let shared = Arc::new(Shared::default());
        let worker = {
            let shared = Arc::clone(&shared);
            let transcriber = Arc::clone(transcriber);
            std::thread::Builder::new()
                .name("stt-live-stream".into())
                .spawn(move || run_worker(rx, shared, transcriber, first))
                .ok()?
        };
        tracing::info!(chunk_samples, "live_stream_started");
        Some(Self {
            tx,
            shared,
            worker: Some(worker),
            chunk_samples,
            fed: 0,
            generation: 0,
        })
    }

    pub fn chunk_samples(&self) -> usize {
        self.chunk_samples
    }

    /// Samples of the current segment already handed to the worker.
    pub fn fed(&self) -> usize {
        self.fed
    }

    /// Whether the worker has decoded everything handed to it. The
    /// mid-sentence heuristic inspects the TRAILING word, so it must not run
    /// on a transcript that is still missing the last window of audio.
    pub fn caught_up(&self) -> bool {
        self.shared.queued.load(Ordering::Relaxed) == self.shared.processed.load(Ordering::Relaxed)
    }

    /// Latched decode failure, or a worker that fell too far behind. Once
    /// true the caller must use its one-shot path for this utterance.
    pub fn degraded(&self) -> bool {
        self.shared.failed.load(Ordering::Relaxed)
            || self
                .shared
                .queued
                .load(Ordering::Relaxed)
                .saturating_sub(self.shared.processed.load(Ordering::Relaxed))
                > MAX_QUEUE_LAG_CHUNKS
    }

    /// Hand the worker every complete chunk of `buffer` not yet fed.
    ///
    /// `buffer` is the caller's whole current segment (i16, 16 kHz mono); the
    /// already-fed prefix is skipped. Partial trailing audio is left for the
    /// next call or for [`Self::finish`].
    pub fn feed(&mut self, buffer: &[i16]) {
        if self.degraded() {
            return;
        }
        while buffer.len() - self.fed >= self.chunk_samples {
            let end = self.fed + self.chunk_samples;
            let samples = to_f32(&buffer[self.fed..end]);
            self.fed = end;
            self.shared.queued.fetch_add(1, Ordering::Relaxed);
            if self
                .tx
                .send(Msg::Chunk {
                    generation: self.generation,
                    samples,
                    fed_after: self.fed,
                })
                .is_err()
            {
                self.shared.failed.store(true, Ordering::Relaxed);
                return;
            }
        }
    }

    /// The transcript decoded so far, or `None` if the session is degraded or
    /// nothing has been decoded yet. Never blocks — safe on the capture loop.
    pub fn transcript(&self) -> Option<String> {
        if self.shared.failed.load(Ordering::Relaxed) {
            return None;
        }
        let guard = self.shared.transcript.lock();
        (guard.0 == self.generation && !guard.1.is_empty()).then(|| guard.1.clone())
    }

    /// Discard the current segment and start a fresh session.
    ///
    /// MUST be called whenever the caller drops or re-bases the audio buffer
    /// (chunk flush, speaker-gate tail cut). Bumps the generation so in-flight
    /// work from the old segment can never be read as the new one's.
    pub fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.fed = 0;
        self.shared.history.lock().clear();
        let _ = self.tx.send(Msg::Reset);
    }

    /// The cumulative transcript as it stood after the last chunk that ended
    /// at or before `samples`, for the CURRENT segment.
    ///
    /// # Why this exists
    ///
    /// The speaker gate can truncate the caller's audio buffer mid-utterance
    /// (`speaker_gate_tail_cut`, `speaker_gate_chunk_refuted`). The session has
    /// already decoded past the cut, so it cannot keep streaming — but the
    /// audio *before* the cut was decoded correctly, and that prefix transcript
    /// is still exactly right. Recovering it turns a tail cut from "re-decode
    /// the whole utterance from scratch" (measured 7.3-40.0 s on real
    /// utterances) into a lock and a clone.
    ///
    /// Rolling back to a chunk BOUNDARY is what makes this safe rather than a
    /// guess: the returned transcript describes only audio ending at or before
    /// the cut, so no rejected trailing audio can reach the output. The cost is
    /// that up to one 560 ms chunk of *confirmed* speech before the cut is
    /// dropped along with it — the same direction of error the gate itself
    /// already chose, and the gate cuts at a 500 ms cadence anyway.
    ///
    /// `None` when the session is degraded, when nothing was decoded, or when
    /// the cut reaches back further than the retained history — all of which
    /// leave the caller on its unchanged one-shot path.
    pub fn transcript_through(&self, samples: usize) -> Option<String> {
        if self.shared.failed.load(Ordering::Relaxed) {
            return None;
        }
        let history = self.shared.history.lock();
        history
            .iter()
            .rev()
            .find(|(generation, fed_after, text)| {
                *generation == self.generation && *fed_after <= samples && !text.is_empty()
            })
            .map(|(_, _, text)| text.clone())
    }

    /// Feed `tail` (the un-fed remainder of the segment), flush the decoder,
    /// and return the final transcript.
    ///
    /// `None` means "no usable result" — degraded session, decode error, or
    /// the worker did not answer within `timeout`. The caller must then run
    /// its one-shot decode. An empty-but-successful decode returns
    /// `Some("")`, which is a real "no words were spoken" answer.
    pub fn finish(&mut self, tail: &[i16], timeout: Duration) -> Option<String> {
        if self.degraded() {
            return None;
        }
        // Everything not yet fed: whole chunks first, then the short remainder
        // (the implementation zero-pads it — correct only because this is the
        // end of the utterance).
        self.feed(tail);
        if tail.len() > self.fed {
            // Zero-pad to a full encoder window HERE rather than trusting each
            // implementation to do it: padding is only ever correct at the end
            // of an utterance, so the decision belongs to the one call site
            // that knows this is the end.
            let mut samples = to_f32(&tail[self.fed..]);
            samples.resize(self.chunk_samples, 0.0);
            self.fed = tail.len();
            self.shared.queued.fetch_add(1, Ordering::Relaxed);
            if self
                .tx
                .send(Msg::Chunk {
                    generation: self.generation,
                    samples,
                    fed_after: self.fed,
                })
                .is_err()
            {
                return None;
            }
        }
        let (reply_tx, reply_rx) = sync_channel(1);
        self.tx
            .send(Msg::Finish {
                generation: self.generation,
                reply: reply_tx,
            })
            .ok()?;
        match reply_rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!(
                    timeout_ms = timeout.as_millis() as u64,
                    "live_stream_finish_timeout"
                );
                self.shared.failed.store(true, Ordering::Relaxed);
                None
            }
        }
    }
}

impl Drop for LiveStream {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        // Do NOT join: the worker may be mid-decode (~54 ms) and the capture
        // loop must not stall at end-of-utterance. The thread owns everything
        // it touches and exits as soon as the decode returns.
        drop(self.worker.take());
    }
}

fn to_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|s| *s as f32 / 32_768.0).collect()
}

fn run_worker(
    rx: Receiver<Msg>,
    shared: Arc<Shared>,
    transcriber: Arc<dyn Transcriber>,
    first: Box<dyn crate::stt::LiveTranscriptionStream>,
) {
    let mut session = Some(first);
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Stop => break,
            Msg::Reset => {
                // Drop the old decode state; the next chunk lazily opens a
                // fresh session so a reset costs nothing when the utterance
                // ends right after a flush.
                session = None;
            }
            Msg::Chunk {
                generation,
                samples,
                fed_after,
            } => {
                let Some(s) = ensure_session(&mut session, &transcriber, &shared) else {
                    continue;
                };
                match s.push_chunk(&samples) {
                    Ok(text) => {
                        {
                            let mut history = shared.history.lock();
                            if history.len() == TRUNCATION_HISTORY_CHUNKS {
                                history.pop_front();
                            }
                            history.push_back((generation, fed_after, text.clone()));
                        }
                        let mut guard = shared.transcript.lock();
                        *guard = (generation, text);
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "live_stream_chunk_failed");
                        shared.failed.store(true, Ordering::Relaxed);
                    }
                }
                shared.processed.fetch_add(1, Ordering::Relaxed);
            }
            Msg::Finish { generation, reply } => {
                let outcome = match session.as_mut() {
                    Some(s) if !shared.failed.load(Ordering::Relaxed) => match s.finish() {
                        Ok(text) => {
                            let mut guard = shared.transcript.lock();
                            *guard = (generation, text.clone());
                            Some(text)
                        }
                        Err(error) => {
                            tracing::warn!(error = %error, "live_stream_finish_failed");
                            shared.failed.store(true, Ordering::Relaxed);
                            None
                        }
                    },
                    // No session means no audio ever reached this segment
                    // (utterance ended immediately after a reset) — a real,
                    // empty answer rather than a failure.
                    Some(_) => None,
                    None => Some(String::new()),
                };
                let _ = reply.send(outcome);
                // One session per utterance: the handle is dropped right
                // after `finish`, so stop rather than linger on a thread.
                break;
            }
        }
    }
}

fn ensure_session<'a>(
    session: &'a mut Option<Box<dyn crate::stt::LiveTranscriptionStream>>,
    transcriber: &Arc<dyn Transcriber>,
    shared: &Arc<Shared>,
) -> Option<&'a mut Box<dyn crate::stt::LiveTranscriptionStream>> {
    if session.is_none() {
        match transcriber.open_live_stream() {
            Some(s) => *session = Some(s),
            None => {
                shared.failed.store(true, Ordering::Relaxed);
                return None;
            }
        }
    }
    session.as_mut()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::{
        LiveTranscriptionStream, StreamingTranscriptionResult, SttError, TranscriptionResult,
    };
    use futures::stream::Stream;
    use std::pin::Pin;

    const CHUNK: usize = 8_960;

    /// Records every chunk it is fed and reports one word per chunk, so a
    /// test can assert both the transcript and the exact audio that produced
    /// it without loading a 900 MB model.
    struct FakeStream {
        fed: Arc<Mutex<Vec<Vec<f32>>>>,
        finished: Arc<AtomicBool>,
        fail_on_chunk: Option<usize>,
    }

    impl LiveTranscriptionStream for FakeStream {
        fn chunk_samples(&self) -> usize {
            CHUNK
        }
        fn push_chunk(&mut self, samples: &[f32]) -> Result<String, SttError> {
            let mut fed = self.fed.lock();
            if self.fail_on_chunk == Some(fed.len()) {
                return Err(SttError::Other(anyhow::anyhow!("boom")));
            }
            fed.push(samples.to_vec());
            Ok((0..fed.len())
                .map(|i| format!("w{i}"))
                .collect::<Vec<_>>()
                .join(" "))
        }
        fn finish(&mut self) -> Result<String, SttError> {
            self.finished.store(true, Ordering::Relaxed);
            let fed = self.fed.lock();
            Ok((0..fed.len())
                .map(|i| format!("w{i}"))
                .collect::<Vec<_>>()
                .join(" ")
                + " end")
        }
    }

    #[derive(Default)]
    struct FakeTranscriber {
        fed: Arc<Mutex<Vec<Vec<f32>>>>,
        finished: Arc<AtomicBool>,
        opened: Arc<AtomicUsize>,
        streaming: bool,
        fail_on_chunk: Option<usize>,
    }

    impl Transcriber for FakeTranscriber {
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
        fn open_live_stream(&self) -> Option<Box<dyn LiveTranscriptionStream>> {
            if !self.streaming {
                return None;
            }
            self.opened.fetch_add(1, Ordering::Relaxed);
            Some(Box::new(FakeStream {
                fed: Arc::clone(&self.fed),
                finished: Arc::clone(&self.finished),
                fail_on_chunk: self.fail_on_chunk,
            }))
        }
    }

    fn streaming_transcriber() -> (Arc<dyn Transcriber>, Arc<Mutex<Vec<Vec<f32>>>>) {
        let fed = Arc::new(Mutex::new(Vec::new()));
        let t = FakeTranscriber {
            fed: Arc::clone(&fed),
            streaming: true,
            ..Default::default()
        };
        (Arc::new(t) as Arc<dyn Transcriber>, fed)
    }

    fn wait_for<F: Fn() -> bool>(f: F) -> bool {
        for _ in 0..400 {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn non_streaming_engine_yields_no_session() {
        let t: Arc<dyn Transcriber> = Arc::new(FakeTranscriber::default());
        assert!(LiveStream::start(&t).is_none());
    }

    #[test]
    fn feeds_only_whole_chunks_and_tracks_the_boundary() {
        let (t, fed) = streaming_transcriber();
        let mut s = LiveStream::start(&t).unwrap();
        // 2.5 chunks in: exactly 2 go out, the remainder waits.
        s.feed(&vec![1i16; CHUNK * 5 / 2]);
        assert_eq!(s.fed(), CHUNK * 2);
        assert!(wait_for(|| fed.lock().len() == 2));
        // Re-feeding the SAME buffer must not re-send the fed prefix.
        s.feed(&vec![1i16; CHUNK * 5 / 2]);
        assert_eq!(s.fed(), CHUNK * 2);
        assert_eq!(fed.lock().len(), 2);
    }

    /// The speaker-gate rescue path: a truncation must be answerable from the
    /// chunk history WITHOUT a re-decode, and must never hand back a
    /// transcript that describes audio past the cut.
    #[test]
    fn transcript_through_rolls_back_to_the_chunk_boundary_at_or_before_a_cut() {
        let (t, fed) = streaming_transcriber();
        let mut s = LiveStream::start(&t).unwrap();
        s.feed(&vec![1i16; CHUNK * 3]);
        assert!(wait_for(|| fed.lock().len() == 3));

        // Exactly on a boundary: everything decoded up to it.
        assert_eq!(s.transcript_through(CHUNK * 3).as_deref(), Some("w0 w1 w2"));
        // A cut one sample before the third boundary must NOT include w2 —
        // that chunk covers audio the gate rejected.
        assert_eq!(
            s.transcript_through(CHUNK * 3 - 1).as_deref(),
            Some("w0 w1")
        );
        // A cut mid-second-chunk falls back to the first boundary.
        assert_eq!(s.transcript_through(CHUNK + 1).as_deref(), Some("w0"));
        // A cut before any boundary has nothing to offer — caller re-decodes.
        assert_eq!(s.transcript_through(CHUNK - 1), None);
        // A cut past the end still cannot invent audio.
        assert_eq!(
            s.transcript_through(usize::MAX).as_deref(),
            Some("w0 w1 w2")
        );

        // A re-based segment must never answer with the old one's text.
        s.reset();
        assert_eq!(s.transcript_through(CHUNK * 3), None);
    }

    #[test]
    fn transcript_is_the_full_text_not_a_delta() {
        let (t, _fed) = streaming_transcriber();
        let mut s = LiveStream::start(&t).unwrap();
        s.feed(&vec![1i16; CHUNK * 3]);
        assert!(wait_for(|| s.transcript().as_deref() == Some("w0 w1 w2")));
    }

    #[test]
    fn finish_pads_the_tail_flushes_and_returns_the_final_text() {
        let (t, fed) = streaming_transcriber();
        let mut s = LiveStream::start(&t).unwrap();
        let audio = vec![1i16; CHUNK * 2 + 100];
        s.feed(&audio);
        let out = s.finish(&audio, FINISH_TIMEOUT);
        assert_eq!(out.as_deref(), Some("w0 w1 w2 end"));
        let fed = fed.lock();
        assert_eq!(fed.len(), 3, "two whole chunks plus the padded tail");
        assert_eq!(fed[2].len(), CHUNK, "tail is zero-padded to a full window");
        assert_eq!(fed[2][100..].iter().copied().sum::<f32>(), 0.0);
    }

    #[test]
    fn finish_without_any_audio_returns_an_empty_transcript() {
        let (t, _fed) = streaming_transcriber();
        let mut s = LiveStream::start(&t).unwrap();
        // No chunk ever reached the worker, but a session was pre-opened, so
        // finish still drains it rather than reporting failure.
        assert_eq!(s.finish(&[], FINISH_TIMEOUT).as_deref(), Some(" end"));
    }

    #[test]
    fn caught_up_reports_whether_the_worker_drained_the_queue() {
        let (t, _fed) = streaming_transcriber();
        let mut s = LiveStream::start(&t).unwrap();
        assert!(s.caught_up(), "nothing queued yet");
        s.feed(&vec![1i16; CHUNK * 3]);
        assert!(wait_for(|| s.caught_up()));
        assert_eq!(s.transcript().as_deref(), Some("w0 w1 w2"));
    }

    #[test]
    fn reset_rebases_the_boundary_and_hides_the_old_transcript() {
        let (t, _fed) = streaming_transcriber();
        let mut s = LiveStream::start(&t).unwrap();
        s.feed(&vec![1i16; CHUNK * 2]);
        assert!(wait_for(|| s.transcript().is_some()));
        s.reset();
        assert_eq!(s.fed(), 0);
        // The old generation's text must never be readable as the new one's.
        assert_eq!(s.transcript(), None);
    }

    #[test]
    fn a_decode_error_degrades_the_session_so_the_caller_falls_back() {
        let fed = Arc::new(Mutex::new(Vec::new()));
        let t: Arc<dyn Transcriber> = Arc::new(FakeTranscriber {
            fed: Arc::clone(&fed),
            streaming: true,
            fail_on_chunk: Some(1),
            ..Default::default()
        });
        let mut s = LiveStream::start(&t).unwrap();
        s.feed(&vec![1i16; CHUNK * 2]);
        assert!(wait_for(|| s.degraded()));
        assert_eq!(s.transcript(), None);
        assert_eq!(s.finish(&vec![1i16; CHUNK * 2], FINISH_TIMEOUT), None);
    }
}
