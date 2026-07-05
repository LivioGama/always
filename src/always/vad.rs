use anyhow::{Context, Result};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::always::audio::{self, FRAME_BYTES, FRAME_MS, FRAME_SAMPLES};
use crate::always::config::AlwaysConfig;
use crate::always::event;
use crate::always::keyboard;
use crate::always::log::{Event, Logger};
use crate::always::pause;
use crate::always::vad_silero::SileroVad;
use crate::stt::Transcriber;

/// Speculative transcription slot with a generation counter so late
/// writes from discarded speculation threads cannot poison the slot.
struct SpeculationSlot {
    generation: AtomicU32,
    result: Mutex<Option<Result<crate::stt::TranscriptionResult>>>,
}

impl SpeculationSlot {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            generation: AtomicU32::new(0),
            result: Mutex::new(None),
        })
    }

    fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::Release);
        *self.result.lock() = None;
    }

    fn current_generation(&self) -> u32 {
        self.generation.load(Ordering::Acquire)
    }

    fn store_if_current(&self, captured_gen: u32, value: Result<crate::stt::TranscriptionResult>) {
        if captured_gen == self.generation.load(Ordering::Acquire) {
            *self.result.lock() = Some(value);
        }
    }

    fn take(&self) -> Option<Result<crate::stt::TranscriptionResult>> {
        self.result.lock().take()
    }

    /// Non-consuming look at the speculative transcript text. Used by the
    /// adaptive mid-sentence extension to inspect the text while leaving
    /// the result in place for the finalize path. Returns `None` while
    /// the speculation is still in flight, failed, or was invalidated.
    fn peek_text(&self) -> Option<String> {
        match self.result.lock().as_ref() {
            Some(Ok(r)) if !r.text.is_empty() => Some(r.text.clone()),
            _ => None,
        }
    }

    /// Has ANY speculation outcome (success or error) landed? Lets the
    /// adaptive decision grace stop waiting as soon as the thread reports,
    /// instead of burning the full grace on a failed transcription.
    fn peek_ready(&self) -> bool {
        self.result.lock().is_some()
    }
}

/// Per-utterance latency capture points, consumed by the
/// `latency_breakdown` log line in `event_loop` so regressions in the
/// speech-end → paste path are visible in production logs.
pub struct UtteranceTiming {
    /// When the final-silence (or max-frames) cut fired — the moment the
    /// user is considered done speaking.
    pub speech_end_at: std::time::Instant,
    /// When the transcription result became available to the main loop
    /// (speculation slot taken or fresh transcription returned).
    pub stt_done_at: std::time::Instant,
    /// Whether the speculative transcription result was used.
    pub speculation_used: bool,
}

pub enum RecordResult {
    Speech {
        text: String,
        energy: f64,
        transcription: crate::stt::TranscriptionResult,
        timing: UtteranceTiming,
    },
    Silence,
    DroppedLowEnergy {
        energy: f64,
    },
    DroppedNoise {
        raw: String,
    },
    Timeout,
}

pub fn record_utterance(
    cfg: &AlwaysConfig,
    log: &mut Logger,
    transcriber: &Arc<dyn Transcriber>,
    rt: &tokio::runtime::Handle,
) -> Result<RecordResult> {
    record_with_local_vad(cfg, log, transcriber, rt)
}

/// Single-frame energy probe used while idle-paused so the user can wake
/// listening by speaking without manually lifting pause.
#[cfg(feature = "macos")]
pub fn poll_speech_energy(cfg: &AlwaysConfig) -> Result<bool> {
    let recorder_arc = audio::RecChild::get_or_spawn()?;
    let mut frame_buf = [0u8; FRAME_BYTES];
    // INVARIANT (concurrency): `read_frame` blocks under GLOBAL_RECORDER (see
    // the matching note in `record_with_local_vad`). This idle wake-on-voice
    // poll must run on the same single thread as `record_utterance`; running
    // them concurrently risks serializing on — or, if `rec` wedges, deadlocking
    // behind — the global recorder lock.
    let read = {
        let mut recorder = recorder_arc.lock();
        let rec = recorder
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Audio recorder not available"))?;
        rec.read_frame(&mut frame_buf)?
    };
    if read < FRAME_BYTES {
        return Ok(false);
    }
    let mut sample_buf = [0i16; FRAME_SAMPLES];
    for (i, chunk) in frame_buf.chunks_exact(2).enumerate() {
        sample_buf[i] = i16::from_le_bytes([chunk[0], chunk[1]]);
    }
    let energy = normalized_energy(&sample_buf[..]);
    Ok(energy >= voice_activity_energy_threshold(cfg))
}

/// Non-macOS stub.
#[cfg(not(feature = "macos"))]
pub fn poll_speech_energy(_cfg: &AlwaysConfig) -> Result<bool> {
    Err(anyhow::anyhow!("Audio capture not supported on this platform"))
}

/// Speech shorter than this is treated as a "short utterance" and gets
/// the aggressive silence cutoff below. Tuned tight — only TRULY tiny
/// single-word commands ("yes", "no", "done", "stop", "next") should
/// trip the fast path. Earlier values (600ms) let normal opener phrases
/// like "So, ..." or "And, ..." into the short bucket and got cut off
/// mid-thought.
const SHORT_SPEECH_MS: u32 = 400;
/// Silence-after-speech window for short utterances. Standard window
/// is `cfg.silence_secs`; for a short utterance we cut at 200ms so
/// single words ("yes", "ok") paste extremely fast.
/// Mid-sentence safety is covered by the higher-level `silence_secs` which
/// catches longer phrases once the utterance grows beyond `SHORT_SPEECH_MS`.
const SHORT_SILENCE_MS: u32 = 200;
/// Safety floor only. The configured `silence_secs` is validated once in
/// config (`SILENCE_SECS_MIN..=SILENCE_SECS_MAX`) and trusted here. The
/// old `NORMAL_SILENCE_CAP_SECS = 0.50` silently overrode every user
/// setting above half a second — combined with config's old 0.7 floor it
/// pinned the window to exactly 0.5s and made mid-speech cutoffs
/// untunable. Responsiveness is preserved by speculative STT, which
/// kicks off at 1/3 of the window regardless of its length.
const NORMAL_SILENCE_FLOOR_SECS: f64 = 0.30;
/// Lowered 0.70 → 0.60 for a snappier overlay: the user perceived the
/// activity-only overlay as slow to appear. Misfires are cheap — the
/// false-start retraction below (150ms window) pulls the announcement
/// back before any transcription work starts.
const EARLY_VOICE_ENERGY_RATIO: f64 = 0.60;
const EARLY_VOICE_FALSE_START_MS: u32 = 150;
/// Consecutive qualifying frames required before the early-voice
/// announcement fires. A single 30ms frame was enough before, and
/// keyboard clacks / door thumps / mouse clicks are near-always exactly
/// one frame of combined energy+Silero spike — they tripped the overlay
/// on noise that never became speech. Real speech onset sustains both
/// gates across frames, so requiring 2 (60ms total) kills transient
/// false positives at an imperceptible latency cost.
const EARLY_VOICE_MIN_FRAMES: usize = 2;
/// Keep-alive cadence for `VoiceActivityDetected` re-emits while an
/// utterance is live. The GUI arms a stale-overlay watchdog (6s lease)
/// on every detected event; the heartbeat keeps the lease fresh during
/// long utterances so a lost terminal event can never strand the
/// "Listening" overlay for more than one lease window.
const VOICE_HEARTBEAT_MS: u64 = 2000;
/// One-shot overlay warning when continuous speech reaches this many
/// seconds, so the hard cap never surprises anyone mid-thought. 25 min —
/// chunking transcribes as it goes, so only the absolute cap remains.
const LONG_RECORDING_WARN_SECS: u32 = 1500;
/// Continuous-speech hard cap (mirrors `max_speech_frames` below).
/// 30 minutes: an absolute runaway-recording safety only. Chunking (see
/// `chunker.rs`) removes the old 5-minute single-request cliff — audio is
/// flushed to STT every CHUNK_TARGET_SECS at a natural pause, so nothing
/// accumulates toward an API payload limit.
const MAX_SPEECH_SECS: u32 = 1800;
/// Flush the live buffer as a committed chunk at the next tentative
/// silence once it holds at least this much speech. 90s ≈ 2.9MB WAV —
/// far below Groq's 25MB cap, big enough that chunk seams are rare.
const CHUNK_TARGET_SECS: u32 = 90;

/// `CHUNK_TARGET_SECS` with a test override: `ALWAYS_CHUNK_TARGET_SECS`
/// lets an end-to-end test exercise the chunk path with seconds of audio
/// instead of minutes. Floor of 3s so a typo can't flush every frame.
fn chunk_target_secs() -> u32 {
    std::env::var("ALWAYS_CHUNK_TARGET_SECS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .map(|v| v.max(3))
        .unwrap_or(CHUNK_TARGET_SECS)
}
/// Absolute per-chunk ceiling: flush at a frame boundary even mid-speech
/// if the user talks continuously for this long without a 0.2s dip.
/// Whisper tolerates a clean-frame cut; 2.5 min of literally pause-free
/// speech is vanishingly rare.
const CHUNK_HARD_MAX_SECS: u32 = 150;
/// Adaptive mid-sentence extension: when the speculative transcript ends
/// mid-thought (no terminator, or a trailing connector word), the final
/// silence window is stretched by this factor so a thinking pause doesn't
/// split one sentence into two pastes.
const MIDSENTENCE_EXTENSION_FACTOR: f64 = 2.0;
/// Absolute ceiling on the extra quiet the extension may add, so a large
/// user-configured window doesn't double into something absurd.
const MIDSENTENCE_MAX_EXTRA_SECS: f64 = 1.5;
/// Decision grace past the base silence window while the speculative STT
/// result is still in flight. The paste can't happen before STT completes
/// anyway (the post-loop wait blocks on the same result), so holding the
/// cut for up to this many frames costs ~zero user-visible latency — it
/// just moves the wait INSIDE the loop where the mid-sentence decision
/// can still extend the window. 20 frames = 600ms: observed Groq
/// kickoff→result times range ~0.7-1.0s, so 300ms of grace lost the race
/// whenever Groq was on the slow side of that band.
const MIDSENTENCE_DECISION_GRACE_FRAMES: usize = 20;
/// Trailing words that mark a clause as clearly unfinished even when
/// Whisper appended its habitual period. Lowercase, punctuation-stripped.
const TRAILING_CONNECTORS: &[&str] = &[
    "and", "but", "or", "so", "because", "which", "that", "to", "the", "a", "an", "with", "for",
    "of", "in", "on", "at", "by",
];

#[cfg(feature = "macos")]
fn record_with_local_vad(
    cfg: &AlwaysConfig,
    log: &mut Logger,
    transcriber: &Arc<dyn Transcriber>,
    rt: &tokio::runtime::Handle,
) -> Result<RecordResult> {
    let silence_frames = normal_silence_frames(cfg);
    // Two distinct timeouts:
    // - `max_pre_voice_frames`: hard cap on the initial wait for voice
    //   onset. Uses `cfg.timeout_secs` (default 30s) so a stale recorder
    //   that never produces a voiced frame can't run forever.
    // - `max_speech_frames`: cap on continuous speech once voice is
    //   detected. 5 minutes is generous enough for long monologues
    //   (the original 30s cap was mid-sentence-cutting users who gave
    //   long prompts — observed split at the 28s mark).
    let max_pre_voice_frames = (cfg.timeout_secs as usize * 1000) / FRAME_MS as usize;
    let max_speech_frames = (MAX_SPEECH_SECS as usize * 1000) / FRAME_MS as usize;
    let min_speech_frames = (cfg.onset_ms / FRAME_MS).max(1) as usize;
    let voice_activity_energy_threshold = voice_activity_energy_threshold(cfg);
    let early_voice_energy_threshold =
        (voice_activity_energy_threshold * EARLY_VOICE_ENERGY_RATIO).max(cfg.hear_energy_threshold);
    let early_voice_false_start_frames =
        ((EARLY_VOICE_FALSE_START_MS as f64) / FRAME_MS as f64).ceil() as usize;
    // Short-utterance cutoff. Computed once; activated per-iteration
    // when `speech_samples` duration is still under SHORT_SPEECH_MS.
    let short_silence_frames = ((SHORT_SILENCE_MS as f64) / FRAME_MS as f64).ceil() as usize;
    // Tentative kickoff at 50% of the short window — earlier than the
    // 85% used for normal utterances because the speech itself is so
    // brief that even a misfired speculation is cheap to retry.
    let short_tentative_frames = (short_silence_frames / 2).max(1);
    // Two-stage end-of-utterance detection (Option B):
    // - At `tentative_silence_frames`, kick off a SPECULATIVE transcription in a
    //   background thread using the audio captured so far.
    // - Continue recording until `silence_frames` (final).
    // - If speech resumes during the tentative window, discard the speculation.
    // - At final, if speculation is still valid (no resume), use its result —
    //   transcription has been running in parallel during the silence wait, so
    //   the user gets snappy paste with no extra latency cost.
    // Tentative at 20% of final window — starts Whisper while the user
    // is still in the trailing-silence wait, but we still require the
    // full `silence_secs` of quiet before finalizing. Wrong guesses are
    // discarded if speech resumes. This only moves the preview/STT kickoff;
    // it does not shorten the hard final silence limit.
    let tentative_silence_frames = tentative_silence_frames(silence_frames);
    // Pre-buffer: keep 200ms of audio before speech detection to catch first words.
    // 50ms was too short — first syllable of "Run the program" was dropped. 200ms
    // gives enough headroom for VAD onset latency without bloating speech_samples.
    let pre_buffer_frames = (200 / FRAME_MS as usize).max(1);
    // Silero VAD via vad-rs. Frame size is locked to FRAME_SAMPLES (480 = 30ms @ 16kHz);
    // the wrapper enforces this and converts i16 → f32 internally.
    let vad = SileroVad::new().context("failed to load Silero VAD")?;
    let speech_threshold: f32 = cfg.silero_threshold;
    // Hysteresis: easier to STAY in speech than to ENTER it.
    // Silero's probability naturally dips below 0.5 during voiceless consonants
    // (h, s, f), inter-syllable pauses, and quiet syllables — without hysteresis,
    // these brief dips accumulate consecutive_silence and prematurely cut the
    // utterance mid-sentence. Reverted from 0.60 → 0.75 for snappier
    // end-of-speech: the previous 0.60 kept utterances alive through long
    // ambient/breath windows, making the daemon feel slow to start
    // transcribing. Mid-sentence "soft trailing" cases ('s', 'f', 'h')
    // are still covered by the smoothing window below.
    let silence_threshold: f32 = speech_threshold * 0.75;
    let mut last_prob: f32 = 0.0;
    // Probability smoothing window. Silero outputs per-512-sample (32ms) chunks;
    // single-frame dips during voiceless consonants (s/f/th/h) or breaths can
    // briefly drop prob below threshold without genuine end-of-utterance.
    // We track the last 10 readings (~300ms) and use the running max while in
    // speech — the prob must stay low for the FULL window to count as silence.
    // History: 8 frames cut mid-phrase; 16 still split soft trails; 24 was
    // safe but slow. Option-held pause is now the explicit long-break control,
    // so normal smoothing can stay tight without forcing every utterance to
    // wait behind a long tail.
    let smoothing_window: usize = 10;
    let mut prob_history: VecDeque<f32> = VecDeque::with_capacity(smoothing_window);

    // Use persistent recorder to avoid process spawning overhead
    let recorder_arc = audio::RecChild::get_or_spawn()?;
    let mut frame_buf = [0u8; FRAME_BYTES];
    // Pre-allocate ~4 seconds of audio (16000 Hz * 4s = 64000 samples) to avoid Vec growth
    let mut speech_samples: Vec<i16> = Vec::with_capacity(64_000);
    let mut consecutive_silence = 0usize;
    let mut consecutive_speech = 0usize;
    let mut tentative_voice_silence = 0usize;
    let mut in_speech = false;
    let mut voice_logged = false;
    let mut voice_activity_announced = false;
    let mut voice_activity_announced_at: Option<std::time::Instant> = None;
    let mut last_voice_heartbeat: Option<std::time::Instant> = None;
    let mut early_voice_streak = 0usize;
    let mut long_recording_warned = false;
    let mut total_frames = 0usize;
    let mut pre_buffer: VecDeque<Vec<i16>> = VecDeque::with_capacity(pre_buffer_frames);

    // Single source of truth for the overlay state. Flips to Transcribing
    // at speculative STT kickoff (~60% of silence_secs), not on a early
    // energy dip — that mismatch made paste feel ~2s slower than the UI.
    // Final cut also guarantees the state. Resume mid-silence flips back.
    // (`allow(unused_assignments)` covers terminal flips before return.)
    let mut transcribing_overlay = false;
    // Helper: flip overlay to transcribing if not already.
    // The `#[allow(unused_assignments)]` covers terminal flips that
    // happen on the final iteration of the loop: the assignment is
    // observable by *subsequent* macro calls (which is exactly what
    // makes it correct), but on the last invocation before `return`
    // there's no subsequent reader, so the lint fires at the
    // expansion site. Scoping the allow into the macro body keeps
    // the surrounding code under the project-wide `-D warnings` gate.
    macro_rules! flip_to_transcribing {
        () => {{
            if !transcribing_overlay {
                #[allow(unused_assignments)]
                {
                    transcribing_overlay = true;
                }
                event::global_broadcaster().transcribing_started();
            }
        }};
    }
    macro_rules! flip_to_listening {
        () => {{
            if transcribing_overlay {
                #[allow(unused_assignments)]
                {
                    transcribing_overlay = false;
                }
                event::global_broadcaster().transcribing_stopped();
            }
        }};
    }
    macro_rules! announce_voice_activity {
        () => {{
            if !voice_activity_announced {
                voice_activity_announced = true;
                voice_activity_announced_at = Some(std::time::Instant::now());
                #[allow(unused_assignments)]
                {
                    last_voice_heartbeat = Some(std::time::Instant::now());
                }
                event::global_broadcaster().voice_activity_detected();
            }
        }};
    }

    // Optimization for very low energy thresholds (≤ 0.01): use fast energy check
    let use_fast_energy_check = cfg.energy_threshold <= 0.01;
    let fast_energy_threshold = voice_activity_energy_threshold;

    // For very low thresholds, we can use a simplified RMS calculation
    // that's much faster than the full normalized energy calculation
    let fast_energy_threshold_sq = if use_fast_energy_check {
        (fast_energy_threshold * 32768.0).powi(2) as i64
    } else {
        0
    };

    // Reusable per-frame sample buffer (stack allocated, no heap allocation per frame)
    let mut sample_buf = [0i16; FRAME_SAMPLES];

    // Speculation state (Option B): kicked off at tentative silence, may be
    // discarded if speech resumes before final silence.
    let speculation_slot = SpeculationSlot::new();
    let mut speculation_pending = false;
    // Adaptive mid-sentence extension: latched once per silence run when
    // the speculative transcript looks unfinished; reset on speech resume.
    // `decided` records that the speculative text was inspected (either
    // verdict) so the decision grace stops holding the cut.
    let mut midsentence_extended = false;
    let mut midsentence_decided = false;
    // Rolling chunk transcription for long dictations (see chunker.rs).
    // `committed_samples` tracks audio already flushed out of the live
    // buffer so the long-recording warn/cap math still sees the total.
    // `voiced_since_flush` gates the speculation kickoff: after a chunk
    // drain the live buffer holds only trailing silence, which is not
    // worth a speculative STT round trip.
    let mut chunker = crate::always::chunker::ChunkAccumulator::new();
    let mut committed_samples = 0usize;
    let mut voiced_since_flush = true;

    loop {
        // INVARIANT (concurrency): `read_frame` blocks on rec's stdout for up
        // to one full frame while holding GLOBAL_RECORDER. `record_utterance`
        // and `poll_speech_energy` therefore MUST NOT run concurrently on
        // different threads — they would serialize on this lock a full frame
        // each, and a wedged `rec` (neither bytes nor EOF) would hold the lock
        // indefinitely and deadlock every other audio caller. The live
        // pipeline upholds this by driving both from the single event-loop
        // thread; a future multi-reader design needs a single owning reader or
        // a read timeout before this invariant can be relaxed.
        let read = {
            let mut recorder = recorder_arc.lock();
            let Some(rec) = recorder.as_mut() else {
                return Err(anyhow::anyhow!("Audio recorder not available"));
            };
            match rec.read_frame(&mut frame_buf) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    // Wedged recorder (alive but producing neither bytes
                    // nor EOF): evict it like the EOF case below — Drop
                    // kills and reaps the child — so the next capture
                    // cycle respawns fresh instead of wedging forever.
                    // Any partial audio flows through the post-loop path.
                    if recorder.take().is_some() {
                        tracing::warn!("rec_timeout_recorder_reset");
                    }
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        };

        if read == 0 {
            // True EOF on rec's stdout: the recorder died (USB
            // re-enumeration, mic unplug, TCC revoke, or `rec` exit).
            // `read_frame` already logged `rec_eof_on_read_frame`. The
            // danger is the *dead* RecChild lingering in the global slot:
            // until the next `get_or_spawn` notices it via `try_wait`, every
            // caller keeps reading immediate EOF from the corpse, so a
            // transient re-enumeration looks like permanent death. Evict it
            // now — `take()` drops the RecChild, whose Drop impl `kill()`s and
            // `wait()`s the child — so the NEXT capture cycle's `get_or_spawn`
            // respawns a fresh recorder and capture recovers. We deliberately
            // do NOT attempt an in-loop respawn/retry of the current utterance:
            // the safe, well-contained fix is to guarantee the dead child can't
            // wedge subsequent cycles. Any partial audio captured so far flows
            // through the post-loop path below exactly as on any other break.
            {
                let mut recorder = recorder_arc.lock();
                if recorder.take().is_some() {
                    tracing::warn!("rec_eof_recorder_reset");
                }
            }
            break;
        }

        if read < FRAME_BYTES {
            // Partial frame (0 < read < FRAME_BYTES) at shutdown/disconnect:
            // preserve the existing behavior of ending the utterance. The
            // recorder is not necessarily dead here (a short read can precede
            // a clean EOF on the next iteration), so we do not evict it.
            break;
        }

        for (i, chunk) in frame_buf.chunks_exact(2).enumerate() {
            sample_buf[i] = i16::from_le_bytes([chunk[0], chunk[1]]);
        }
        let samples = &sample_buf[..];

        // One inference per 30ms frame. vad-rs requires exactly 480 samples
        // (which is what `samples` always is here — see FRAME_SAMPLES). A
        // transient inference error keeps the previous probability rather
        // than mistakenly resetting to 0 and dropping a live utterance.
        if let Ok(prob) = vad.predict(samples) {
            last_prob = prob;
            if prob_history.len() >= smoothing_window {
                prob_history.pop_front();
            }
            prob_history.push_back(last_prob);
        }
        // Smoothed probability for end-of-speech check: use the MAX of recent
        // window so a single-frame dip does not register as silence. Brief
        // consonant gaps (s/f/th/h, ~30-100ms) leave the window max above
        // silence_threshold, preventing premature cutoff.
        let smoothed_max = prob_history
            .iter()
            .copied()
            .fold(0.0f32, f32::max)
            .max(last_prob);
        // Hysteresis: while already in speech, exit only when BOTH Silero's
        // smoothed max AND raw energy say silence; while not yet in speech,
        // require the raw higher Silero threshold to enter (faster onset).
        //
        // The energy fallback is the load-bearing fix for mid-speech cuts:
        // Silero misfires on soft trailing syllables, accent-edge phonemes,
        // and quiet voiced segments — it drops its probability for hundreds
        // of milliseconds while the user is genuinely still talking. Pure
        // Silero+smoothing can't recover from this because the max stays low
        // for the full window. Anchoring on raw energy means: as long as the
        // microphone is picking up sound above the user's configured energy
        // floor, the utterance stays open regardless of Silero's opinion.
        let frame_energy = if use_fast_energy_check {
            fast_normalized_energy(samples)
        } else {
            normalized_energy(samples)
        };
        // Use 2.0x energy_threshold for the in-speech keepalive: 1.5x let
        // HVAC/keyboard spikes keep `consecutive_silence` from advancing,
        // stretching the post-speech wait. 2.0x is still below normal speech
        // RMS (~0.04-0.10) but above typical ambient (~0.003-0.010).
        let energy_above_threshold = frame_energy >= cfg.energy_threshold * 2.0;
        let is_speech = if in_speech {
            smoothed_max >= silence_threshold || energy_above_threshold
        } else {
            last_prob >= speech_threshold
                || (last_prob >= speech_threshold * 0.65
                    && frame_energy >= cfg.energy_threshold * 1.1)
        };
        // Early-voice announce: a per-frame gate plus a short streak.
        // The gate ratios (0.60 energy / 0.45 Silero) are deliberately
        // loose for snappiness; the streak is the transient
        // discriminator (see EARLY_VOICE_MIN_FRAMES) and the 150ms
        // retraction below covers anything that still slips through.
        if !in_speech && !voice_activity_announced {
            if early_voice_frame_ok(
                frame_energy,
                last_prob,
                early_voice_energy_threshold,
                speech_threshold,
            ) {
                early_voice_streak += 1;
            } else {
                early_voice_streak = 0;
            }
            if early_voice_streak >= EARLY_VOICE_MIN_FRAMES {
                announce_voice_activity!();
                tentative_voice_silence = 0;
                early_voice_streak = 0;
            }
        }

        // Lease heartbeat: re-emit VoiceActivityDetected every 2s while
        // the announcement is live so the GUI watchdog never expires
        // mid-utterance (see VOICE_HEARTBEAT_MS).
        if voice_activity_announced
            && last_voice_heartbeat.is_some_and(|t| {
                t.elapsed() >= std::time::Duration::from_millis(VOICE_HEARTBEAT_MS)
            })
        {
            last_voice_heartbeat = Some(std::time::Instant::now());
            event::global_broadcaster().voice_activity_detected();
        }

        if is_speech {
            tentative_voice_silence = 0;
            // Speech resumed: discard any pending speculation (its audio snapshot
            // is now stale because more speech will be appended).
            if speculation_pending {
                speculation_pending = false;
                speculation_slot.invalidate();
                // Overlay was likely flipped by speculation kickoff.
                // User resumed — flip back. (No-op if already listening.)
                flip_to_listening!();
            }
            midsentence_extended = false;
            midsentence_decided = false;
            consecutive_speech += 1;
            consecutive_silence = 0;
            if consecutive_speech >= min_speech_frames {
                in_speech = true;
                if !voice_logged {
                    // Fast energy check for very low thresholds to minimize latency
                    let passes_energy_check = if use_fast_energy_check {
                        fast_energy_check(samples, fast_energy_threshold_sq)
                    } else {
                        let current_energy = normalized_energy(samples);
                        current_energy >= voice_activity_energy_threshold
                    };

                    if passes_energy_check {
                        log.write(Event::VoiceDetected);
                        voice_logged = true;

                        // Warn if energy is barely above threshold (within 20% margin)
                        // Use the already-calculated frame_energy to avoid redundant computation
                        if frame_energy < cfg.energy_threshold * 1.2 {
                            event::global_broadcaster().low_microphone_volume_maybe(frame_energy);
                        }

                        // Send voice activity detected event
                        announce_voice_activity!();
                        // Clear the idle-auto-paused flag the moment we
                        // see voice. Upstream calls `mark_voice_seen()`
                        // unconditionally below (every confirmed speech
                        // frame), so we only need to drop the idle flag
                        // here — that's the one piece upstream didn't
                        // wire and that was leaving us stuck in idle-pause
                        // after the audio-output auto-resume path.
                        if pause::is_idle_auto_paused() {
                            pause::set_idle_auto_paused(false);
                            let (effective, changed) = pause::recompute_effective();
                            if changed && !effective {
                                event::global_broadcaster().idle_auto_resumed();
                                event::global_broadcaster().resumed();
                            }
                        }
                        // Cancel any in-flight auto-enter countdown — the
                        // user is clearly still speaking. The dictation
                        // buffer is intentionally NOT cleared (cancel
                        // path in auto_enter_countdown leaves it), so
                        // when this fresh utterance finalises the
                        // dictation-merge gate in `handle_speech` picks
                        // up the previous text and appends. Without this
                        // cancel, a Return would fire mid-sentence and
                        // split the user's utterance in two.
                        if pause::countdown_active() {
                            pause::countdown_request_cancel();
                            tracing::info!("countdown_cancel_on_voice_resume");
                        }
                    }
                }
                // Anchor the idle-pause watchdog: refresh on EVERY confirmed
                // speech frame so a long continuous utterance (>idle threshold)
                // never trips a false idle-auto-pause. Earlier code called this
                // exactly once per utterance start, so a user speaking
                // continuously for >idle_pause_secs would get auto-paused
                // mid-sentence even though there was no real silence gap.
                pause::mark_voice_seen();
                // Prepend pre-buffer to capture audio before VAD triggered
                for buffered_samples in pre_buffer.drain(..) {
                    speech_samples.extend_from_slice(&buffered_samples);
                }
            }
            if in_speech {
                speech_samples.extend_from_slice(samples);
                voiced_since_flush = true;
                // Same anchor refresh for the case where we were already in
                // speech (consecutive_speech overflows min_speech_frames
                // immediately) — keep the watchdog clock pinned to "right
                // now" for as long as audio is genuinely voice.
                pause::mark_voice_seen();

                // Per-chunk ceiling: a pause-free monologue never reaches
                // the tentative-silence flush point, so cut at a frame
                // boundary once the chunk is oversized.
                if speech_samples.len() >= CHUNK_HARD_MAX_SECS as usize * 16_000 {
                    tracing::info!(
                        chunk_secs = speech_samples.len() / 16_000,
                        "chunk_hard_flush"
                    );
                    committed_samples += speech_samples.len();
                    let grammar = if cfg.postprocess_config.grammar_correction_enabled {
                        cfg.post_processor.clone()
                    } else {
                        None
                    };
                    chunker.flush(
                        std::mem::take(&mut speech_samples),
                        transcriber,
                        grammar,
                        rt,
                    );
                    speculation_pending = false;
                    speculation_slot.invalidate();
                    voiced_since_flush = false;
                }
            }
        } else if in_speech {
            if keyboard::is_option_held() {
                consecutive_silence = 0;
                consecutive_speech = 0;
                speech_samples.extend_from_slice(samples);
                pause::mark_voice_seen();
                continue;
            }
            consecutive_silence += 1;
            consecutive_speech = 0;
            speech_samples.extend_from_slice(samples);

            // Short-utterance fast path. Speech duration is just
            // samples / 16 (16kHz mono). While we're still under
            // SHORT_SPEECH_MS, use the aggressive cutoff. If speech
            // resumes and the total grows past the threshold, we
            // automatically fall back to the standard window on the
            // next iteration. No state, no commitment.
            let speech_ms = (speech_samples.len() as u32) / 16;
            let is_short = speech_ms < SHORT_SPEECH_MS;
            let eff_silence_frames = if is_short {
                short_silence_frames
            } else {
                silence_frames
            };
            let eff_tentative_frames = if is_short {
                short_tentative_frames
            } else {
                tentative_silence_frames
            };

            // At tentative silence with a target-sized buffer, COMMIT the
            // chunk instead of speculating: flush it for background
            // transcription (never discarded — see chunker.rs) and keep
            // recording into a fresh buffer. The seam lands inside a
            // silence region at a frame boundary, so no word is split.
            if voice_logged
                && consecutive_silence >= eff_tentative_frames
                && speech_samples.len() >= chunk_target_secs() as usize * 16_000
            {
                committed_samples += speech_samples.len();
                let grammar = if cfg.postprocess_config.grammar_correction_enabled {
                    cfg.post_processor.clone()
                } else {
                    None
                };
                chunker.flush(
                    std::mem::take(&mut speech_samples),
                    transcriber,
                    grammar,
                    rt,
                );
                speculation_pending = false;
                speculation_slot.invalidate();
                midsentence_extended = false;
                midsentence_decided = false;
                voiced_since_flush = false;
            }

            // At tentative silence, kick off speculative transcription in the
            // background so the result is ready (or nearly so) by the time we
            // hit final silence. If the user resumes, we discard it above.
            // Skipped after a chunk drain until real speech lands in the
            // fresh buffer — trailing silence isn't worth an STT round trip.
            if voice_logged
                && voiced_since_flush
                && !speculation_pending
                && consecutive_silence >= eff_tentative_frames
            {
                speculation_pending = true;
                speculation_slot.invalidate();
                let captured_gen = speculation_slot.current_generation();
                // Overlay → Transcribing when speculative STT starts.
                flip_to_transcribing!();
                let audio_snapshot = speech_samples.clone();
                let transcriber_for_spec = Arc::clone(transcriber);
                let slot = Arc::clone(&speculation_slot);
                // For the speculative grammar warm below: the post-processor
                // and runtime handle must be owned by the thread (the `cfg`
                // borrow can't cross it).
                let grammar_warm = if cfg.postprocess_config.grammar_correction_enabled {
                    cfg.post_processor.clone()
                } else {
                    None
                };
                let rt_for_warm = rt.clone();
                std::thread::spawn(move || {
                    // Wrap in catch_unwind: a panic in transcribe_from_bytes
                    // (network, deserialization, etc.) used to poison
                    // std::sync::Mutex and stall the next utterance. With
                    // parking_lot the lock can't poison, but we still want
                    // the slot populated with an Err so the main loop sees
                    // the failure instead of timing out.
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                        || -> Result<crate::stt::TranscriptionResult> {
                            let wav = audio::create_wav_bytes_i16_mono_16k(&audio_snapshot)?;
                            transcriber_for_spec
                                .transcribe_from_bytes(wav)
                                .map_err(anyhow::Error::from)
                        },
                    ))
                    .unwrap_or_else(|panic_payload| {
                        let msg = panic_payload
                            .downcast_ref::<&'static str>()
                            .copied()
                            .or_else(|| panic_payload.downcast_ref::<String>().map(String::as_str))
                            .unwrap_or("speculation thread panicked");
                        tracing::error!(panic = %msg, "speculation transcription panicked");
                        Err(anyhow::anyhow!("speculation transcription panicked: {msg}"))
                    });
                    // Emit the speculative text as a TranscriptChunk so the
                    // overlay can show a streaming preview while we wait for
                    // final silence. If the user keeps talking, the final
                    // transcription will supersede this.
                    if let Ok(ref r) = outcome
                        && !r.text.is_empty()
                    {
                        event::global_broadcaster().transcript_chunk(r.text.clone());
                    }
                    let warm_text = match &outcome {
                        Ok(r) if !r.text.is_empty() => Some(r.text.clone()),
                        _ => None,
                    };
                    // Store the STT result FIRST — the paste path must never
                    // wait on the grammar warm.
                    slot.store_if_current(captured_gen, outcome);
                    // Speculative grammar warm: when this STT result survives
                    // to final silence (the common case), the paste path asks
                    // grammar for the exact same text — starting the LLM call
                    // now hides its ~600ms inside the remaining silence wait.
                    // The single-flight cell in PostProcessor dedupes if the
                    // paste path arrives while this is still in flight. If
                    // the user resumed speaking, the final text differs and
                    // this warm is a wasted-but-cached call.
                    if let Some(text) = warm_text
                        && captured_gen == slot.current_generation()
                        && !crate::always::event_loop::is_short_utterance(&text)
                        && let Some(pp) = grammar_warm
                    {
                        // Build through the SAME request builder as the
                        // paste path — identical tiering, candidates, and
                        // session context mean an identical cache key, so
                        // the paste path's grammar call is a cache hit.
                        let req = crate::always::correction_request::build(&text, pp.can_correct());
                        rt_for_warm.spawn(async move {
                            let _ = pp.process_request(&req).await;
                        });
                    }
                });
            }

            // Adaptive mid-sentence extension: once the speculative
            // outcome lands, inspect the text (non-consuming; every 2nd
            // frame to keep the hot loop cheap) and stretch the final
            // window when it ends mid-thought. Decided at most once per
            // silence run; both flags reset on speech resume.
            let adaptive_active = cfg.adaptive_silence_enabled && !is_short && speculation_pending;
            if adaptive_active
                && !midsentence_decided
                && consecutive_silence % 2 == 0
                && speculation_slot.peek_ready()
            {
                midsentence_decided = true;
                if let Some(spec_text) = speculation_slot.peek_text()
                    && looks_mid_sentence(&cfg.localization, &spec_text)
                {
                    midsentence_extended = true;
                }
                // One line either way so production logs show how often
                // the heuristic fires vs. passes.
                tracing::info!(
                    extended = midsentence_extended,
                    decided_at_frame = consecutive_silence,
                    base_frames = eff_silence_frames,
                    extended_frames = extended_silence_frames(silence_frames),
                    "midsentence_decision"
                );
            }

            let eff_final_frames = if midsentence_extended {
                extended_silence_frames(silence_frames)
            } else if adaptive_active && !midsentence_decided {
                // Speculative STT still in flight: hold the cut briefly so
                // the decision above can happen. Costs ~no paste latency —
                // the post-loop wait would block on the same result.
                eff_silence_frames + MIDSENTENCE_DECISION_GRACE_FRAMES
            } else {
                eff_silence_frames
            };
            if consecutive_silence >= eff_final_frames {
                break;
            }
        } else {
            consecutive_speech = 0;
            if voice_activity_announced && !voice_logged {
                tentative_voice_silence += 1;
                let enough_wall_time = voice_activity_announced_at
                    .map(|started| {
                        started.elapsed()
                            >= std::time::Duration::from_millis(EARLY_VOICE_FALSE_START_MS as u64)
                    })
                    .unwrap_or(false);
                if enough_wall_time && tentative_voice_silence >= early_voice_false_start_frames {
                    voice_activity_announced = false;
                    voice_activity_announced_at = None;
                    tentative_voice_silence = 0;
                    event::global_broadcaster().voice_activity_ended();
                }
            }
            // Maintain pre-buffer: add new frame, drop oldest if full
            pre_buffer.push_back(samples.to_vec());
            if pre_buffer.len() > pre_buffer_frames {
                pre_buffer.pop_front();
            }
        }

        total_frames += 1;
        // One-shot heads-up well before the hard cap cuts the recording.
        // Includes chunk-committed audio — the live buffer resets per chunk.
        if in_speech && !long_recording_warned {
            let speech_secs = ((committed_samples + speech_samples.len()) / 16_000) as u32;
            if speech_secs >= LONG_RECORDING_WARN_SECS {
                long_recording_warned = true;
                tracing::info!(
                    speech_secs,
                    cap_secs = MAX_SPEECH_SECS,
                    "long_recording_warning"
                );
                event::global_broadcaster().long_recording_warning(speech_secs, MAX_SPEECH_SECS);
            }
        }
        // Two-tier timeout: pre-voice frames get the short cap so a
        // dead recorder bails fast; in-speech frames get the long cap
        // so a user mid-monologue doesn't get sliced in half.
        let effective_max = if in_speech {
            max_speech_frames
        } else {
            max_pre_voice_frames
        };
        if total_frames >= effective_max {
            break;
        }
    }

    // "Speech end" for latency accounting: the loop exit is within one
    // 30ms frame of the final-silence cut firing.
    let speech_end_at = std::time::Instant::now();

    let has_chunks = !chunker.is_empty();

    if speech_samples.is_empty() && !has_chunks {
        // Send voice activity ended event if no speech was detected
        event::global_broadcaster().voice_activity_ended();
        // Don't set listening to false here - let it time out naturally
        // We bailed without ever entering speech, so only the pre-voice
        // cap matters here (speech cap can't have fired — `in_speech`
        // was never true).
        return if total_frames >= max_pre_voice_frames {
            Ok(RecordResult::Timeout)
        } else {
            Ok(RecordResult::Silence)
        };
    }

    // Final energy check using appropriate method based on threshold
    let speech_energy = if speech_samples.is_empty() {
        0.0
    } else if use_fast_energy_check {
        fast_normalized_energy(&speech_samples)
    } else {
        normalized_energy(&speech_samples)
    };

    // Only log drops if we actually logged voice detection. With committed
    // chunks in flight this gate applies to the TAIL only (it decides
    // whether the tail is worth transcribing) — a whisper-quiet ending
    // must never discard ten minutes of already-committed speech.
    let tail_has_voice = speech_energy >= cfg.energy_threshold;
    if !has_chunks && !tail_has_voice {
        event::global_broadcaster().voice_activity_ended();
        // Predictive overlay may have flipped to Transcribing during the
        // silence wait — there's no transcription coming on the dropped
        // path, so unflip so the badge doesn't lie.
        flip_to_listening!();
        // Don't set listening to false here - let it time out naturally
        if voice_logged {
            // Only log dropped energy if we previously logged voice detected
            return Ok(RecordResult::DroppedLowEnergy {
                energy: speech_energy,
            });
        } else {
            // Silent drop - we never logged voice detected, so just return silence
            return Ok(RecordResult::Silence);
        }
    }

    // Guarantee Transcribing overlay (speculation usually already flipped).
    flip_to_transcribing!();

    // Chunked utterance whose tail is pure trailing silence: nothing to
    // transcribe here — assemble the committed chunks directly.
    if has_chunks && !tail_has_voice {
        return finalize_chunked(
            &chunker,
            transcriber,
            String::new(),
            &crate::stt::TranscriptionResult::default(),
            committed_samples,
            speech_energy.max(cfg.energy_threshold),
            speech_end_at,
            false,
        );
    }

    // Try to use the speculative transcription if it was kicked off and
    // wasn't invalidated by a speech resume. Wait if it's still in
    // flight — it has been running for up to (silence_secs - tentative)
    // seconds already, so it's likely complete or close to it. The wait
    // cap scales with utterance length: a flat 10s meant a 2-minute
    // monologue routinely timed out, threw the near-done speculative
    // result away, and re-transcribed the ENTIRE audio from scratch —
    // doubling the worst wait exactly when it was already longest.
    let speculation = if speculation_pending {
        let started_wait = std::time::Instant::now();
        let audio_secs = speech_samples.len() as f64 / 16_000.0;
        let max_wait = std::time::Duration::from_secs_f64((audio_secs * 0.5).clamp(10.0, 60.0));
        let mut taken: Option<Result<crate::stt::TranscriptionResult>> = None;
        let mut last_heartbeat = std::time::Instant::now();
        loop {
            if let Some(r) = speculation_slot.take() {
                taken = Some(r);
                break;
            }
            if started_wait.elapsed() >= max_wait {
                break;
            }
            // Keep the GUI's transcribing lease fresh during long waits
            // (TranscribingStarted re-emits double as the heartbeat).
            if last_heartbeat.elapsed() >= std::time::Duration::from_secs(2) {
                last_heartbeat = std::time::Instant::now();
                event::global_broadcaster().transcribing_started();
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        taken
    } else {
        None
    };

    let (result, speculation_used) = match speculation {
        Some(Ok(r)) => {
            event::global_broadcaster().transcribing_stopped();
            (r, true)
        }
        _ => {
            // No speculation, speculation errored, or timeout — do a fresh
            // transcription with the full audio (including any trailing silence).
            let wav_data = match audio::create_wav_bytes_i16_mono_16k(&speech_samples) {
                Ok(data) => data,
                Err(err) => {
                    event::global_broadcaster().transcribing_stopped();
                    return Err(err).context("failed to create WAV data in memory");
                }
            };
            // The blocking transcription below can run tens of seconds
            // for long audio — heartbeat from a helper thread so the
            // GUI's transcribing lease can't expire mid-call.
            let heartbeat_stop = Arc::new(AtomicBool::new(false));
            let heartbeat_handle = {
                let stop = Arc::clone(&heartbeat_stop);
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_secs(2));
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        event::global_broadcaster().transcribing_started();
                    }
                })
            };
            let transcribed = transcriber.transcribe_from_bytes(wav_data);
            heartbeat_stop.store(true, Ordering::Relaxed);
            let _ = heartbeat_handle.join();
            match transcribed {
                Ok(result) => {
                    event::global_broadcaster().transcribing_stopped();
                    (result, false)
                }
                Err(err) => {
                    event::global_broadcaster().transcribing_stopped();
                    return Err(err).context("failed to transcribe utterance");
                }
            }
        }
    };
    let stt_done_at = std::time::Instant::now();

    let raw = result.text.clone();

    // Chunked utterance with a voiced tail: the noise filters below are
    // tuned to judge a whole short utterance and would here judge only
    // the tail — route to assembly instead.
    if has_chunks {
        return finalize_chunked(
            &chunker,
            transcriber,
            raw,
            &result,
            committed_samples + speech_samples.len(),
            speech_energy.max(cfg.energy_threshold),
            speech_end_at,
            speculation_used,
        );
    }

    // Enhanced noise detection: filter out empty, very short, or low-energy results
    if raw.is_empty() {
        return Ok(RecordResult::DroppedNoise { raw });
    }

    // Filter very short results that are likely noise (less than 2 characters)
    let raw_trimmed = raw.trim();
    if raw_trimmed.len() < 2 {
        // Privacy: gate transcript text even for filtered noise
        if crate::always::telemetry::should_log_transcripts() {
            tracing::debug!(text = %raw, "dropped_noise_too_short");
        } else {
            tracing::debug!(chars = raw.chars().count(), "dropped_noise_too_short");
        }
        return Ok(RecordResult::DroppedNoise { raw });
    }

    // Filter results with very low energy - likely background noise
    if speech_energy < cfg.energy_threshold * 0.5 {
        tracing::debug!(
            energy = speech_energy,
            threshold = cfg.energy_threshold,
            "dropped_noise_low_energy"
        );
        return Ok(RecordResult::DroppedNoise { raw });
    }

    Ok(RecordResult::Speech {
        text: raw,
        energy: speech_energy,
        transcription: result,
        timing: UtteranceTiming {
            speech_end_at,
            stt_done_at,
            speculation_used,
        },
    })
}

/// Non-macOS stub.
#[cfg(not(feature = "macos"))]
fn record_with_local_vad(
    _cfg: &AlwaysConfig,
    _log: &mut Logger,
    _transcriber: &Arc<dyn Transcriber>,
    _rt: &tokio::runtime::Handle,
) -> Result<RecordResult> {
    Err(anyhow::anyhow!("Audio capture not supported on this platform"))
}

/// Assemble a chunked utterance: wait for committed chunks, join them in
/// flush order, and append the (already transcribed) tail text.
#[cfg(feature = "macos")]
#[allow(clippy::too_many_arguments)]
fn finalize_chunked(
    chunker: &crate::always::chunker::ChunkAccumulator,
    transcriber: &Arc<dyn Transcriber>,
    tail_text: String,
    tail_result: &crate::stt::TranscriptionResult,
    total_samples: usize,
    energy: f64,
    speech_end_at: std::time::Instant,
    speculation_used: bool,
) -> Result<RecordResult> {
    let assembled = chunker.finalize(transcriber);
    event::global_broadcaster().transcribing_stopped();
    let tail = tail_text.trim();
    let mut full_text = assembled.text;
    if !tail.is_empty() {
        if full_text.is_empty() {
            full_text = tail.to_string();
        } else {
            full_text.push(' ');
            full_text.push_str(tail);
        }
    }
    if full_text.is_empty() {
        return Ok(RecordResult::DroppedNoise { raw: String::new() });
    }
    tracing::info!(
        chunks = assembled.chunk_count,
        failed_chunks = assembled.failed_chunks,
        chars = full_text.chars().count(),
        "chunked_utterance_assembled"
    );
    let stt_done_at = std::time::Instant::now();
    // Synthetic result with EMPTY segments, deliberately: the segment-based
    // hallucination heuristics are tuned for short single-request
    // utterances, and the only segment stats available here would describe
    // the tail — letting them judge a multi-minute joined transcript could
    // drop it wholesale.
    let transcription = crate::stt::TranscriptionResult {
        text: full_text.clone(),
        duration: total_samples as f64 / 16_000.0,
        language: tail_result.language.clone(),
        segments: Vec::new(),
    };
    Ok(RecordResult::Speech {
        text: full_text,
        energy,
        transcription,
        timing: UtteranceTiming {
            speech_end_at,
            stt_done_at,
            speculation_used,
        },
    })
}

/// Fast energy check for very low thresholds using squared values to avoid sqrt
/// This is much faster for thresholds <= 0.01 where precision isn't critical
#[inline]
fn fast_energy_check(samples: &[i16], threshold_sq: i64) -> bool {
    if samples.is_empty() {
        return false;
    }

    let sum_sq: i64 = samples
        .iter()
        .map(|sample| (*sample as i64) * (*sample as i64))
        .sum();

    // Compare squared values directly (no sqrt needed)
    sum_sq > threshold_sq * samples.len() as i64
}

/// Optimized normalized energy calculation for low thresholds
/// Uses SIMD-friendly operations when possible
fn fast_normalized_energy(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }

    // Use chunks for better cache performance
    let chunk_size = 64; // Process in chunks of 64 for better cache utilization
    let mut sum_sq: i64 = 0;

    for chunk in samples.chunks(chunk_size) {
        for &sample in chunk {
            let sample_i64 = sample as i64;
            sum_sq += sample_i64 * sample_i64;
        }
    }

    (sum_sq as f64 / samples.len() as f64).sqrt() / 32768.0
}

/// Standard normalized energy calculation (for higher thresholds where precision matters)
fn normalized_energy(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: i64 = samples
        .iter()
        .map(|sample| (*sample as i64) * (*sample as i64))
        .sum();
    (sum_sq as f64 / samples.len() as f64).sqrt() / 32768.0
}

fn voice_activity_energy_threshold(cfg: &AlwaysConfig) -> f64 {
    cfg.hear_energy_threshold.max(cfg.energy_threshold)
}

/// Per-frame early-voice gate: both raw energy and Silero probability
/// must clear their (deliberately loose) early thresholds. Pure so the
/// streak behavior is unit-testable with synthetic frame sequences.
fn early_voice_frame_ok(
    frame_energy: f64,
    silero_prob: f32,
    early_voice_energy_threshold: f64,
    speech_threshold: f32,
) -> bool {
    frame_energy >= early_voice_energy_threshold && silero_prob >= speech_threshold * 0.45
}

fn normal_silence_frames(cfg: &AlwaysConfig) -> usize {
    let secs = cfg.silence_secs.clamp(
        NORMAL_SILENCE_FLOOR_SECS,
        crate::always::config::SILENCE_SECS_MAX,
    );
    ((secs * 1000.0) / FRAME_MS as f64).ceil() as usize
}

fn tentative_silence_frames(final_silence_frames: usize) -> usize {
    // 1/4 of the window (was 1/3): with the 0.9s default the speculative
    // STT now starts ~225ms into the silence, raising the odds the result
    // (and the adaptive peek below) is home before the final cut. Wrong
    // guesses are discarded on speech resume and are cheap.
    (final_silence_frames / 4).max(1)
}

/// Extended final-silence window used once `looks_mid_sentence` fires:
/// FACTOR × the configured window, capped at +MIDSENTENCE_MAX_EXTRA_SECS.
fn extended_silence_frames(final_silence_frames: usize) -> usize {
    let extra_cap = ((MIDSENTENCE_MAX_EXTRA_SECS * 1000.0) / FRAME_MS as f64).ceil() as usize;
    (((final_silence_frames as f64) * MIDSENTENCE_EXTENSION_FACTOR) as usize)
        .min(final_silence_frames + extra_cap)
}

/// Does the speculative transcript look like an unfinished thought?
///
/// Two signals, both cheap and unit-testable:
/// - no sentence terminator at the end (commas/colons/dashes count as
///   explicitly unfinished), or
/// - a terminator is present but the last word is a connector ("and",
///   "which", …) — Whisper habitually appends a period to incomplete
///   clauses, so the trailing word overrides it.
///
/// False negatives are benign (current behavior: window unchanged).
fn looks_mid_sentence(loc: &crate::always::localization::Localization, text: &str) -> bool {
    let trimmed = text.trim();
    let Some(last_char) = trimmed.chars().last() else {
        return false;
    };
    if matches!(last_char, ',' | ';' | ':' | '—' | '-') {
        return true;
    }
    let last_word: String = trimmed
        .trim_end_matches(|c: char| !c.is_alphanumeric())
        .rsplit(char::is_whitespace)
        .next()
        .unwrap_or("")
        .to_lowercase();
    if TRAILING_CONNECTORS.contains(&last_word.as_str()) {
        return true;
    }
    !loc.sentence_terminators.contains(&last_char)
}

#[cfg(test)]
mod tests {
    use super::{
        early_voice_frame_ok, extended_silence_frames, fast_energy_check, fast_normalized_energy,
        looks_mid_sentence, normal_silence_frames, normalized_energy, tentative_silence_frames,
        voice_activity_energy_threshold,
    };
    use crate::always::AlwaysConfig;

    #[test]
    fn normalized_energy_handles_empty_input() {
        assert_eq!(normalized_energy(&[]), 0.0);
        assert_eq!(fast_normalized_energy(&[]), 0.0);
    }

    #[test]
    fn normalized_energy_scales_to_i16_range() {
        let samples = [16_384, -16_384];
        let energy = normalized_energy(&samples);
        let fast_energy = fast_normalized_energy(&samples);
        assert!((energy - 0.5).abs() < 0.001);
        assert!((fast_energy - energy).abs() < 0.001); // Fast version should be close
    }

    #[test]
    fn fast_energy_check_works() {
        let samples = [16_384, -16_384, 8_192, -8_192];
        let threshold_sq = (0.01f64 * 32768.0).powi(2) as i64;

        // These samples should easily pass a 0.01 threshold
        assert!(fast_energy_check(&samples, threshold_sq));

        // Very quiet samples should not pass
        let quiet_samples = [10, -10, 5, -5];
        assert!(!fast_energy_check(&quiet_samples, threshold_sq));
    }

    #[test]
    fn fast_methods_are_consistent_with_standard() {
        let test_samples = [
            1000, -800, 1200, -900, 600, -700, 1500, -1100, 400, -300, 800, -600, 200, -100, 900,
            -750,
        ];

        let standard_energy = normalized_energy(&test_samples);
        let fast_energy = fast_normalized_energy(&test_samples);

        // Fast method should be very close to standard method
        assert!((fast_energy - standard_energy).abs() < 0.001);
    }

    #[test]
    fn voice_activity_confirmation_uses_stricter_energy_floor() {
        let cfg = AlwaysConfig {
            energy_threshold: 0.012,
            hear_energy_threshold: 0.001,
            ..Default::default()
        };
        assert_eq!(voice_activity_energy_threshold(&cfg), 0.012);

        let cfg = AlwaysConfig {
            energy_threshold: 0.012,
            hear_energy_threshold: 0.02,
            ..Default::default()
        };
        assert_eq!(voice_activity_energy_threshold(&cfg), 0.02);
    }

    #[test]
    fn normal_silence_window_honors_configured_value() {
        // 0.6s default → 20 frames; the old internal 0.5s cap would
        // have produced 17 and silently shortened every utterance tail.
        let cfg = AlwaysConfig {
            silence_secs: 0.6,
            ..Default::default()
        };
        assert_eq!(normal_silence_frames(&cfg), 20);

        // User-raised window passes through (0.8s → 27 frames).
        let cfg = AlwaysConfig {
            silence_secs: 0.8,
            ..Default::default()
        };
        assert_eq!(normal_silence_frames(&cfg), 27);

        // 2.0s → 67 frames, no cap.
        let cfg = AlwaysConfig {
            silence_secs: 2.0,
            ..Default::default()
        };
        assert_eq!(normal_silence_frames(&cfg), 67);

        // Hard floor still protects against degenerate configs.
        let cfg = AlwaysConfig {
            silence_secs: 0.1,
            ..Default::default()
        };
        assert_eq!(normal_silence_frames(&cfg), 10);
    }

    #[test]
    fn tentative_silence_starts_before_final_cutoff() {
        // 1/4 of the window (30 frames = 0.9s default → kickoff at 7
        // frames ≈ 210ms of silence).
        assert_eq!(tentative_silence_frames(30), 7);
        assert_eq!(tentative_silence_frames(1), 1);
    }

    #[test]
    fn extended_silence_doubles_but_caps() {
        // 0.9s default → 30 frames → doubled to 60 (1.8s), under the
        // +1.5s (50 frame) cap... 30+50=80 > 60, so factor wins.
        assert_eq!(extended_silence_frames(30), 60);
        // Large user window (3s → 100 frames): cap wins, 100+50=150 < 200.
        assert_eq!(extended_silence_frames(100), 150);
    }

    #[test]
    fn looks_mid_sentence_truth_table() {
        let loc = &crate::always::localization::Localization::ENGLISH;
        // No terminator → mid-sentence.
        assert!(looks_mid_sentence(loc, "I went to the store"));
        // Explicit continuation punctuation.
        assert!(looks_mid_sentence(loc, "first item,"));
        assert!(looks_mid_sentence(loc, "the following:"));
        // Whisper's habitual period after a trailing connector.
        assert!(looks_mid_sentence(loc, "I want to change the file and."));
        assert!(looks_mid_sentence(loc, "It depends on the."));
        // Genuinely complete sentences.
        assert!(!looks_mid_sentence(loc, "Send the email now."));
        assert!(!looks_mid_sentence(loc, "Is that correct?"));
        assert!(!looks_mid_sentence(loc, "Stop!"));
        // Degenerate inputs.
        assert!(!looks_mid_sentence(loc, ""));
        assert!(!looks_mid_sentence(loc, "   "));
    }

    /// Mirror of the in-loop streak logic so synthetic frame sequences
    /// can prove transient rejection without a live audio pipeline.
    fn announce_after(frames: &[(f64, f32)]) -> bool {
        let early_energy_threshold = 0.0072; // 0.60 × 0.012 default
        let speech_threshold = 0.5f32;
        let mut streak = 0usize;
        for &(energy, prob) in frames {
            if early_voice_frame_ok(energy, prob, early_energy_threshold, speech_threshold) {
                streak += 1;
            } else {
                streak = 0;
            }
            if streak >= super::EARLY_VOICE_MIN_FRAMES {
                return true;
            }
        }
        false
    }

    #[test]
    fn early_voice_gate_requires_both_signals() {
        // Energy alone (keyboard clack): Silero stays low → no.
        assert!(!early_voice_frame_ok(0.05, 0.10, 0.0072, 0.5));
        // Silero alone (distant TV murmur under the energy floor) → no.
        assert!(!early_voice_frame_ok(0.001, 0.60, 0.0072, 0.5));
        // Both → yes.
        assert!(early_voice_frame_ok(0.02, 0.40, 0.0072, 0.5));
    }

    #[test]
    fn single_frame_transient_does_not_announce() {
        // One loud frame (clap / key press) surrounded by silence.
        assert!(!announce_after(&[
            (0.0001, 0.01),
            (0.05, 0.30), // the transient — passes both gates for 1 frame
            (0.0001, 0.02),
            (0.0001, 0.01),
        ]));
    }

    #[test]
    fn interrupted_streak_resets() {
        // Two qualifying frames separated by a silent frame must not
        // accumulate across the gap.
        assert!(!announce_after(&[
            (0.05, 0.30),
            (0.0001, 0.01),
            (0.05, 0.30),
            (0.0001, 0.01),
        ]));
    }

    #[test]
    fn sustained_speech_onset_announces() {
        assert!(announce_after(&[
            (0.0001, 0.01),
            (0.03, 0.35),
            (0.04, 0.55)
        ]));
    }
}
