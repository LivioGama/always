use anyhow::{Context, Result};
use futures::StreamExt;
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
    /// "My Voice" gate: the utterance's speaker embedding scored below
    /// the enrolled-voiceprint threshold — not the enrolled user
    /// (movie dialogue, music vocals, another person). Discarded
    /// before any STT spend.
    DroppedSpeaker {
        score: f32,
    },
    /// Another app (SuperWhisper, Zoom, FaceTime, …) took the microphone
    /// while this utterance was being captured. Recording stops on the
    /// spot so the two apps never transcribe the same speech, but the
    /// words captured up to that point are still transcribed and
    /// reported — they are just never pasted, because the other app is
    /// about to paste its own take of the same audio.
    PreemptedByMicConflict {
        text: String,
    },
    Timeout,
}

/// Operational "My Voice" gate for one utterance.
struct SpeakerGate {
    embedder: std::sync::Arc<crate::always::speaker_embed::SpeakerEmbedder>,
    voiceprint: std::sync::Arc<crate::always::voiceprint::VoiceProfile>,
    /// Bar for WHOLE-UTTERANCE judgements — the early (~2s) check, the
    /// tentative-silence check, the final check, and the mandatory
    /// end-of-utterance confirmation. Always exactly the user's
    /// configured pref: a whole utterance of the user's own voice must
    /// clear the bar they chose, with or without music playing, so
    /// dictating over media keeps working.
    threshold: f32,
    /// Bar for the SINGLE trailing-window verification in the ladder.
    /// Same as `threshold`, plus `AUDIO_PLAYING_GATE_BUMP` while system
    /// audio is playing. The window check is a Bernoulli trial repeated
    /// every 0.5s of voice against the max over four voiceprint targets,
    /// so it is the leaky statistic — media only has to get lucky once.
    /// Raising it while media plays costs the user nothing but latency:
    /// a genuine utterance that misses the raised window bar is still
    /// admitted by the whole-utterance check at ~2s, at the
    /// `early_abort_threshold` below.
    window_threshold: f32,
    /// Bar for the ~2s EARLY ABORT, and for that alone.
    ///
    /// The early abort is the only speaker check that DESTROYS an
    /// in-flight recording (`RecordResult::DroppedSpeaker`: the buffer is
    /// gone and capture restarts empty). Every other check either delays
    /// transcription or trims a tail. A bar that is wrong here does not
    /// cost latency — it costs the user the sentence they were speaking.
    ///
    /// It used to be `threshold`, on the reasoning that a 2s prefix is a
    /// "whole-utterance judgement". Measurement falsified that premise.
    /// Across 2026-08-31 and 2026-09-01 `speaker_gate_early_reject` fired
    /// 11,541 times and the whole-buffer score reached the user's 0.35
    /// pref exactly 5 times (max 0.4741; on 09-01 alone, 6,951 rejects
    /// and a maximum of 0.3330 — not one). The branch had become an
    /// unconditional kill. The user's own voice is why: `bc0ffb5`
    /// measured it at 0.024, -0.031, 0.071, 0.040, 0.318 across 12.6
    /// SECONDS before a window reached 0.617, and `speaker_embed`'s
    /// `MIN_EMBED_SAMPLES` already documents that scores get noisy below
    /// ~1s. A 2s prefix is a long window, not a short utterance, and
    /// `threshold` is calibrated for the latter.
    ///
    /// Observed consequence, 2026-09-01 11:37:17-11:37:31 UTC: the user
    /// spoke ~12.5s continuously, the abort fired at 0.1448 and again at
    /// 0.1931 destroying 5.7s of it, and what reached the clipboard and
    /// the UDS stream was the mid-sentence fragment
    /// "end to achieve this vision."
    ///
    /// So this bar is the permissive one, and deliberately NOT bumped
    /// while system audio plays: the bump exists to make a *leaky,
    /// repeated* trial stricter, and applying it to a destructive
    /// one-shot would delete the buffer of anyone dictating over music.
    /// The hostage case the abort exists for is untouched — media sits at
    /// p50 0.0116 / p90 0.1240, so ~90% of aborts still fire on the same
    /// audio. Whatever now survives to finalization still faces
    /// `threshold` at the mandatory whole-utterance confirmation, whose
    /// measured media ceiling is 0.3407, so this cannot paste anything
    /// the gate previously blocked.
    early_abort_threshold: f32,
    /// Bar for the TAIL MONITOR, which decides where a verified utterance
    /// stops being the user — and then TRUNCATES the buffer there.
    ///
    /// The second destructive decision in this file, and the same category
    /// of error as `early_abort_threshold`: a 1.5s window judged against a
    /// bar derived from the whole-utterance one. It was
    /// `threshold * SPEAKER_TAIL_THRESHOLD_FACTOR` — 0.21 against the
    /// user's 0.35 pref, not the 0.30 its comment claims, which was written
    /// when the default was 0.50.
    ///
    /// Its own doc already named the failure — "a mixed user+media window
    /// can dip well below the gate threshold while the user is genuinely
    /// still talking (observed live: mid-sentence cuts with a video
    /// playing)" — and the logs show it costing real speech: 185 cuts
    /// across 2026-08-31/09-01 discarded 243.9 SECONDS of audio, firing on
    /// 98% of the tails it examined.
    ///
    /// The decisive case is a single utterance contradicting itself. At
    /// 2026-09-01 11:37:30Z `speaker_gate_tail_cut` trimmed 1.02s at a
    /// window score of 0.1447, and 8s earlier the same utterance had been
    /// `speaker_gate_verified` at 0.5764 — the authoritative statistic said
    /// "this is the user" while the noisy one deleted the end of their
    /// sentence. A destructive decision must not be taken on the weaker
    /// statistic.
    ///
    /// So the tail monitor moves onto the permissive bar too. Media tails
    /// measure ~0.05 through real speakers and the observed cut scores sit
    /// at p25 0.0454, so the majority of genuine media tails are still cut;
    /// what stops being cut is the 0.12-0.26 band where the user's own
    /// trailing words live. Nothing new can be pasted: the kept buffer
    /// still faces the mandatory whole-utterance confirmation at
    /// `threshold`, so an utterance that is really media is still refuted
    /// whole rather than trimmed and kept.
    tail_threshold: f32,
}

struct SpeakerGateContext {
    requested: bool,
    gate: Option<SpeakerGate>,
}

fn speaker_gate_dependencies_ready(
    enabled: bool,
    profile_complete: bool,
    embedder_ready: bool,
) -> bool {
    enabled && profile_complete && embedder_ready
}

fn speaker_gate_allows_score(score: Option<f32>, threshold: f32) -> bool {
    score.is_some_and(|score| score >= threshold)
}

fn speaker_gate_allows_transcription(requested: bool, score: Option<f32>, threshold: f32) -> bool {
    !requested || speaker_gate_allows_score(score, threshold)
}

/// Speaker-gate enforcement. `true` = "My Voice" is enforced: an utterance that
/// doesn't verify as the enrolled user is dropped (and its trailing foreign
/// audio trimmed). This is what keeps Always listening ONLY to the user — a
/// YouTube video / another person in the room must NOT be transcribed or pasted.
/// (A brief fail-open experiment set this false and let every voice through;
/// that was wrong. The genuine data-loss was the hallucination filter, fixed
/// elsewhere.) The threshold is `cfg.speaker_gate_threshold` (a pref); lower it
/// if the user's own voice sits too close to the cutoff, rather than disabling.
const SPEAKER_GATE_ENFORCE_DROP: bool = true;

/// Verdict of the mandatory whole-utterance speaker confirmation.
///
/// The ladder verifies the user from a single trailing 1.5s window and
/// then LATCHES: `speaker_checked` is never cleared, so everything that
/// follows is treated as the user's speech and judged only against the
/// heavily relaxed tail bar (0.6x). That is one Bernoulli trial, retried
/// every 0.5s of voice, scored as the max over four voiceprint targets —
/// against continuous media it eventually fires, and when it does the
/// whole utterance is pasted.
///
/// Measured on the incident (2026-08-31 18:40:30-18:43:30 UTC, YouTube
/// playing, threshold 0.35): of ~50 single-window scores, three crossed
/// 0.35 (0.3623, 0.3655, 0.3751) and each leaked an entire utterance. Of
/// 42 WHOLE-UTTERANCE scores over the same audio, the maximum was 0.3407
/// — every one of them below the bar. The whole-utterance embedding is
/// simply the better statistic: a lucky 1.5s window is diluted by the
/// seconds of media around it, while the user's own utterance is their
/// voice end to end and scores at its usual level.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SpeakerConfirmation {
    /// The whole utterance matched. Transcribe it.
    Confirmed(f32),
    /// The whole utterance did NOT match. Drop it, no matter what a
    /// single window said earlier.
    Refuted(f32),
    /// Too little voiced audio (or the embedder failed) to form an
    /// opinion. Defer to whatever the ladder already decided — this must
    /// not newly drop a genuine dictation whose tail happens to be short
    /// or whose embedding call errored.
    Insufficient,
}

/// Pure decision half of `speaker_gate_confirm_utterance`.
///
/// `scored` is false when there was not enough voiced audio to bother
/// embedding; `score` is `None` when the embedder itself failed. Both
/// mean "no opinion" — deliberately NOT a rejection, so a transient
/// embed error or a short trailing fragment can never newly discard
/// speech the ladder already accepted.
fn speaker_confirmation(scored: bool, score: Option<f32>, threshold: f32) -> SpeakerConfirmation {
    if !scored {
        return SpeakerConfirmation::Insufficient;
    }
    match score {
        Some(score) if score >= threshold => SpeakerConfirmation::Confirmed(score),
        Some(score) => SpeakerConfirmation::Refuted(score),
        None => SpeakerConfirmation::Insufficient,
    }
}

/// Re-score the audio that is about to be transcribed against the
/// enrolled voiceprint, as a whole.
fn speaker_gate_confirm_utterance(
    gate: &SpeakerGate,
    samples: &[i16],
    _voiced_samples: usize,
) -> SpeakerConfirmation {
    let min = crate::always::speaker_embed::MIN_EMBED_SAMPLES;
    // Score whenever the embedder CAN embed (samples.len() >= min), not only
    // when voiced_samples >= min. A short burst of video/TTS audio can have
    // samples.len() >= min (the 1.5s trailing window that passed the ladder
    // is 24000 samples) but voiced_samples < min (only 0.3s of actual voice).
    // The old `voiced_samples >= min` gate returned `Insufficient` for these,
    // deferring to the ladder — which accepted them on a 0.15 window bar that
    // media audio clears easily. That was the leak: non-user audio passed the
    // window, STT ran, and the whole-utterance check said "no opinion" instead
    // of scoring and refuting. Now we score whenever the embedder can, so the
    // whole-utterance bar (0.5) catches what the window bar (0.15) let through.
    let can_embed = samples.len() >= min;
    let score = can_embed.then(|| speaker_gate_score(gate, samples)).flatten();
    speaker_confirmation(can_embed, score, gate.threshold)
}

/// Bar for the ladder's single trailing-window verification.
///
/// Raised by `AUDIO_PLAYING_GATE_BUMP` while the Mac is playing audio.
/// The whole-utterance threshold is deliberately NOT raised — see
/// `SpeakerGate::window_threshold`.
fn speaker_gate_window_threshold(threshold: f32, system_audio_playing: bool) -> f32 {
    if system_audio_playing {
        // Competing audio present: be strict per-window. A media voice that
        // wins one window used to latch the whole utterance.
        threshold + AUDIO_PLAYING_GATE_BUMP
    } else {
        // Nothing else is playing, so the ONLY plausible speaker is the user.
        // The per-window bar exists to decide "start transcribing now"; the
        // authoritative reject is the whole-utterance confirmation that runs
        // at finalization and cannot be bypassed. Keeping both bars equal made
        // the fast one the bottleneck: measured on a real utterance, the
        // user's own voice scored 0.024, -0.031, 0.071, 0.040, 0.318 across
        // 12.6 SECONDS before a window finally hit 0.617 -- 12.6s of their
        // speech discarded while every rejection was later contradicted by
        // the whole-utterance score.
        //
        // So this bar is deliberately permissive: admit early, and let the
        // whole-utterance check do the actual rejecting. Its own measured
        // ceiling for media is 0.3407, so `WINDOW_FLOOR` stays below the
        // user's observed range while the real gate keeps its full strength.
        (threshold * WINDOW_THRESHOLD_FRACTION).min(WINDOW_THRESHOLD_CEILING)
    }
}

/// Per-window bar as a fraction of the configured whole-utterance threshold.
/// Only governs how fast transcription STARTS, never whether it is kept.
const WINDOW_THRESHOLD_FRACTION: f32 = 0.35;
/// Absolute ceiling so raising the main threshold cannot make the fast bar
/// strict enough to reintroduce the 12-second rejection stalls.
const WINDOW_THRESHOLD_CEILING: f32 = 0.15;

fn speaker_gate_allows_stt(requested: bool, speaker_verified: bool) -> bool {
    // "Only me": when the gate is requested, STT/speculation/preview must wait
    // until the enrolled user is verified, so a foreign voice never even
    // transcribes or streams a preview.
    !requested || speaker_verified
}

fn speaker_gate_should_reject_unavailable(
    requested: bool,
    ready: bool,
    voiced_samples: usize,
) -> bool {
    SPEAKER_GATE_ENFORCE_DROP && requested && !ready && voiced_samples >= SPEAKER_GATE_EARLY_SAMPLES
}

fn speaker_gate_ctx(cfg: &AlwaysConfig) -> SpeakerGateContext {
    if !cfg.speaker_gate_enabled {
        return SpeakerGateContext {
            requested: false,
            gate: None,
        };
    }
    let profile = crate::always::voiceprint::current();
    let profile_complete = profile
        .as_ref()
        .is_some_and(|profile| profile.is_complete());
    let requested = cfg.speaker_gate_enabled && profile_complete;
    let embedder = if requested {
        crate::always::speaker_embed::global()
    } else {
        None
    };
    let ready = speaker_gate_dependencies_ready(
        cfg.speaker_gate_enabled,
        profile_complete,
        embedder.is_some(),
    );
    // While system audio is playing we are, by definition, hearing at least
    // one voice that is not the user's. The wake-on-voice path in
    // `event_loop` deliberately records ONE utterance through an
    // audio-output pause so the user can dictate over music -- but that same
    // path let a YouTube narrator through: it scored 0.404 against a 0.35
    // threshold and was transcribed and pasted as if the user had said it.
    //
    // So the SINGLE-WINDOW verification gets stricter exactly when competing
    // audio is present. The bump lands on `window_threshold` only, never on
    // `threshold`: whole-utterance judgements keep the user's configured bar,
    // so dictating over music still works even when the user's voice sits
    // near the cutoff (their Nepali scores ~0.45-0.52 against a 0.35 pref --
    // a bumped whole-utterance bar of 0.50 would have rejected half of it).
    // The cost of the raised window bar is at most ~1.5s of extra latency
    // before the badge lights: the whole-utterance check at ~2s admits them.
    //
    // NOTE: this reads `is_system_audio_playing()` -- the FACT -- not
    // `is_audio_output_paused()`, the pause SOURCE. The pause source is
    // force-cleared by the UDS handler whenever this gate is ready, so
    // keying off it made the bump unreachable in the only configuration
    // that needs it. See `pause::SYSTEM_AUDIO_PLAYING`.
    let threshold = cfg.speaker_gate_threshold as f32;
    let window_threshold =
        speaker_gate_window_threshold(threshold, crate::always::pause::is_system_audio_playing());
    let gate = ready.then(|| SpeakerGate {
        embedder: embedder.expect("ready speaker gate must have an embedder"),
        voiceprint: profile.expect("ready speaker gate must have a voiceprint"),
        threshold,
        window_threshold,
        // Unbumped on purpose — see `SpeakerGate::early_abort_threshold`.
        early_abort_threshold: speaker_gate_window_threshold(threshold, false),
        // Also destructive, also a 1.5s window — same bar, same reason.
        tail_threshold: speaker_gate_window_threshold(threshold, false),
    });
    SpeakerGateContext { requested, gate }
}

pub(crate) fn speaker_gate_ready(cfg: &AlwaysConfig) -> bool {
    speaker_gate_ctx(cfg).gate.is_some()
}

/// Score `samples` against the enrolled voiceprint. `None` means the
/// speaker could not be verified; gated callers must not treat it as a
/// match.
fn speaker_gate_score(gate: &SpeakerGate, samples: &[i16]) -> Option<f32> {
    let started = std::time::Instant::now();
    match gate.embedder.embed(samples) {
        Ok(e) => {
            let (score, matched) = best_voiceprint_match(&e, &gate.voiceprint);
            tracing::debug!(
                score,
                matched,
                elapsed_ms = started.elapsed().as_millis() as u64,
                audio_secs = samples.len() as f64 / 16_000.0,
                "speaker_gate_scored"
            );
            Some(score)
        }
        Err(err) => {
            tracing::warn!(error = %err, "speaker_gate_embed_failed");
            None
        }
    }
}

/// Best cosine similarity between `embedding` and the enrolled profile,
/// taken over the combined voiceprint AND each individual enrollment
/// style. Returns the score plus which target won, for the logs.
///
/// Enrollment records three deliberately different styles (normal,
/// lower, louder) and stores each embedding, but the combined
/// voiceprint is their normalised sum — a centroid that is not equal to
/// any of them. Scoring against the centroid alone therefore caps what
/// the user can ever reach: measured on a real profile, a flawless
/// re-recording of the "louder" style tops out at 0.87, and the styles
/// sit as far as 0.66 from each other. Every genuine utterance paid
/// that gap, which pushed the first (shortest, noisiest) check below
/// threshold and delayed the listening overlay by whole seconds while
/// the ladder retried.
///
/// Taking the max costs one dot product per style over 256 floats and
/// never scores BELOW the old behaviour, since the centroid stays in
/// the candidate set.
fn best_voiceprint_match(
    embedding: &[f32],
    profile: &crate::always::voiceprint::VoiceProfile,
) -> (f32, &'static str) {
    use crate::always::speaker_embed::cosine;
    let mut best = cosine(embedding, &profile.voiceprint);
    let mut matched = "combined";
    for (step, step_embedding) in &profile.steps {
        // A profile written by a different model is rejected before the
        // gate is built, but a truncated/partial step would still be a
        // dimension mismatch — skip rather than score garbage.
        if step_embedding.len() != embedding.len() {
            continue;
        }
        let score = cosine(embedding, step_embedding);
        if score > best {
            best = score;
            matched = match step.as_str() {
                "normal" => "normal",
                "lower" => "lower",
                "louder" => "louder",
                _ => "step",
            };
        }
    }
    (best, matched)
}

/// Voiced audio needed before the mid-recording (early) speaker check
/// fires: 2 s of actual voice gives the embedding enough evidence
/// while still cutting a movie scene off ~2 s in.
const SPEAKER_GATE_EARLY_SAMPLES: usize = 32_000;

/// Speaker-aware end-of-utterance. Generic VAD keeps the utterance open
/// as long as ANY speech-like audio continues — background media held
/// recordings hostage until the user paused the video (observed live:
/// dictation only pasted after pausing YouTube). Once the user's voice is
/// verified, the trailing window is re-scored every
/// `SPEAKER_TAIL_CHECK_EVERY_SAMPLES` of new voiced audio; after
/// `SPEAKER_TAIL_FAIL_STREAK` consecutive mismatches the utterance is cut
/// at the last matching boundary and finalized — so the end of dictation
/// is defined by THE USER going quiet, not by the room going quiet.
const SPEAKER_TAIL_CHECK_EVERY_SAMPLES: usize = 8_000; // 0.5s of voice
/// Re-check cadence while the speaker is still UNVERIFIED: 0.25 s of new
/// voiced audio between attempts. Tighter than the tail cadence — each
/// embed costs ~30-50 ms and the payoff is the listening overlay (and
/// live preview, which is gated on verification) appearing up to ~1 s
/// sooner. Reverts to `SPEAKER_TAIL_CHECK_EVERY_SAMPLES` once verified.
const SPEAKER_GATE_UNVERIFIED_CHECK_EVERY_SAMPLES: usize = 4_000; // 0.25s of voice
/// Trailing window scored by each tail check: 1.5s is enough voice for a
/// stable embedding while keeping the cut ~1.5s behind the user's last
/// word.
const SPEAKER_TAIL_WINDOW_SAMPLES: usize = 24_000;
/// FLOOR on consecutive mismatching windows before the cut. The actual
/// requirement scales with the user's configured `silence_secs` (see
/// `speaker_tail_fail_checks` in the record loop) so the pause tolerance
/// they chose applies to media-covered pauses too — a fixed 2-check
/// (~1s) cut felt "immediate" against a 2.2s configured window.
const SPEAKER_TAIL_FAIL_STREAK: usize = 2;
/// The bar the tail monitor used to judge against: a fraction of the
/// whole-utterance threshold. Retained only so the regression it caused
/// stays testable and named — see `SpeakerGate::tail_threshold`, which
/// replaced it. Its own comment already knew the failure mode ("a mixed
/// user+media window can dip well below the gate threshold while the user
/// is genuinely still talking") and its arithmetic was stale: it claims
/// 0.6 × 0.50 = 0.30, but against the user's actual 0.35 pref it produced
/// 0.21, and it cut 243.9 seconds of audio across 185 firings in two days.
#[cfg(test)]
const SPEAKER_TAIL_THRESHOLD_FACTOR: f32 = 0.6;

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
    Err(anyhow::anyhow!(
        "Audio capture not supported on this platform"
    ))
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
/// silence once it holds at least this much speech. This is deliberately
/// short: Groq's API is file-based, so "live" transcription means keeping
/// small committed chunks in flight while the user keeps talking, then
/// pasting once after the final relaxed pause.
const CHUNK_TARGET_SECS: u32 = 6;

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
/// Is a live decode session actually carrying this utterance right now?
///
/// Both failure modes must count as "no": a session whose worker has died or
/// fallen hopelessly behind (`degraded`) returns no transcript and finalizes
/// through the one-shot path, and a session whose audio was truncated out
/// from under it (`live_invalidated`) no longer describes the buffer. In
/// either case rolling chunks are valuable again and must resume.
fn live_session_carrying(
    live: Option<&crate::always::live_stream::LiveStream>,
    live_invalidated: bool,
) -> bool {
    live_carrying_state(live.map(|live| live.degraded()), live_invalidated)
}

/// The decision above with the session reduced to its observable state, so
/// the truth table is testable without a loaded model.
/// `degraded`: `None` = no session at all.
fn live_carrying_state(degraded: Option<bool>, live_invalidated: bool) -> bool {
    !live_invalidated && degraded == Some(false)
}

/// Inputs to the live verdict's evaluation cadence, extracted so the gate is
/// testable in isolation.
///
/// The gate used to be an inline `&&` chain, and the reason log lived INSIDE
/// it. When the chain was false the block never ran, no reason was computed,
/// and the blocker was invisible — the exact failure the reason log had been
/// added to prevent, one level up. Pulling it out makes each term nameable
/// (see `silence_verdict_state`) and unit-testable.
#[derive(Clone, Copy)]
struct VerdictTick {
    live_invalidated: bool,
    voice_logged: bool,
    speculation_speaker_ok: bool,
    consecutive_silence: usize,
    tentative_frames: usize,
    base_frames: usize,
    midsentence_decided: bool,
}

/// May the live mid-sentence / early-finalize verdict be evaluated this frame?
///
/// Deliberately does NOT include `voiced_since_flush`. That flag means "no real
/// speech has landed in the buffer since the last chunk drain", and its purpose
/// is to avoid SPENDING a speculative STT round trip on trailing silence. The
/// live verdict spends nothing — it reads a transcript that already exists — and
/// gating it on the flag cost every chunked utterance its verdict for the whole
/// remaining silence run, because the flag is cleared by the flush and only a
/// VOICED frame ever sets it back. When the flushing pause was also the end of
/// the utterance, nothing could ever re-enable it.
fn verdict_tick_allowed(t: VerdictTick) -> bool {
    t.consecutive_silence >= t.tentative_frames
        && !t.live_invalidated
        && t.voice_logged
        && t.speculation_speaker_ok
        // Every 2nd frame to keep the hot loop cheap, plus one guaranteed last
        // look immediately before the base cut. The last look matters because
        // the verdict waits for the trailing audio to be decoded: without it,
        // an utterance whose undecoded remainder outlives the final even frame
        // would reach the cut with no verdict at all.
        && (t.consecutive_silence.is_multiple_of(2)
            || (!t.midsentence_decided && t.consecutive_silence + 1 >= t.base_frames))
}

/// Why rolling chunking is (or is not) bypassed right now, as a log field.
///
/// `live_session_carrying` collapses three very different situations into one
/// `false`, and the only observable was `chunk_flush` appearing on a machine
/// where chunking was supposed to be off. This names which one it was.
fn live_carrying_reason(
    live: Option<&crate::always::live_stream::LiveStream>,
    live_invalidated: bool,
) -> &'static str {
    match live {
        None => "no_session",
        Some(_) if live_invalidated => "invalidated",
        Some(live) if live.degraded() => "degraded",
        Some(_) => "carrying",
    }
}

/// Speech that must accumulate before a chunk is committed at the next
/// natural pause. See [`STREAM_CHUNK_TARGET_SECS`] for why a live session
/// raises this so far.
///
/// The `ALWAYS_CHUNK_TARGET_SECS` test override still wins even while
/// streaming, so an end-to-end test can exercise the chunk path with seconds
/// of audio on a machine whose engine happens to stream.
fn effective_chunk_target_secs(live_streaming: bool) -> u32 {
    if live_streaming && std::env::var_os("ALWAYS_CHUNK_TARGET_SECS").is_none() {
        STREAM_CHUNK_TARGET_SECS
    } else {
        chunk_target_secs()
    }
}

/// Mid-speech ceiling for the current utterance. Never below the target —
/// a hard max under the target would flush every chunk early and defeat the
/// pause-aligned seam.
fn effective_chunk_hard_max_secs(live_streaming: bool) -> u32 {
    CHUNK_HARD_MAX_SECS.max(effective_chunk_target_secs(live_streaming))
}

/// Absolute per-chunk ceiling: flush at a frame boundary even mid-speech
/// if the user talks continuously for this long without a tentative dip.
/// This keeps uninterrupted monologues from becoming one large final STT
/// call; natural-silence chunking above handles the common case.
const CHUNK_HARD_MAX_SECS: u32 = 15;
/// Chunk ceiling while a healthy LIVE decode session is carrying the
/// utterance. Effectively "don't chunk", with a safety valve.
///
/// Rolling chunking exists for exactly one reason: a one-shot decode costs
/// ~0.10x realtime, so a long utterance's single final decode grows without
/// bound (0.5 s for 5 s of audio, 3.8 s for 40 s, 11.5 s for 2 min). A live
/// streaming session has no such term — it decodes each 560 ms window as it
/// arrives and finalization is flat.
///
/// Chunking a STREAMING utterance is therefore not merely useless, it is a
/// large net loss: every flush calls `LiveStream::reset` (the ordering
/// contract in `live_stream.rs` requires it), so each committed chunk falls
/// back to a full from-scratch one-shot decode. Past `CHUNK_TARGET_SECS` the
/// user paid a fresh one-shot decode every 6 s of speech, and the end-of-
/// utterance wait grew linearly with how long they talked — measured 487 ms
/// while streaming, then 880 → 1086 → 1296 ms once chunking kicked in. That
/// is the "it takes longer and longer the longer I speak" complaint.
///
/// 120 s, not "never": it is the longest span over which per-chunk cost was
/// actually MEASURED flat (`examples/nemotron_stream_bench.rs` drives one
/// session across 119.7 s / 214 chunks; parakeet-rs bounds its own retained
/// audio to ~1.8 s regardless of session length, so the flatness is
/// structural, not luck). Past that the rolling-chunk machinery takes over
/// again and keeps what only it provides: per-chunk retries, per-chunk
/// grammar for text beyond `GRAMMAR_MAX_CHARS`, the failed-chunk WAV spill,
/// and a bound on `speech_samples` (120 s ≈ 3.8 MB).
const STREAM_CHUNK_TARGET_SECS: u32 = 120;
/// How often to re-attempt opening a live decode session when the first
/// attempt (at the top of `record`) came back `None` while the engine was
/// still loading. Cheap — a `try_lock` plus, on a non-streaming engine, an
/// immediate `None` — but not free, so not every 30 ms frame.
const LIVE_START_RETRY_MS: u64 = 300;
/// How many times one utterance may replace a degraded live session before it
/// gives up and lets rolling chunking carry the rest. Bounded so a genuinely
/// broken engine costs three re-opens, not one per 300 ms for a whole
/// dictation.
const MAX_LIVE_REOPENS: u32 = 3;
/// First-speculation cadence, used in EVERY mode (not just consume mode):
/// fire the speculative transcription at a brief inter-phrase pause (~240ms
/// = 8 × 30ms frames) so a stream consumer sees text land as the user
/// speaks / can react to a leading wake word, instead of waiting for the
/// slower normal tentative mark. Only the PREVIEW (`TranscriptChunk`) timing
/// is affected — the final cut still uses the full silence window, so
/// dictation-finalization/paste latency is unchanged.
const CONSUME_STREAM_TENTATIVE_FRAMES: usize = 8;
/// Consume-mode LIVE streaming: while the user is talking continuously (no
/// pause to trigger the tentative-silence speculation above), re-transcribe
/// the growing buffer this often so previews land mid-sentence. Groq calls
/// are file-based (~0.7-1.5s each) and `speculation_pending` serialises them,
/// so this interval is measured from kickoff and the effective cadence
/// self-limits to ≈ one Groq round-trip. Kept well below that latency so a
/// new preview fires the instant the previous one lands — i.e. as fast as
/// Groq can answer. Only active in consume mode.
const CONSUME_STREAM_INTERVAL_MS: u64 = 200;
/// Minimum voiced audio before the first live preview fires (~0.25s), so even
/// a short utterance streams at least one preview before the final.
const CONSUME_STREAM_MIN_SAMPLES: usize = 4_000;
/// Cap each live preview to the last ~10s of audio. The preview re-transcribes
/// on the SAME single STT engine as the final; letting it grow unbounded meant
/// a long chunk's preview held the engine for seconds and stalled the final
/// transcription (a major source of consume-mode/Iris latency). 10s shows the
/// recent words while keeping every preview cheap; the final is always complete.
// 10s → 3s. Each preview re-decodes this much audio FROM SCRATCH on the same
// model mutex the final transcription needs, and on a local engine that is
// real CPU, not a network wait. Measured during one 30s utterance: seven
// previews at 318-1966ms each, and the final decode queued behind them for
// 1911ms. The overlay only needs the recent words to feel live; the paste
// needs the lock. 3s keeps the preview useful at a third of the cost.
const CONSUME_STREAM_PREVIEW_MAX_SAMPLES: usize = 3 * 16_000;
/// Minimum gap between live-session preview broadcasts. The session produces
/// a new transcript every 560 ms of audio; this only stops a burst of
/// identical/near-identical UDS frames when the worker catches up on several
/// queued windows at once. Costs nothing to compute — the text already exists.
const LIVE_STREAM_PREVIEW_MIN_GAP_MS: u64 = 250;

/// Live mid-speech preview cadence for NON-streaming backends (Groq)
/// during normal dictation, gated by the `stt_live_preview` pref. Each
/// tick is a full cloud round trip (~250-400ms typical, up to ~1.5s),
/// so this is an order of magnitude slower than
/// `CONSUME_STREAM_INTERVAL_MS` above: fast enough that the overlay
/// text feels live, slow enough that a minute of dictation costs tens
/// of extra API calls, not hundreds.
const LIVE_PREVIEW_INTERVAL_MS: u64 = 1_500;
/// How much stricter the speaker gate gets while system audio is playing.
/// See `build_speaker_gate_context`.
const AUDIO_PLAYING_GATE_BUMP: f32 = 0.15;

/// Live preview cadence for a LOCAL streaming engine (Nemotron).
///
/// `CONSUME_STREAM_INTERVAL_MS` above is 200ms and explicitly relies on a
/// Groq round trip to self-limit the rate ("the effective cadence
/// self-limits to ≈ one Groq round-trip"). A local engine has NO network
/// latency, so nothing throttles it: previews run back-to-back, each
/// re-decoding up to `CONSUME_STREAM_PREVIEW_MAX_SAMPLES` (10s) through the
/// model on this machine's own cores, continuously, for as long as the user
/// keeps talking. Observed effect: load average >90 and the recorder starved
/// of CPU (`rec_coreaudio_overrun`).
///
/// 700ms is above Nemotron's 560ms chunk period, so a preview still lands
/// roughly per chunk and the overlay stays live, while the engine gets real
/// idle time between passes.
// 700ms → 1400ms. Previews measured 318-1966ms on this machine, so at 700ms
// they ran effectively back-to-back and never released the model lock. The
// interval must exceed the typical preview cost or the queue never drains.
const LOCAL_STREAM_INTERVAL_MS: u64 = 1_400;
/// Minimum NEW voiced audio before another local-streaming preview fires.
/// The 200ms path sets this to 0, so it re-decodes IDENTICAL audio when the
/// user pauses mid-sentence — pure waste on a compute-bound engine. 0.5s.
const LOCAL_STREAM_MIN_NEW_SAMPLES: usize = 8_000;
/// Minimum NEW voiced audio (samples at 16kHz, ~1s) accumulated since
/// the last live-preview kickoff before the next tick may fire.
/// Re-transcribing near-identical audio is a wasted round trip.
const LIVE_PREVIEW_MIN_NEW_SAMPLES: usize = 16_000;
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
/// Early finalization: the silence required to end an utterance whose LIVE
/// transcript already reads as a finished sentence.
///
/// The mid-sentence machinery only ever made the window LONGER. But the same
/// signal read the other way is a much better end-of-utterance detector than
/// raw silence duration: if the user has stopped and what they said ends in a
/// terminator with no trailing connector, the thought is over and there is
/// nothing to wait for. Streaming finalization costs ~130 ms (see
/// `live_stream.rs`), so the silence window is now the dominant term in
/// perceived latency — cutting it from the configured 600 ms to 300 ms on
/// finished sentences is the single largest available win.
///
/// 300 ms (10 frames) is deliberately conservative: it is above the 240 ms
/// tentative mark, above `SHORT_SILENCE_MS`, and well above a normal
/// inter-word gap. It is only ever used when the transcript is COMPLETE and
/// fully decoded — see `EarlyFinalize` below — and never exceeds the
/// configured window.
const COMPLETE_UTTERANCE_SILENCE_MS: u32 = 300;
/// Words required in the live transcript before the "finished sentence"
/// verdict may shorten the window. A one- or two-word fragment that happens
/// to carry a period ("Okay.") is exactly what a mid-thought pause looks
/// like to the decoder, and short utterances already have their own fast
/// path (`SHORT_SILENCE_MS`). Three words is the cheapest guard that keeps
/// the aggressive cut off the ambiguous cases.
const COMPLETE_UTTERANCE_MIN_WORDS: usize = 3;
/// Trailing words that mark a clause as clearly unfinished even when
/// Whisper appended its habitual period. Lowercase, punctuation-stripped.
const TRAILING_CONNECTORS: &[&str] = &[
    "and", "but", "or", "so", "because", "which", "that", "to", "the", "a", "an", "with", "for",
    "of", "in", "on", "at", "by",
    // Hesitation fillers. The user reports saying "uh" precisely WHEN STILL
    // THINKING -- it is the most reliable signal in the transcript that more
    // speech is coming, and cutting there truncates the thought. Treating it
    // as a connector extends the silence window exactly as "and" or "to" do.
    // The filler itself is stripped from the final text by
    // `strip_trailing_filler`; it should buy thinking time, not appear.
    "uh", "um", "uhh", "umm", "er", "erm", "hmm", "mmm", "like",
];

/// Fillers to remove from the END of a finished transcript.
///
/// Deliberately the hesitation subset of `TRAILING_CONNECTORS` -- a real
/// trailing "and" or "to" is the user's word and must survive; a trailing
/// "uh" is them thinking out loud and is never wanted in the pasted text.
const TRAILING_FILLERS: &[&str] = &["uh", "um", "uhh", "umm", "er", "erm", "hmm", "mmm"];

/// Strip trailing hesitation fillers (and any punctuation they trail) from a
/// finished transcript. Applied once at finalization, never to previews.
pub(crate) fn strip_trailing_filler(text: &str) -> String {
    let mut out = text.trim_end().to_string();
    let mut removed_any = false;
    loop {
        // Look past whitespace/comma/period to find the last real word.
        let stripped = out.trim_end_matches(|c: char| c == ',' || c == '.' || c.is_whitespace());
        let last = stripped.rsplit(char::is_whitespace).next().unwrap_or("");
        let norm = last
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        if norm.is_empty() || !TRAILING_FILLERS.contains(&norm.as_str()) {
            break;
        }
        out = stripped[..stripped.len() - last.len()]
            .trim_end()
            .to_string();
        removed_any = true;
    }
    if removed_any {
        // Only now trim punctuation the removed filler orphaned. A transcript
        // that legitimately ends "done." must keep its period.
        out = out
            .trim_end_matches(|c: char| c == ',' || c.is_whitespace())
            .to_string();
    }
    out
}

/// How the mid-speech live-preview loop ticks for the current
/// mode/engine/pref combination. `None` = no live preview at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PreviewCadence {
    /// Minimum time between preview kickoffs.
    interval_ms: u64,
    /// New samples (beyond the last kickoff) required before the next
    /// tick. Zero for the fast local/consume path.
    min_new_samples: usize,
    /// Whether a pending tentative-silence speculation blocks the tick.
    /// The slow cloud path must never race the speculative/final
    /// transcription; the fast path never waited and must not start.
    require_speculation_idle: bool,
    /// Whether kicking off a preview flips the overlay to Transcribing.
    /// Consume/streaming previews historically did; the slow cloud
    /// preview keeps the Listening badge — the user is still talking
    /// and the partial text renders under it.
    flip_overlay: bool,
    /// Prefix the settled chunk-join text so a long chunked utterance's
    /// preview shows the whole sentence, not just the open chunk. Only
    /// the slow overlay path — consume-mode payloads are parsed by
    /// external consumers and must not change shape.
    prefix_settled_chunks: bool,
}

/// Decide whether — and how — the live preview loop is armed.
///
/// Priority: consume mode or a genuinely-streaming engine keep the
/// original fast cadence (unchanged behavior). Otherwise the
/// `stt_live_preview` pref arms a slow cloud cadence so Groq dictation
/// still shows provisional text while the user talks.
fn preview_cadence(
    consume_mode: bool,
    streaming_engine: bool,
    local_engine: bool,
    apple_engine: bool,
    live_preview_pref: bool,
) -> Option<PreviewCadence> {
    // Apple STT is non-streaming and file-based. Live preview competes with
    // final chunk transcription for the same serialized recognizer, adding
    // latency and empty-result races. Disable preview entirely for Apple.
    if apple_engine {
        return None;
    }
    // A LOCAL streaming engine is compute-bound, not network-bound. It must
    // be throttled explicitly — see `LOCAL_STREAM_INTERVAL_MS`. Checked
    // BEFORE the consume/streaming branch so a local streaming engine never
    // falls into the unthrottled cloud cadence, in consume mode or out of it.
    if streaming_engine && local_engine {
        return Some(PreviewCadence {
            interval_ms: LOCAL_STREAM_INTERVAL_MS,
            min_new_samples: LOCAL_STREAM_MIN_NEW_SAMPLES,
            require_speculation_idle: true,
            flip_overlay: true,
            prefix_settled_chunks: false,
        });
    }
    if consume_mode || streaming_engine {
        return Some(PreviewCadence {
            interval_ms: CONSUME_STREAM_INTERVAL_MS,
            min_new_samples: 0,
            require_speculation_idle: false,
            flip_overlay: true,
            prefix_settled_chunks: false,
        });
    }
    if live_preview_pref {
        return Some(PreviewCadence {
            interval_ms: LIVE_PREVIEW_INTERVAL_MS,
            min_new_samples: LIVE_PREVIEW_MIN_NEW_SAMPLES,
            require_speculation_idle: true,
            flip_overlay: false,
            prefix_settled_chunks: true,
        });
    }
    None
}

/// Pre-pay the blocking grammar LLM call for `text` while the user is still
/// inside the silence window.
///
/// Byte-identical to what the paste path will ask for — same assembly of
/// settled chunks + tail, same skip conditions, same request builder — so the
/// paste path's call is a cache hit (`PostProcessor` also single-flights, so
/// arriving mid-warm still waits only once). Extracted from the speculation
/// thread so the live-stream path can fire the same warm ~600 ms earlier: with
/// a persistent session the transcript exists at the tentative pause instead
/// of a full re-decode later.
#[cfg(feature = "macos")]
fn spawn_grammar_warm(
    text: &str,
    post_processor: Option<Arc<crate::always::postprocess::PostProcessor>>,
    chunk_join: &crate::always::chunker::ChunkJoinHandle,
    rt: &tokio::runtime::Handle,
) {
    let Some(pp) = post_processor else {
        return;
    };
    if text.is_empty() {
        return;
    }
    // Chunked utterance: the paste key is join(corrected chunks) + tail (see
    // `finalize_chunked`), so warm THAT — but only once the join is
    // deterministic; an unsettled join would warm a key finalize never asks for.
    let warm_target = if chunk_join.chunk_count() > 0 {
        chunk_join.settled_join().map(|joined| {
            let tail = text.trim();
            if joined.is_empty() {
                tail.to_string()
            } else if tail.is_empty() {
                joined
            } else {
                format!("{joined} {tail}")
            }
        })
    } else {
        Some(text.to_string())
    };
    let Some(target) = warm_target else {
        return;
    };
    if crate::always::event_loop::is_short_utterance(&target)
        || target.chars().count() > crate::always::event_loop::GRAMMAR_MAX_CHARS
    {
        return;
    }
    let req = crate::always::correction_request::build(&target, pp.can_correct());
    rt.spawn(async move {
        let _ = pp.process_request(&req).await;
    });
}

/// StateMonitor replaces its partial transcript on every event, so streaming
/// producers must send the cumulative text rather than the latest chunk only.
fn append_streaming_preview(accumulated: &mut String, chunk: &str) -> bool {
    let chunk = chunk.trim();
    if chunk.is_empty() {
        return false;
    }
    if !accumulated.is_empty() {
        accumulated.push(' ');
    }
    accumulated.push_str(chunk);
    true
}

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
    // Two-stage end-of-utterance detection (Option B):
    // - At `eff_tentative_frames` (always the fast ~240ms cadence — see its
    //   computation below), kick off a SPECULATIVE transcription in a
    //   background thread using the audio captured so far.
    // - Continue recording until `silence_frames` (final).
    // - If speech resumes during the tentative window, discard the speculation.
    // - At final, if speculation is still valid (no resume), use its result —
    //   transcription has been running in parallel during the silence wait, so
    //   the user gets snappy paste with no extra latency cost.
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
    // First frame of this utterance that looked like speech — the
    // reference point for the measured onset→badge latency. Declared
    // before `announce_voice_activity!` because macro_rules hygiene
    // binds the locals a macro reads at its DEFINITION site.
    let mut first_voice_at: Option<std::time::Instant> = None;
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
                // Measured onset→badge latency. Previously only
                // derivable from constants, because `voice_detected` is
                // logged late (first frame past the energy check) and
                // understated the wait the user actually feels.
                tracing::info!(
                    latency_ms = first_voice_at.map(|t| t.elapsed().as_millis() as u64),
                    "listening_overlay_shown"
                );
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
    // PERSISTENT cache-aware decode session (Nemotron only; `None` for every
    // other engine). When present it replaces BOTH the speculative
    // whole-utterance decode and the re-decode-the-last-3s preview: the
    // transcript is built as the user speaks, so end-of-speech costs one
    // 560 ms flush window (~54 ms measured) instead of a from-scratch decode
    // (~0.10x realtime — 4 s for a 40 s utterance). Those two old paths also
    // contended with the final decode on the ONE shared ONNX model mutex, so
    // dropping them is a second, independent win.
    let mut live_stream = crate::always::live_stream::LiveStream::start(transcriber);
    // Set when the audio buffer was truncated out from under the session
    // (speaker-gate tail cut / chunk refutation). The decoded state then
    // covers audio the rest of the pipeline has discarded, so the transcript
    // no longer matches `speech_samples` and finalization must fall back to a
    // one-shot decode of the truncated buffer.
    let mut live_invalidated = false;
    // Transcript rescued from the live session when the speaker gate re-bases
    // the buffer. `Some` means "the session cannot keep streaming, but this
    // prefix is a correct final for the truncated audio" — see
    // `LiveStream::transcript_through`.
    let mut live_truncated_transcript: Option<String> = None;
    // Throttle + budget for the re-open attempt below.
    let mut live_start_retry_at: Option<std::time::Instant> = None;
    let mut live_reopens: u32 = 0;
    let mut live_unavailable_logged = false;
    // Whether the tentative-pause work (mid-sentence verdict + grammar warm)
    // has run for THIS silence run. Reset on speech resume / chunk flush,
    // exactly like `midsentence_decided`.
    let mut live_warm_fired = false;
    let mut last_live_preview: Option<(std::time::Instant, String)> = None;
    // Live streaming preview: a lightweight PREVIEW stream that runs WHILE
    // the user is still speaking (the tentative speculation above only fires
    // at a pause). Armed by consume mode OR a genuinely-streaming active
    // engine — see the gating condition below. Independent of the
    // speculation slot / final path — it only re-transcribes the growing
    // buffer and emits a `TranscriptChunk`. The atomic serialises the
    // background transcribes (one round-trip at a time) and is cleared by
    // the thread on completion.
    let preview_pending = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut last_preview_at: Option<std::time::Instant> = None;
    // Total voiced-buffer size (committed + live) at the last preview
    // kickoff. Drives the slow cloud cadence's "enough NEW audio" check
    // so a tick never re-transcribes near-identical audio.
    let mut samples_at_last_preview: usize = 0;
    // Adaptive mid-sentence extension: latched once per silence run when
    // the speculative transcript looks unfinished; reset on speech resume.
    // `decided` records that the speculative text was inspected (either
    // verdict) so the decision grace stops holding the cut.
    let mut midsentence_extended = false;
    let mut midsentence_decided = false;
    // Adaptive EARLY finalization: latched once per silence run when the live
    // transcript reads as a finished sentence AND every voiced sample has been
    // decoded into it. Shortens the final window; reset on speech resume.
    let mut early_finalize_armed = false;
    // Last `early_finalize_skipped` reason emitted for this silence run, so the
    // per-tick instrumentation logs a reason CHANGE rather than the same line
    // every other frame. Reset wherever `early_finalize_armed` is.
    let mut early_finalize_skip_logged: Option<&'static str> = None;
    // One `silence_verdict_state` line per silence run. Reset with the rest.
    let mut verdict_state_logged = false;
    // `speech_samples.len()` as of the end of the last VOICED frame — i.e. how
    // much of the buffer is speech rather than the trailing silence run.
    //
    // This is what makes an early cut safe. `LiveStream::caught_up()` only says
    // the worker decoded everything it was HANDED, and `feed` withholds a
    // partial trailing window of up to `chunk_samples` (560 ms). So a
    // "caught up" transcript can still be missing the last half-second of
    // speech — the very words the end-of-sentence verdict is read from.
    // Comparing this against `LiveStream::fed()` closes that gap exactly:
    // `voiced_len <= fed` means every voiced sample is in the transcript.
    let mut voiced_len: usize = 0;
    // Rolling chunk transcription for long dictations (see chunker.rs).
    // `committed_samples` tracks audio already flushed out of the live
    // buffer so the long-recording warn/cap math still sees the total.
    // `voiced_since_flush` gates the speculation kickoff: after a chunk
    // drain the live buffer holds only trailing silence, which is not
    // worth a speculative STT round trip.
    let mut chunker = crate::always::chunker::ChunkAccumulator::new();
    let mut committed_samples = 0usize;
    let mut voiced_since_flush = true;

    // "My Voice" gate — resolved once per utterance, retried on a short
    // cadence until verified (first check at ~0.3s of voice, then every
    // 0.25s; decisive whole-utterance check at ~2s). A failing decisive
    // check returns DroppedSpeaker immediately, so
    // `speaker_checked == true` below always means "checked and passed
    // (or unverifiable → fail-open)".
    let speaker_gate_ctx = speaker_gate_ctx(cfg);
    let speaker_gate_requested = speaker_gate_ctx.requested;
    let speaker_gate = speaker_gate_ctx.gate;
    let mut speaker_checked = false;
    // Actual voiced audio accumulated (excludes pre-buffer and the
    // trailing-silence frames that also land in `speech_samples`) —
    // the embedding needs real voice, not padded silence.
    let mut voiced_samples = 0usize;
    // Speaker verification ladder + tail monitor (see SPEAKER_TAIL_* /
    // SPEAKER_GATE_UNVERIFIED_* consts). The first check fires as soon as
    // the embedder has enough audio (MIN_EMBED_SAMPLES ≈ 0.3s voiced —
    // the pre-buffer is already in `speech_samples`, so the scored window
    // is really ~0.5s), then every 0.25s of new voice until verified.
    // After verification the cadence relaxes to 0.5s and watches the
    // trailing window so the utterance ends when the USER stops talking,
    // not when the room goes quiet.
    let mut next_speaker_check = crate::always::speaker_embed::MIN_EMBED_SAMPLES;
    let mut tail_fail_streak = 0usize;
    // Set when the mic-conflict watchdog fired mid-capture; turns the
    // finalized utterance into `PreemptedByMicConflict` so the caller
    // keeps the text but skips the paste.
    let mut mic_conflict_preempted = false;
    // The cut requires the user's voice to be absent from the trailing
    // windows for the user's own configured pause tolerance — one check
    // per 0.5s of voiced audio, so silence_secs = 2.2 → 5 checks.
    let speaker_tail_fail_checks =
        (((cfg.silence_secs * 16_000.0) / SPEAKER_TAIL_CHECK_EVERY_SAMPLES as f64).ceil() as usize)
            .max(SPEAKER_TAIL_FAIL_STREAK);
    // Live-buffer length at the last window that matched the user — a
    // tail cut truncates here so foreign trailing audio (movie line
    // after the user's last word) is never sent to STT.
    let mut confirmed_user_len = 0usize;

    loop {
        // A queued "My Voice" enrollment recording needs the mic and the
        // event-loop thread — abandon this capture cycle immediately so
        // the Settings UI's record button responds now, not after a
        // 30s pre-voice timeout. Anything already buffered is discarded
        // (the user just clicked "record my voice"; they are not mid-
        // dictation).
        if crate::always::enrollment::is_pending() {
            event::global_broadcaster().voice_activity_ended();
            flip_to_listening!();
            return Ok(RecordResult::Silence);
        }
        // Another app grabbed the microphone (the mic-conflict watchdog
        // polls every 1s). Stop capturing THIS instant rather than at the
        // top of the next event-loop iteration: `record_utterance` blocks
        // for up to `timeout_secs` waiting for voice, so the loop-level
        // gate in `event_loop` cannot see the flag until the wait ends —
        // which is how the user ended up with the same sentence
        // transcribed twice, once by each app.
        //
        // Only the mic-conflict source aborts mid-capture. The other
        // pause sources deliberately keep running here: `event_loop`
        // calls `process_one` WHILE audio-output-paused for its
        // wake-on-voice path, and aborting on `should_gate_capture()`
        // would cancel exactly the utterance that path exists to record.
        //
        // `break` (not an early return) so whatever was already spoken
        // still flows through the normal transcribe path below — the
        // words are kept and reported, just never pasted.
        if pause::is_mic_conflict_paused() {
            tracing::info!(voiced_samples, "capture_preempted_by_mic_conflict");
            mic_conflict_preempted = true;
            break;
        }
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
        //
        // Skipped entirely when the "My Voice" gate is active
        // (`speaker_gate_requested`): those users enabled the feature
        // precisely so that other people and background media do not
        // register, and an announce-before-verify makes the overlay blink
        // on every non-user voice for the 40ms-4s until the gate rejects
        // it. With the gate on, the announce comes from
        // `speaker_gate_verified` / whole-utterance pass below instead —
        // slightly later, but only ever for the enrolled user.
        if is_speech && first_voice_at.is_none() {
            first_voice_at = Some(std::time::Instant::now());
        }
        if !in_speech && !voice_activity_announced && !speaker_gate_requested {
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
                // Show the listening overlay IMMEDIATELY on voice onset.
                // Without a speaker gate there is nothing to verify against,
                // so waiting would only add 0.2-4.3s of dead time before the
                // mic icon appears (measured live) — the single biggest
                // "it's not fast" complaint. A frame streak that never
                // becomes speech still retracts the overlay via the
                // early-voice false-start path below.
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
            // Mid-utterance "still thinking" pause: the user had already
            // gone quiet for a bit and then kept talking, so this pause
            // never reached the final cutoff. Logged (not just discarded)
            // so silence_secs can eventually be tuned from the user's own
            // observed pause lengths instead of a guess — see
            // `midsentence_decision` for the sibling signal (content-based
            // extension hit rate).
            if in_speech && consecutive_silence > 0 {
                tracing::info!(
                    pause_ms = consecutive_silence as u32 * FRAME_MS,
                    "speech_resumed_after_pause"
                );
            }
            // Speech resumed: discard any pending speculation (its audio snapshot
            // is now stale because more speech will be appended).
            if speculation_pending || live_warm_fired {
                speculation_pending = false;
                speculation_slot.invalidate();
                // Overlay was likely flipped by speculation kickoff (or, on
                // the live-session path, by the tentative-pause verdict).
                // User resumed — flip back. (No-op if already listening.)
                flip_to_listening!();
            }
            midsentence_extended = false;
            midsentence_decided = false;
            early_finalize_armed = false;
            early_finalize_skip_logged = None;
            verdict_state_logged = false;
            live_warm_fired = false;
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

                        // Send voice activity detected event. With the
                        // "My Voice" gate active the overlay waits for
                        // speaker verification instead (see the ladder
                        // below) — unverified audio must stay invisible.
                        if !speaker_gate_requested {
                            announce_voice_activity!();
                        }
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
                        //
                        // "My Voice" active: this voice is UNVERIFIED —
                        // background media was cancelling every countdown
                        // here, so auto-enter never fired while a video
                        // played. The cancel instead happens at speaker
                        // verification (see the ladder below).
                        if !speaker_gate_requested && pause::countdown_active() {
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
                voiced_samples += samples.len();
                voiced_len = speech_samples.len();
                // Same anchor refresh for the case where we were already in
                // speech (consecutive_speech overflows min_speech_frames
                // immediately) — keep the watchdog clock pinned to "right
                // now" for as long as audio is genuinely voice.
                pause::mark_voice_seen();

                // A requested gate with an unavailable embedding engine
                // cannot verify anyone. Wait for a bounded amount of real
                // voiced audio so capture does not spin, then reject before
                // speculation or chunk transcription can spend STT on it.
                if speaker_gate_should_reject_unavailable(
                    speaker_gate_requested,
                    speaker_gate.is_some(),
                    voiced_samples,
                ) {
                    tracing::warn!(voiced_samples, "speaker_gate_unavailable_early_reject");
                    event::global_broadcaster().voice_activity_ended();
                    flip_to_listening!();
                    return Ok(RecordResult::DroppedSpeaker { score: -1.0 });
                }

                // "My Voice" ladder: one check per 0.25s of NEW voiced
                // audio while unverified, 0.5s after verification. The
                // ~30-50ms of inference stalls the read loop briefly;
                // rec's 131KB pipe buffer (~4s) absorbs it.
                //
                // UNVERIFIED phase: a trailing-window match at the full
                // threshold verifies the user (as early as ~0.3s of voice
                // now that MIN_EMBED_SAMPLES is the floor) and only then
                // raises the listening badge — non-user voices never flash
                // it. Still unverified at ~2s → decisive
                // whole-utterance check, ABORT on mismatch — background
                // media dialogue must not hold the recorder hostage for
                // an entire scene.
                //
                // VERIFIED phase: keep scoring the trailing window. The
                // generic VAD can never end the utterance while media
                // keeps "talking" (consecutive_silence never grows), so
                // a mismatch streak is the real end-of-dictation signal:
                // the user stopped, something else holds the mic. Cut at
                // the last matching boundary and finalize what they said.
                if let Some(gate) = &speaker_gate
                    && voiced_samples >= next_speaker_check
                {
                    next_speaker_check = voiced_samples
                        + if speaker_checked {
                            SPEAKER_TAIL_CHECK_EVERY_SAMPLES
                        } else {
                            SPEAKER_GATE_UNVERIFIED_CHECK_EVERY_SAMPLES
                        };
                    let tail_len = SPEAKER_TAIL_WINDOW_SAMPLES.min(speech_samples.len());
                    let window = &speech_samples[speech_samples.len() - tail_len..];
                    if !speaker_checked {
                        let score = speaker_gate_score(gate, window);
                        // `window_threshold`, not `threshold`: this is the
                        // single-window trial, raised while media plays.
                        if speaker_gate_allows_score(score, gate.window_threshold) {
                            let score = score.expect("accepted speaker score must be present");
                            speaker_checked = true;
                            confirmed_user_len = speech_samples.len();
                            tail_fail_streak = 0;
                            tracing::info!(score, "speaker_gate_verified");
                            announce_voice_activity!();
                            // The USER resumed speaking (verified) —
                            // this is the gated equivalent of the
                            // countdown cancel in the voice_logged
                            // block above.
                            if pause::countdown_active() {
                                pause::countdown_request_cancel();
                                tracing::info!("countdown_cancel_on_verified_voice");
                            }
                        } else if voiced_samples >= SPEAKER_GATE_EARLY_SAMPLES {
                            speaker_checked = true;
                            let score = speaker_gate_score(gate, &speech_samples);
                            // `early_abort_threshold`, NOT `threshold`: this
                            // is the one check that destroys the buffer, and
                            // a 2s prefix of the user's own voice does not
                            // clear a whole-utterance bar. See the field doc.
                            if SPEAKER_GATE_ENFORCE_DROP
                                && !speaker_gate_allows_score(score, gate.early_abort_threshold)
                            {
                                let score = score.unwrap_or(-1.0);
                                tracing::info!(
                                    score,
                                    threshold = gate.early_abort_threshold,
                                    utterance_threshold = gate.threshold,
                                    "speaker_gate_early_reject"
                                );
                                event::global_broadcaster().voice_activity_ended();
                                flip_to_listening!();
                                return Ok(RecordResult::DroppedSpeaker { score });
                            }
                            // Whole-utterance pass: arm the tail monitor
                            // and overlay.
                            confirmed_user_len = speech_samples.len();
                            tail_fail_streak = 0;
                            announce_voice_activity!();
                            if pause::countdown_active() {
                                pause::countdown_request_cancel();
                                tracing::info!("countdown_cancel_on_verified_voice");
                            }
                        }
                    } else {
                        // `tail_threshold`, not a fraction of the
                        // whole-utterance bar: this branch truncates the
                        // buffer, and a 1.5s window of the user's own
                        // trailing words does not clear a bar meant for
                        // whole utterances. See the field doc.
                        let tail_threshold = gate.tail_threshold;
                        let score = speaker_gate_score(gate, window);
                        if speaker_gate_allows_score(score, tail_threshold) {
                            tail_fail_streak = 0;
                            confirmed_user_len = speech_samples.len();
                        } else {
                            tail_fail_streak += 1;
                            tracing::debug!(
                                score = ?score,
                                tail_threshold,
                                streak = tail_fail_streak,
                                "speaker_gate_tail_mismatch"
                            );
                            if SPEAKER_GATE_ENFORCE_DROP
                                && tail_fail_streak >= speaker_tail_fail_checks
                            {
                                tracing::info!(
                                    score = ?score,
                                    kept_secs = confirmed_user_len as f64 / 16_000.0,
                                    trimmed_secs = (speech_samples.len() - confirmed_user_len)
                                        as f64
                                        / 16_000.0,
                                    "speaker_gate_tail_cut"
                                );
                                speech_samples.truncate(confirmed_user_len);
                                // The session already decoded past the cut, so it cannot keep
                                // streaming this segment. Its transcript as of the last chunk
                                // boundary at or before the cut is still exactly right, though,
                                // and describes only audio the gate confirmed — take that rather
                                // than re-decode the whole utterance from scratch.
                                //
                                // The "~50 ms" this comment used to weigh against correctness was
                                // the cost of the LIVE path, not of the one-shot path that
                                // replaces it. Measured over the eight real utterances of
                                // 2026-09-02: live finalization 0.15-0.23 s, from-scratch
                                // fallback 7.30 / 9.05 / 12.65 / 40.01 s — to avoid a median
                                // 1.02 s of rejected tail.
                                live_truncated_transcript = live_stream
                                    .as_ref()
                                    .and_then(|live| live.transcript_through(confirmed_user_len));
                                live_invalidated = true;
                                break;
                            }
                        }
                    }
                }

                // LIVE preview: while the user is mid-sentence (this is the
                // voiced branch — no pause needed), re-transcribe the growing
                // buffer on an interval and emit it as a preview. Three
                // independent reasons this can be armed (see
                // `preview_cadence` for the priority):
                // - `is_consume_mode()`: a stream consumer (e.g. Iris) opted
                //   in via `SetConsumeMode`, regardless of engine.
                // - `transcriber.supports_streaming()`: the active engine
                //   genuinely streams (local cache-aware decode, e.g.
                //   Nemotron/MoonshineStreaming — fast, no network round
                //   trip), so the HUD can show live text as SPEC.md §5
                //   promises without waiting for an external consumer to ask.
                // - `cfg.stt_live_preview` (default on): non-streaming cloud
                //   backends (Groq) get a SLOW cadence — one full round trip
                //   every ~1.5s, and only once ~1s of new audio accumulated —
                //   so normal dictation shows provisional text while the
                //   user is still talking without hammering the API.
                // Serialised by `preview_pending`; the slow path additionally
                // yields to a pending speculation so it can never starve the
                // tentative-silence/final transcription. Preview only — the
                // final still comes from the speculation/chunker path,
                // unchanged.
                // A live session already produces previews for free; the
                // old loop would re-decode the last 3 s from scratch on the
                // SAME model mutex the session needs — measured at
                // 318-1966 ms a shot, seven shots in one 30 s utterance,
                // with the final decode queued behind all of them.
                if live_stream.is_none()
                    && let Some(cadence) = preview_cadence(
                        crate::always::pause::is_consume_mode(),
                        transcriber.supports_streaming(),
                        cfg.transcriber_backend.is_local(),
                        cfg.transcriber_backend.is_apple(),
                        cfg.stt_live_preview,
                    )
                    && speaker_gate_allows_stt(speaker_gate_requested, speaker_checked)
                    && speech_samples.len() >= CONSUME_STREAM_MIN_SAMPLES
                    && committed_samples + speech_samples.len()
                        >= samples_at_last_preview + cadence.min_new_samples
                    && !(cadence.require_speculation_idle && speculation_pending)
                    && !preview_pending.load(std::sync::atomic::Ordering::Relaxed)
                    && last_preview_at.is_none_or(|t| {
                        t.elapsed() >= std::time::Duration::from_millis(cadence.interval_ms)
                    })
                {
                    last_preview_at = Some(std::time::Instant::now());
                    samples_at_last_preview = committed_samples + speech_samples.len();
                    preview_pending.store(true, std::sync::atomic::Ordering::Relaxed);
                    if cadence.flip_overlay {
                        flip_to_transcribing!();
                    }
                    // Cap to the last N seconds so a long chunk's preview stays
                    // cheap and can't monopolize the single STT engine ahead of
                    // the final transcription.
                    let preview_len = CONSUME_STREAM_PREVIEW_MAX_SAMPLES.min(speech_samples.len());
                    let audio_snapshot =
                        speech_samples[speech_samples.len() - preview_len..].to_vec();
                    // Slow overlay path only: prefix the already-settled
                    // chunk texts so a long chunked utterance previews as
                    // the whole sentence, not just the open chunk. Cheap
                    // (non-blocking; `None` while any chunk is in flight).
                    let settled_prefix = if cadence.prefix_settled_chunks && !chunker.is_empty() {
                        chunker.join_handle().settled_join()
                    } else {
                        None
                    };
                    let transcriber_for_preview = Arc::clone(transcriber);
                    let rt_for_preview = rt.clone();
                    let flag = Arc::clone(&preview_pending);
                    std::thread::spawn(move || {
                        let preview_started = std::time::Instant::now();
                        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                            || -> Result<crate::stt::TranscriptionResult> {
                                let wav = audio::create_wav_bytes_i16_mono_16k(&audio_snapshot)?;
                                let mut stream = transcriber_for_preview.transcribe_streaming(wav);
                                let mut accumulated = String::new();
                                rt_for_preview.block_on(async {
                                    while let Some(item) = stream.next().await {
                                        let item = item.map_err(anyhow::Error::from)?;
                                        if append_streaming_preview(&mut accumulated, &item.text) {
                                            let display = match settled_prefix.as_deref() {
                                                Some(prefix) if !prefix.is_empty() => {
                                                    format!("{prefix} {accumulated}")
                                                }
                                                _ => accumulated.clone(),
                                            };
                                            event::global_broadcaster().transcript_chunk(display);
                                        }
                                    }
                                    Ok::<(), anyhow::Error>(())
                                })?;
                                Ok(crate::stt::TranscriptionResult {
                                    text: accumulated,
                                    ..Default::default()
                                })
                            },
                        ));
                        match outcome {
                            Ok(Ok(result)) => tracing::info!(
                                chars = result.text.len(),
                                preview_ms = preview_started.elapsed().as_millis() as u64,
                                "live_preview_sent"
                            ),
                            Ok(Err(error)) => {
                                tracing::warn!(error = ?error, "streaming_preview_failed")
                            }
                            Err(error) => {
                                tracing::warn!(error = ?error, "streaming_preview_failed")
                            }
                        }
                        flag.store(false, std::sync::atomic::Ordering::Relaxed);
                    });
                }

                // Per-chunk ceiling: a pause-free monologue never reaches
                // the tentative-silence flush point, so cut at a frame
                // boundary once the chunk is oversized.
                if speech_samples.len()
                    >= effective_chunk_hard_max_secs(live_session_carrying(
                        live_stream.as_ref(),
                        live_invalidated,
                    )) as usize
                        * 16_000
                    && speaker_gate_allows_stt(speaker_gate_requested, speaker_checked)
                {
                    // A chunk leaves this buffer for good, so the
                    // end-of-utterance confirmation below can never see it.
                    // Confirm it as a whole here instead: a latched
                    // `speaker_checked` must not be able to commit 15s of
                    // media into the chunker. Refuted → cut at the last
                    // window-verified boundary and finalize, exactly like
                    // `speaker_gate_tail_cut`.
                    if let Some(gate) = &speaker_gate
                        && SPEAKER_GATE_ENFORCE_DROP
                        && let SpeakerConfirmation::Refuted(score) =
                            speaker_gate_confirm_utterance(gate, &speech_samples, voiced_samples)
                    {
                        tracing::info!(
                            score,
                            threshold = gate.threshold,
                            kept_secs = confirmed_user_len as f64 / 16_000.0,
                            "speaker_gate_chunk_refuted"
                        );
                        speech_samples.truncate(confirmed_user_len);
                        // The session already decoded past the cut, so it cannot keep
                        // streaming this segment. Its transcript as of the last chunk
                        // boundary at or before the cut is still exactly right, though,
                        // and describes only audio the gate confirmed — take that rather
                        // than re-decode the whole utterance from scratch.
                        //
                        // The "~50 ms" this comment used to weigh against correctness was
                        // the cost of the LIVE path, not of the one-shot path that
                        // replaces it. Measured over the eight real utterances of
                        // 2026-09-02: live finalization 0.15-0.23 s, from-scratch
                        // fallback 7.30 / 9.05 / 12.65 / 40.01 s — to avoid a median
                        // 1.02 s of rejected tail.
                        live_truncated_transcript = live_stream
                            .as_ref()
                            .and_then(|live| live.transcript_through(confirmed_user_len));
                        live_invalidated = true;
                        break;
                    }
                    tracing::info!(
                        chunk_secs = speech_samples.len() / 16_000,
                        // Why chunking was not bypassed. `carrying` here means
                        // a healthy live session ran past the 120 s safety
                        // valve; anything else names the failure that put the
                        // 15 s ceiling back in play.
                        live = live_carrying_reason(live_stream.as_ref(), live_invalidated),
                        "chunk_hard_flush"
                    );
                    committed_samples += speech_samples.len();
                    let grammar = if cfg.postprocess_available() {
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
                    // The committed audio left the live buffer, so the
                    // session must start a fresh segment covering only the
                    // new tail — `finalize_chunked` appends the tail to the
                    // separately-decoded chunks, and a session still holding
                    // the committed words would duplicate them.
                    if let Some(live) = live_stream.as_mut() {
                        live.reset();
                    }
                    live_warm_fired = false;
                    last_live_preview = None;
                    voiced_since_flush = false;
                    // The buffer the live session is fed from was just
                    // drained and re-based, so both the "how much is voiced"
                    // counter and any early verdict drawn from the old
                    // transcript are meaningless now.
                    voiced_len = 0;
                    early_finalize_armed = false;
                    early_finalize_skip_logged = None;
                    verdict_state_logged = false;
                    // The tail monitor's boundary points into the drained
                    // buffer — rebase to the fresh (empty) one.
                    confirmed_user_len = 0;
                    tail_fail_streak = 0;
                }
            }
        } else if in_speech {
            if keyboard::is_option_held() {
                consecutive_silence = 0;
                consecutive_speech = 0;
                speech_samples.extend_from_slice(samples);
                // Held-Option audio is deliberately kept, so it counts as
                // "not yet decoded speech" for the early-finalize guard.
                // Over-counting here can only make the guard stricter.
                voiced_len = speech_samples.len();
                early_finalize_armed = false;
                early_finalize_skip_logged = None;
                verdict_state_logged = false;
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
            // Always fire the first speculative round at the fast (~240ms)
            // cadence, not just in consume mode. This only moves up WHEN the
            // background peek transcription kicks off (`transcript_chunk`
            // preview) — the actual final-silence cut below is unchanged, so
            // dictation's paste latency is unaffected. It exists because a
            // stream consumer (e.g. Iris's wake-word redirect) watches these
            // early previews to decide whether to intercept the utterance
            // BEFORE the final paste commits; waiting until the old
            // ~20% (normal) / ~50% (short-utterance) tentative mark left
            // too thin a margin for that round trip and made the redirect
            // race the paste — sometimes losing it even when the wake word
            // was spoken and transcribed correctly.
            let eff_tentative_frames =
                CONSUME_STREAM_TENTATIVE_FRAMES.min(eff_silence_frames.saturating_sub(1).max(1));

            // At tentative silence with a target-sized buffer, COMMIT the
            // chunk instead of speculating: flush it for background
            // transcription (never discarded — see chunker.rs) and keep
            // recording into a fresh buffer. The seam lands inside a
            // silence region at a frame boundary, so no word is split.
            if voice_logged
                && consecutive_silence >= eff_tentative_frames
                && speech_samples.len()
                    >= effective_chunk_target_secs(live_session_carrying(
                        live_stream.as_ref(),
                        live_invalidated,
                    )) as usize
                        * 16_000
                && speaker_gate_allows_stt(speaker_gate_requested, speaker_checked)
            {
                // Same reason as the hard flush above: confirm the chunk as
                // a whole before it leaves the buffer, so a single latched
                // window cannot commit media speech into the chunker.
                if let Some(gate) = &speaker_gate
                    && SPEAKER_GATE_ENFORCE_DROP
                    && let SpeakerConfirmation::Refuted(score) =
                        speaker_gate_confirm_utterance(gate, &speech_samples, voiced_samples)
                {
                    tracing::info!(
                        score,
                        threshold = gate.threshold,
                        kept_secs = confirmed_user_len as f64 / 16_000.0,
                        "speaker_gate_chunk_refuted"
                    );
                    speech_samples.truncate(confirmed_user_len);
                    // The session already decoded past the cut, so it cannot keep
                    // streaming this segment. Its transcript as of the last chunk
                    // boundary at or before the cut is still exactly right, though,
                    // and describes only audio the gate confirmed — take that rather
                    // than re-decode the whole utterance from scratch.
                    //
                    // The "~50 ms" this comment used to weigh against correctness was
                    // the cost of the LIVE path, not of the one-shot path that
                    // replaces it. Measured over the eight real utterances of
                    // 2026-09-02: live finalization 0.15-0.23 s, from-scratch
                    // fallback 7.30 / 9.05 / 12.65 / 40.01 s — to avoid a median
                    // 1.02 s of rejected tail.
                    live_truncated_transcript = live_stream
                        .as_ref()
                        .and_then(|live| live.transcript_through(confirmed_user_len));
                    live_invalidated = true;
                    break;
                }
                // Chunking a streaming utterance is a large net loss (see
                // `STREAM_CHUNK_TARGET_SECS`), so reaching here on a machine
                // whose engine streams means the live session was NOT
                // carrying. Say which of the three reasons it was — that is
                // the whole diagnosis for "chunk_flush still fires".
                tracing::info!(
                    chunk_secs = speech_samples.len() / 16_000,
                    target_secs = effective_chunk_target_secs(live_session_carrying(
                        live_stream.as_ref(),
                        live_invalidated,
                    )),
                    live = live_carrying_reason(live_stream.as_ref(), live_invalidated),
                    "chunk_tentative_flush"
                );
                committed_samples += speech_samples.len();
                let grammar = if cfg.postprocess_available() {
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
                // Same re-base as the hard-max flush above.
                if let Some(live) = live_stream.as_mut() {
                    live.reset();
                }
                live_warm_fired = false;
                last_live_preview = None;
                midsentence_extended = false;
                midsentence_decided = false;
                early_finalize_armed = false;
                early_finalize_skip_logged = None;
                verdict_state_logged = false;
                voiced_len = 0;
                voiced_since_flush = false;
                // The tail monitor's boundary points into the drained
                // buffer — rebase to the fresh (empty) one.
                confirmed_user_len = 0;
                tail_fail_streak = 0;
            }

            // "My Voice": verify the speaker BEFORE any speculative STT
            // can leak a stranger's words into the overlay preview.
            // Short utterances (< 1s of voice) can't be verified yet —
            // they skip speculation and get re-checked at final.
            if voice_logged
                && voiced_since_flush
                && !speculation_pending
                && consecutive_silence >= eff_tentative_frames
                && let Some(gate) = &speaker_gate
                && !speaker_checked
                && voiced_samples >= crate::always::speaker_embed::MIN_EMBED_SAMPLES
            {
                let score = speaker_gate_score(gate, &speech_samples);
                if SPEAKER_GATE_ENFORCE_DROP && !speaker_gate_allows_score(score, gate.threshold) {
                    let score = score.unwrap_or(-1.0);
                    tracing::info!(
                        score,
                        threshold = gate.threshold,
                        "speaker_gate_tentative_reject"
                    );
                    event::global_broadcaster().voice_activity_ended();
                    flip_to_listening!();
                    return Ok(RecordResult::DroppedSpeaker { score });
                }
                speaker_checked = true;
                // Verified: arm the tail monitor in case speech resumes
                // on top of media.
                confirmed_user_len = speech_samples.len();
                tail_fail_streak = 0;
            }
            let speculation_speaker_ok =
                speaker_gate_allows_stt(speaker_gate_requested, speaker_checked);

            // At tentative silence, kick off speculative transcription in the
            // background so the result is ready (or nearly so) by the time we
            // hit final silence. If the user resumes, we discard it above.
            // Skipped after a chunk drain until real speech lands in the
            // fresh buffer — trailing silence isn't worth an STT round trip.
            if live_stream.is_none()
                && voice_logged
                && voiced_since_flush
                && !speculation_pending
                && speculation_speaker_ok
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
                let grammar_warm = if cfg.postprocess_available() {
                    cfg.post_processor.clone()
                } else {
                    None
                };
                let rt_for_warm = rt.clone();
                // For the warm's target text: a chunked utterance pastes
                // the JOIN of the committed chunks + this tail, so the
                // warm must key on that join, not the tail alone.
                let chunk_join = chunker.join_handle();
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
                    //
                    // Chunked utterance: the paste key is over
                    // join(corrected chunks) + tail (see
                    // `finalize_chunked`), so warm THAT — but only once
                    // the join is deterministic (every chunk's grammar
                    // settled); an unsettled join would warm a key
                    // finalize never asks for.
                    if let Some(text) = warm_text
                        && captured_gen == slot.current_generation()
                        && let Some(pp) = grammar_warm
                    {
                        let warm_target = if chunk_join.chunk_count() > 0 {
                            chunk_join.settled_join().map(|joined| {
                                // Byte-identical to finalize_chunked's
                                // assembly: trimmed tail, single-space
                                // separator, joined-only when the tail is
                                // empty.
                                let tail = text.trim();
                                if joined.is_empty() {
                                    tail.to_string()
                                } else if tail.is_empty() {
                                    joined
                                } else {
                                    format!("{joined} {tail}")
                                }
                            })
                        } else {
                            Some(text)
                        };
                        // Build through the SAME request builder as the
                        // paste path — identical tiering, candidates, and
                        // session context mean an identical cache key, so
                        // the paste path's grammar call is a cache hit.
                        // Same skip conditions as the paste path: short
                        // utterances bypass grammar, oversized joins skip
                        // the blocking pass.
                        if let Some(target) = warm_target
                            && !crate::always::event_loop::is_short_utterance(&target)
                            && target.chars().count()
                                <= crate::always::event_loop::GRAMMAR_MAX_CHARS
                        {
                            let req =
                                crate::always::correction_request::build(&target, pp.can_correct());
                            rt_for_warm.spawn(async move {
                                let _ = pp.process_request(&req).await;
                            });
                        }
                    }
                });
            }

            // LIVE SESSION tentative-pause work. Same job the speculation
            // thread used to do — decide whether the user stopped
            // mid-thought, and pre-pay the grammar LLM — but the transcript
            // already exists, so there is nothing to wait for. Two
            // consequences:
            //   * `MIDSENTENCE_DECISION_GRACE_FRAMES` (600 ms of held cut,
            //     spent waiting for a speculative decode) is never armed:
            //     `adaptive_active` requires `speculation_pending`.
            //   * The grammar warm starts at the tentative pause rather than
            //     after a whole-utterance re-decode — ~600 ms earlier.
            //
            // Re-evaluated every 2nd frame until the extension latches,
            // because the worker may still be a window behind: the heuristic
            // reads the TRAILING word, and judging it before the last chunk
            // is decoded would look at the wrong word. `caught_up` gates the
            // first verdict for the same reason.
            //
            // Every evaluation now RECORDS ITS OUTCOME: either a
            // `midsentence_decision` / `early_finalize_decision` line, or
            // `early_finalize_skipped` naming the exact precondition that was
            // false. The fast path used to log only on success, so a
            // precondition that was false on every frame of every utterance
            // was completely invisible — which is how the mid-sentence branch
            // permanently short-circuiting the early branch (see
            // `looks_mid_sentence_live`) stayed invisible.
            //
            // The gate's own terms are themselves instrumented
            // (`silence_verdict_state`), because the FIRST round of this fix
            // put the reason log INSIDE the guard: when the guard itself was
            // false the block never ran, no reason was computed, and the
            // blocker was invisible one level further up. A gate that can be
            // false must say so from OUTSIDE itself.
            //
            // `voiced_since_flush` is deliberately NOT one of them. Its
            // documented job is "after a chunk drain the live buffer holds
            // only trailing silence, which is not worth a speculative STT
            // round trip" — that reasoning is about SPENDING a decode, and
            // applies to the speculation kickoff above, which still carries
            // it. The live verdict spends nothing; it READS a transcript that
            // already exists. Gating it here meant that any utterance long
            // enough to chunk lost its verdict for the entire remaining
            // silence run — `voiced_since_flush` is cleared by the flush and
            // only ever set true again by a VOICED frame, so when the flushing
            // pause was also the end of the utterance (the common case: the
            // user stopped) neither branch could run again. Removing it cannot
            // cut anyone off: the flush calls `live.reset()`, so
            // `live.transcript()` is `None` until real speech is decoded
            // again, which is the `no_transcript` skip, which leaves the
            // window exactly as configured.
            // ONE unconditional line per silence run, at the tentative mark,
            // naming the value of every input to the verdict guard below.
            // This is the log that cannot be starved by the thing it is
            // measuring: it is emitted before the guard and does not depend on
            // any of the guard's terms.
            // Latched, not `== eff_tentative_frames`: `is_short` can flip
            // mid-silence (the trailing silence frames grow `speech_samples`),
            // which moves `eff_tentative_frames` and could step over an
            // equality test. This log exists to be unmissable.
            if consecutive_silence >= eff_tentative_frames && !verdict_state_logged {
                verdict_state_logged = true;
                tracing::info!(
                    live_invalidated,
                    voice_logged,
                    voiced_since_flush,
                    speculation_speaker_ok,
                    speaker_gate_requested,
                    speaker_checked,
                    adaptive = cfg.adaptive_silence_enabled,
                    is_short,
                    tentative_frames = eff_tentative_frames,
                    base_frames = eff_silence_frames,
                    early_frames = complete_utterance_silence_frames(eff_silence_frames),
                    live = live_carrying_reason(live_stream.as_ref(), live_invalidated),
                    live_caught_up = live_stream.as_ref().is_some_and(|l| l.caught_up()),
                    live_fed = live_stream.as_ref().map_or(0, |l| l.fed()),
                    live_chars = live_stream
                        .as_ref()
                        .and_then(|l| l.transcript())
                        .map_or(0, |t| t.chars().count()),
                    voiced_len,
                    buffered_secs = speech_samples.len() as f64 / 16_000.0,
                    committed_secs = committed_samples as f64 / 16_000.0,
                    "silence_verdict_state"
                );
            }
            let early_tick = verdict_tick_allowed(VerdictTick {
                live_invalidated,
                voice_logged,
                speculation_speaker_ok,
                consecutive_silence,
                tentative_frames: eff_tentative_frames,
                base_frames: eff_silence_frames,
                midsentence_decided,
            });
            if early_tick {
                let early_frames = complete_utterance_silence_frames(eff_silence_frames);
                let mut words = 0usize;
                let skip: Option<&'static str> = if midsentence_extended {
                    Some("already_extended")
                } else if let Some(live) = live_stream.as_ref() {
                    if !live.caught_up() {
                        Some("worker_behind")
                    } else if let Some(spec_text) = live.transcript() {
                        if !live_warm_fired {
                            live_warm_fired = true;
                            // Overlay -> Transcribing at the same point the
                            // speculative decode used to flip it.
                            flip_to_transcribing!();
                            let grammar = if cfg.postprocess_available() {
                                cfg.post_processor.clone()
                            } else {
                                None
                            };
                            spawn_grammar_warm(&spec_text, grammar, &chunker.join_handle(), rt);
                        }
                        words = spec_text.split_whitespace().count();
                        // Is every VOICED sample actually represented in
                        // `spec_text`?
                        //
                        // `caught_up()` only proves the worker drained what it
                        // was handed, and `feed` deliberately withholds a
                        // partial trailing window of up to `chunk_samples`
                        // (560 ms). At the 240 ms tentative mark that remainder
                        // still holds real speech more often than not, so the
                        // verdict was routinely read off a transcript missing
                        // the user's last words — which reads as "unfinished"
                        // almost by construction and extended the window on
                        // utterances that were in fact complete. The trailing
                        // silence keeps flowing into `feed`, so this becomes
                        // true on its own within one chunk; until then, no
                        // verdict.
                        let tail_decoded = voiced_len <= live.fed();
                        if !cfg.adaptive_silence_enabled {
                            Some("adaptive_disabled")
                        } else if is_short {
                            Some("short_utterance")
                        } else if !tail_decoded {
                            Some("tail_not_decoded")
                        } else if looks_mid_sentence_live(&cfg.localization, &spec_text) {
                            midsentence_extended = true;
                            midsentence_decided = true;
                            tracing::info!(
                                extended = true,
                                decided_at_frame = consecutive_silence,
                                base_frames = eff_silence_frames,
                                extended_frames = extended_silence_frames(silence_frames),
                                source = "live_stream",
                                "midsentence_decision"
                            );
                            None
                        } else if consecutive_silence < early_frames {
                            // Complete-looking, but the user has not been
                            // quiet long enough yet. Deliberately NOT latched:
                            // the next tick re-reads a longer transcript, so a
                            // resumed thought still wins.
                            Some("silence_below_early_window")
                        } else if !looks_complete_utterance(&cfg.localization, &spec_text) {
                            Some("not_complete_utterance")
                        } else {
                            // Finished sentence, fully decoded, and the user
                            // has been quiet for the early window: end it now
                            // instead of serving out the rest of the
                            // configured silence.
                            early_finalize_armed = true;
                            midsentence_decided = true;
                            tracing::info!(
                                early = true,
                                decided_at_frame = consecutive_silence,
                                base_frames = eff_silence_frames,
                                early_frames,
                                words,
                                source = "live_stream",
                                "early_finalize_decision"
                            );
                            None
                        }
                    } else {
                        Some("no_transcript")
                    }
                } else {
                    Some("no_live_session")
                };
                // Once per distinct reason per silence run. The tick fires
                // every other frame, so logging unconditionally would be ~15
                // identical lines an utterance and no extra information; a
                // reason CHANGE is the interesting event.
                if let Some(reason) = skip
                    && early_finalize_skip_logged != Some(reason)
                {
                    early_finalize_skip_logged = Some(reason);
                    tracing::info!(
                        reason,
                        at_frame = consecutive_silence,
                        base_frames = eff_silence_frames,
                        early_frames,
                        words,
                        live = live_carrying_reason(live_stream.as_ref(), live_invalidated),
                        "early_finalize_skipped"
                    );
                }
            }

            // Adaptive mid-sentence extension: once the speculative
            // outcome lands, inspect the text (non-consuming; every 2nd
            // frame to keep the hot loop cheap) and stretch the final
            // window when it ends mid-thought. Decided at most once per
            // silence run; both flags reset on speech resume.
            let adaptive_active = cfg.adaptive_silence_enabled && !is_short && speculation_pending;
            if adaptive_active
                && !midsentence_decided
                && consecutive_silence.is_multiple_of(2)
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
            } else if early_finalize_armed {
                // Only reachable from the live-session verdict above, which
                // requires a fully-decoded transcript that ends a sentence.
                complete_utterance_silence_frames(eff_silence_frames)
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
            // A speech-like blip that died before anything was announced:
            // drop the latency reference with it. Left set, the next real
            // utterance in this same recording call would measure its
            // badge latency from the stale blip — the same class of bogus
            // multi-second `latency_ms` values as the retraction leak
            // fixed below.
            if !voice_activity_announced && early_voice_streak == 0 {
                first_voice_at = None;
            }
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
                    // Reset the latency reference too. It is set once per
                    // speech-like frame and was never cleared, so after a
                    // retracted false start it kept measuring from the
                    // FIRST sound of the whole recording call rather than
                    // from the utterance actually being announced. That
                    // made `listening_overlay_shown.latency_ms` report
                    // multi-second values for badges that appeared in
                    // 13 ms — observed at 20467 ms — and sent a previous
                    // investigation chasing a latency problem that did
                    // not exist.
                    first_voice_at = None;
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

        // The session is opened once at the top of `record`, which on a cold
        // daemon can be BEFORE the model finished loading:
        // `PendingTranscriber::open_live_stream` is a deliberately
        // non-blocking `try_lock` and answers `None` while the load thread
        // holds the slot. That single `None` used to stand for the whole
        // utterance — no live transcript (so `early_finalize_skipped`
        // `no_live_session` on every tick) and `effective_chunk_target_secs`
        // back to 6 s, i.e. `chunk_flush` on an engine that streams.
        //
        // Retrying is safe and complete: `feed` starts from offset 0, so a
        // session opened mid-utterance still decodes `speech_samples` from its
        // first sample, and after a chunk flush that buffer IS the tail that
        // `finalize_chunked` appends. Throttled, and only while the active
        // engine says it can stream, so a cloud backend never pays for it.
        //
        // A DEGRADED session counts as absent. This is the second half of the
        // same hole: `degraded()` latches on one worker decode error or a
        // queue that fell 16 windows behind, and a degraded session is
        // present-but-dead — `feed` returns immediately, `transcript()` is
        // `None`, `finish()` is `None`, and `live_session_carrying` goes
        // false, which puts the 6 s chunk target back in force for the rest of
        // the utterance. Gating the retry on `is_none()` alone meant that
        // state was permanent once entered. Re-opening is complete, not
        // partial: the new session starts at `fed = 0` on a fresh generation
        // and re-decodes the current buffer from its first sample.
        //
        // Capped per utterance so a genuinely broken engine degrades to
        // chunking (which has its own retries and spill) instead of thrashing.
        let live_dead = live_stream.as_ref().is_none_or(|live| live.degraded());
        if live_dead
            && !live_invalidated
            && live_reopens < MAX_LIVE_REOPENS
            && live_start_retry_at.is_none_or(|at: std::time::Instant| {
                at.elapsed() >= std::time::Duration::from_millis(LIVE_START_RETRY_MS)
            })
        {
            // Stamp before the engine query, not after, so a non-streaming
            // backend is asked once every 300 ms rather than every 30 ms frame.
            live_start_retry_at = Some(std::time::Instant::now());
            if transcriber.supports_streaming() {
                let was_degraded = live_stream.is_some();
                if let Some(fresh) = crate::always::live_stream::LiveStream::start(transcriber) {
                    live_stream = Some(fresh);
                    live_warm_fired = false;
                    voiced_len = speech_samples.len();
                    if was_degraded {
                        live_reopens += 1;
                        tracing::info!(
                            reopens = live_reopens,
                            buffered_secs = speech_samples.len() as f64 / 16_000.0,
                            "live_stream_reopened"
                        );
                    }
                } else if was_degraded {
                    // Could not replace it; leave the dead handle in place so
                    // `live_carrying_reason` still reports `degraded` rather
                    // than silently becoming `no_session`.
                    live_reopens += 1;
                    tracing::warn!("live_stream_reopen_failed");
                } else if !live_unavailable_logged {
                    // The engine claims it streams but will not open a
                    // session. This is a real configuration class, not a
                    // transient: `supports_streaming` is a per-model registry
                    // constant, while `open_live_stream` additionally requires
                    // the engine to actually be Nemotron — the
                    // `moonshine-*-streaming-*` entries declare the flag and
                    // then answer `None` forever. Said once per utterance so
                    // the retry loop above cannot flood the log.
                    live_unavailable_logged = true;
                    tracing::warn!("live_stream_unavailable_despite_supports_streaming");
                }
            }
        }
        // Hand the live session every complete 560 ms window captured so far.
        // O(n) copy + channel send on this thread; the ~54 ms decode happens
        // on the session's worker. `feed` tracks its own boundary, so calling
        // it every frame is cheap and idempotent.
        if !live_invalidated && let Some(live) = live_stream.as_mut() {
            live.feed(&speech_samples);
        }
        // Live preview straight off the session — no extra decode, no model
        // lock, and (unlike the old path) the CUMULATIVE transcript rather
        // than concatenated per-chunk fragments, which split words mid-token
        // ("whe ther", "finali zes") because the tokenizer emits sub-words.
        if !live_invalidated
            && voice_logged
            && speaker_gate_allows_stt(speaker_gate_requested, speaker_checked)
            && let Some(live) = live_stream.as_ref()
            && let Some(text) = live.transcript()
        {
            let due = last_live_preview.as_ref().is_none_or(|(at, last)| {
                *last != text
                    && at.elapsed()
                        >= std::time::Duration::from_millis(LIVE_STREAM_PREVIEW_MIN_GAP_MS)
            });
            if due {
                let display = match chunker.join_handle().settled_join() {
                    Some(prefix) if !prefix.is_empty() => format!("{prefix} {text}"),
                    _ => text.clone(),
                };
                event::global_broadcaster().transcript_chunk(display);
                last_live_preview = Some((std::time::Instant::now(), text));
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

    // "My Voice" final check: utterances that ended before the early
    // (~2s) or tentative checks could verify the speaker are checked
    // here, before any STT spend. Fail CLOSED: a snippet too short to
    // even embed (< 0.5s of voice) is dropped, not pasted — background
    // media constantly produces short bursts, and one stranger's
    // "Okay." landing in the user's editor breaks the entire promise
    // of the gate. (Observed live: a movie-voice tail transcribed as
    // "Okay." and pasted under the old fail-open rule.)
    if speaker_gate_requested && !speaker_checked {
        let threshold = speaker_gate
            .as_ref()
            .map_or(cfg.speaker_gate_threshold as f32, |gate| gate.threshold);
        let score = speaker_gate.as_ref().and_then(|gate| {
            (voiced_samples >= crate::always::speaker_embed::MIN_EMBED_SAMPLES)
                .then(|| speaker_gate_score(gate, &speech_samples))
                .flatten()
        });
        if SPEAKER_GATE_ENFORCE_DROP && !speaker_gate_allows_transcription(true, score, threshold) {
            if speaker_gate.is_none() {
                tracing::warn!("speaker_gate_unavailable_reject");
            } else if voiced_samples < crate::always::speaker_embed::MIN_EMBED_SAMPLES {
                tracing::info!(voiced_samples, "speaker_gate_dropped_unverifiable_short");
            } else {
                tracing::info!(score = ?score, threshold, "speaker_gate_final_reject");
            }
            event::global_broadcaster().voice_activity_ended();
            flip_to_listening!();
            return Ok(RecordResult::DroppedSpeaker {
                score: score.unwrap_or(-1.0),
            });
        }
    }

    // "My Voice" END-OF-UTTERANCE CONFIRMATION — the check the leak got
    // past. The block above only runs when the ladder never verified;
    // once a single 1.5s window latched `speaker_checked`, NOTHING
    // re-examined the audio, and the tail monitor's 0.6x bar (0.21 at
    // the user's 0.35 pref) is cleared by ordinary media speech, so the
    // recording kept growing and the whole thing was pasted.
    //
    // Observed live (2026-08-31 18:41:47 UTC): one window scored 0.3655,
    // latched, and 6.3s of Hindi YouTube dialogue was transcribed and
    // pasted — while the tail check on the very same audio was reporting
    // 0.1667. Re-scoring the kept buffer as a whole is what catches
    // that: it is the same statistic the early/tentative/final checks
    // already use, at the same threshold the user configured, and across
    // the whole incident window it never once accepted media (42 samples,
    // max 0.3407).
    //
    // Scope is deliberately narrow: this can only turn an ACCEPT into a
    // reject, and only on positive evidence that the audio is not the
    // user's. `Insufficient` (too short to embed, or the embedder
    // errored) defers to the ladder exactly as before.
    if speaker_gate_requested
        && speaker_checked
        && let Some(gate) = &speaker_gate
        && let SpeakerConfirmation::Refuted(score) =
            speaker_gate_confirm_utterance(gate, &speech_samples, voiced_samples)
        && SPEAKER_GATE_ENFORCE_DROP
    {
        if has_chunks {
            // Committed chunks were each confirmed at flush time, so the
            // user's already-transcribed speech is NOT thrown away — only
            // this unverified tail is.
            tracing::info!(
                score,
                threshold = gate.threshold,
                tail_secs = speech_samples.len() as f64 / 16_000.0,
                "speaker_gate_tail_refuted_keeping_chunks"
            );
            // The chunks still get assembled and pasted, so the badge must
            // say Transcribing exactly as it does on the silent-tail path.
            flip_to_transcribing!();
            return finalize_chunked(
                &chunker,
                transcriber,
                String::new(),
                &crate::stt::TranscriptionResult::default(),
                committed_samples,
                speech_energy.max(cfg.energy_threshold),
                speech_end_at,
                false,
            )
            .map(|r| apply_mic_conflict_preemption(r, mic_conflict_preempted));
        }
        tracing::info!(
            score,
            threshold = gate.threshold,
            secs = speech_samples.len() as f64 / 16_000.0,
            "speaker_gate_utterance_refuted"
        );
        event::global_broadcaster().voice_activity_ended();
        flip_to_listening!();
        return Ok(RecordResult::DroppedSpeaker { score });
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
        )
        .map(|r| apply_mic_conflict_preemption(r, mic_conflict_preempted));
    }

    // Try to use the speculative transcription if it was kicked off and
    // wasn't invalidated by a speech resume. Wait if it's still in
    // flight — it has been running for up to (silence_secs - tentative)
    // seconds already, so it's likely complete or close to it. The wait
    // cap scales with utterance length: a flat 10s meant a 2-minute
    // monologue routinely timed out, threw the near-done speculative
    // result away, and re-transcribed the ENTIRE audio from scratch —
    // doubling the worst wait exactly when it was already longest.
    // FINALIZATION. With a live session the transcript is already built: the
    // worker has consumed everything except at most the last 560 ms window,
    // so `finish` is one tail chunk plus one flush — 51-54 ms measured, flat
    // from a 5 s utterance to a 2-minute one, against 514 ms / 4200 ms /
    // 11512 ms for the from-scratch decode of the same 4.7 s / 39.6 s /
    // 119.7 s clips.
    //
    // `None` (degraded session, decode error, timeout) and an EMPTY transcript
    // both fall through to the untouched speculation/one-shot path below. The
    // empty case matters: one-shot Nemotron returns an empty decode often
    // enough that `chunker` carries a dedicated retry for it, so a blank live
    // result must never become a silent `DroppedNoise`.
    // Why the live path did or did not carry this utterance, on EVERY
    // utterance. `stt_wait_ms` scaling with utterance length is the signature
    // of a one-shot decode (~0.10x realtime); flat ~130 ms is the signature of
    // live finalization. Without this line the two are indistinguishable in
    // the log and the question "is the session actually carrying?" can only be
    // answered by inference.
    tracing::info!(
        live = live_carrying_reason(live_stream.as_ref(), live_invalidated),
        reopens = live_reopens,
        fed = live_stream.as_ref().map_or(0, |l| l.fed()),
        voiced_len,
        buffered_secs = speech_samples.len() as f64 / 16_000.0,
        committed_secs = committed_samples as f64 / 16_000.0,
        chunks = chunker.chunk_count(),
        speculation_pending,
        "live_final_state"
    );
    let live_final = if live_invalidated {
        // A speaker-gate truncation. Not a dead end any more: the session's
        // transcript at the last chunk boundary at or before the cut is a
        // correct final for the audio that survived, and costs a clone
        // instead of a full re-decode. `None` here (deep cut, degraded
        // session, nothing decoded yet) still falls through unchanged.
        let rescued = live_truncated_transcript.take();
        tracing::info!(
            rescued = rescued.is_some(),
            chars = rescued.as_ref().map_or(0, |t| t.chars().count()),
            kept_secs = speech_samples.len() as f64 / 16_000.0,
            "live_truncation_rollback"
        );
        rescued.filter(|t| !t.trim().is_empty())
    } else if let Some(live) = live_stream.as_mut() {
        let started_finish = std::time::Instant::now();
        let out = live.finish(&speech_samples, crate::always::live_stream::FINISH_TIMEOUT);
        tracing::info!(
            finish_ms = started_finish.elapsed().as_millis() as u64,
            chars = out.as_ref().map_or(0, |t| t.chars().count()),
            "live_stream_finalized"
        );
        out.filter(|t| !t.trim().is_empty())
    } else {
        None
    };

    let speculation = if live_final.is_some() {
        None
    } else if speculation_pending {
        let started_wait = std::time::Instant::now();
        let audio_secs = speech_samples.len() as f64 / 16_000.0;
        // Floor 2s → 0.35s. The 2s was still sized for a cloud round trip, and
        // the comment's own reasoning ("a local re-transcribe costs ~300ms")
        // argues against it: on a miss we wait 2s and THEN spend ~300ms
        // re-decoding, so the floor is ~1.7s of pure dead time.
        //
        // This is not a rare path. Measured over 56 real utterances the
        // speculation landed exactly ONCE (stt_wait_ms < 50ms); 32 took over
        // 1.5s. 2000ms floor + ~900ms re-decode ≈ the observed 2920ms median.
        // 0.35s still covers a healthy local speculation and caps the miss.
        let max_wait = std::time::Duration::from_secs_f64((audio_secs * 0.5).clamp(0.35, 60.0));
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
        _ if live_final.is_some() => {
            event::global_broadcaster().transcribing_stopped();
            let text = live_final.unwrap_or_default().trim().to_string();
            (
                crate::stt::TranscriptionResult {
                    text,
                    duration: speech_samples.len() as f64 / 16_000.0,
                    language: cfg.lang.clone(),
                    // Deliberately empty, exactly as `finalize_chunked` does:
                    // there are no per-segment timings to expose, and the
                    // segment-based hallucination heuristics must not judge a
                    // transcript that never had segments.
                    segments: vec![],
                },
                true,
            )
        }
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
            //
            // CRITICAL LATENCY FIX: the heartbeat must poll the stop flag
            // frequently. The previous version slept a full `from_secs(2)`
            // as its FIRST action, so `join()` below blocked until that 2s
            // sleep expired even though local parakeet finishes in
            // ~100-300ms — injecting a flat ~2000ms tax on EVERY
            // non-speculative utterance (measured: a 33-char clip logged
            // stt_wait_ms=2005, identical to a 936-char one — proof it was
            // the timer, not compute). We keep the 2s heartbeat CADENCE but
            // tick the sleep in 50ms slices so `join()` returns within ~50ms
            // of transcription completing.
            let heartbeat_stop = Arc::new(AtomicBool::new(false));
            let heartbeat_handle = {
                let stop = Arc::clone(&heartbeat_stop);
                std::thread::spawn(move || {
                    const TICK: std::time::Duration = std::time::Duration::from_millis(50);
                    const BEAT_EVERY: std::time::Duration = std::time::Duration::from_secs(2);
                    let mut since_beat = std::time::Duration::ZERO;
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(TICK);
                        since_beat += TICK;
                        if since_beat >= BEAT_EVERY {
                            since_beat = std::time::Duration::ZERO;
                            if stop.load(Ordering::Relaxed) {
                                break;
                            }
                            event::global_broadcaster().transcribing_started();
                        }
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
        )
        .map(|r| apply_mic_conflict_preemption(r, mic_conflict_preempted));
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

    Ok(apply_mic_conflict_preemption(
        RecordResult::Speech {
            text: raw,
            energy: speech_energy,
            transcription: result,
            timing: UtteranceTiming {
                speech_end_at,
                stt_done_at,
                speculation_used,
            },
        },
        mic_conflict_preempted,
    ))
}

/// Turn a finalized utterance into [`RecordResult::PreemptedByMicConflict`]
/// when another app took the microphone mid-capture. The transcript is
/// carried through so the words are not lost, but the variant tells the
/// caller not to paste: the app that took the mic is about to paste its
/// own transcript of the very same speech.
///
/// Non-speech outcomes (silence, noise, a speaker-gate drop) pass
/// through unchanged — there is nothing to keep and nothing to suppress.
#[cfg(feature = "macos")]
fn apply_mic_conflict_preemption(result: RecordResult, preempted: bool) -> RecordResult {
    if !preempted {
        return result;
    }
    match result {
        RecordResult::Speech { text, .. } => RecordResult::PreemptedByMicConflict { text },
        other => other,
    }
}

/// Non-macOS stub.
#[cfg(not(feature = "macos"))]
fn record_with_local_vad(
    _cfg: &AlwaysConfig,
    _log: &mut Logger,
    _transcriber: &Arc<dyn Transcriber>,
    _rt: &tokio::runtime::Handle,
) -> Result<RecordResult> {
    Err(anyhow::anyhow!(
        "Audio capture not supported on this platform"
    ))
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
    // Drop the hesitation filler the user trails off with while thinking.
    // It has already done its job by extending the silence window (see
    // TRAILING_CONNECTORS); it should not reach the clipboard. Applied at
    // finalization only -- previews keep it, so the overlay still reflects
    // what was actually said while speaking.
    let full_text = strip_trailing_filler(&full_text);
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

/// `looks_mid_sentence` for a transcript whose punctuation carries no
/// information — i.e. the LIVE streaming path.
///
/// Identical to [`looks_mid_sentence`] except it drops the final
/// "no sentence terminator ⇒ unfinished" clause. That clause is right for the
/// cloud/Whisper speculation path (those engines punctuate reliably) and
/// catastrophic here: Nemotron does not punctuate dictation, so EVERY live
/// transcript ended without a terminator, `looks_mid_sentence` was true on
/// every utterance, and it short-circuited the `else if` that owns early
/// finalization. Measured consequence: `early_finalize_decision` fired 0
/// times ever, while every ordinary sentence silently took the EXTENDED
/// window (2 × 0.9 s = 1.8 s) instead of the configured 0.9 s.
///
/// What is left is the part that actually signals "still going" and does not
/// depend on the decoder punctuating: an explicitly unfinished trailing mark
/// (`,` `;` `:` `—` `-`) or a dangling connector / hesitation filler as the
/// last word. Both keep the long window, so a user who trails off with "and"
/// or thinks out loud with "uh" is still never cut off.
fn looks_mid_sentence_live(loc: &crate::always::localization::Localization, text: &str) -> bool {
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
    let _ = loc;
    TRAILING_CONNECTORS.contains(&last_word.as_str())
}

/// Does the live transcript read as a FINISHED thought — i.e. is it safe to
/// end the utterance early instead of waiting out the full silence window?
///
/// The inverse of [`looks_mid_sentence`] plus a length floor. `!mid_sentence`
/// already means "ends on a sentence terminator and the last word is not a
/// connector or a hesitation filler"; the extra word-count guard keeps the
/// aggressive cut away from one-word fragments, where the decoder's habitual
/// period carries no information about whether the user is done.
///
/// False negatives are benign — the window stays at its configured length,
/// exactly as before this existed.
fn looks_complete_utterance(loc: &crate::always::localization::Localization, text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Deliberately NOT `!looks_mid_sentence`. That helper treats a missing
    // sentence terminator as "unfinished", which is right for deciding whether
    // to EXTEND a window but fatal as a precondition for finalizing EARLY:
    // Nemotron does not reliably punctuate dictation, so real utterances read
    //   "How do you think it working"
    //   "That still took like five to six seconds"
    // and the fast path was unreachable -- measured `early_finalize_decision`
    // = 0 across a live session, i.e. the optimisation never once fired.
    //
    // What actually signals "still going" is the LAST WORD: a dangling
    // connector ("and", "to", "because") or a hesitation filler ("uh", "um").
    // Those keep the long window. Anything else, with enough words to be a
    // real thought, is treated as finishable. Punctuation, when present,
    // still counts -- it just is not required.
    let last_word: String = trimmed
        .trim_end_matches(|c: char| !c.is_alphanumeric())
        .rsplit(char::is_whitespace)
        .next()
        .unwrap_or("")
        .to_lowercase();
    if TRAILING_CONNECTORS.contains(&last_word.as_str()) {
        return false;
    }
    if let Some(last_char) = trimmed.chars().last()
        && matches!(last_char, ',' | ';' | ':' | '—' | '-')
    {
        return false;
    }
    let _ = loc;
    trimmed.split_whitespace().count() >= COMPLETE_UTTERANCE_MIN_WORDS
}

/// Shortened final-silence window used once `looks_complete_utterance` fires.
///
/// Clamped to the configured window at the top so this can only ever make the
/// wait SHORTER, never longer — a user who deliberately sets a very small
/// `stt_silence` keeps it.
fn complete_utterance_silence_frames(final_silence_frames: usize) -> usize {
    let floor = ((COMPLETE_UTTERANCE_SILENCE_MS as f64) / FRAME_MS as f64).ceil() as usize;
    floor.max(1).min(final_silence_frames)
}

#[cfg(test)]
mod tests {
    use super::{
        chunk_target_secs, complete_utterance_silence_frames, early_voice_frame_ok,
        effective_chunk_hard_max_secs, effective_chunk_target_secs, extended_silence_frames,
        fast_energy_check, fast_normalized_energy, live_carrying_state, live_session_carrying,
        looks_complete_utterance, looks_mid_sentence, looks_mid_sentence_live,
        normal_silence_frames, normalized_energy, speaker_gate_allows_score,
        speaker_gate_allows_stt, speaker_gate_allows_transcription,
        speaker_gate_dependencies_ready, speaker_gate_should_reject_unavailable,
        verdict_tick_allowed, voice_activity_energy_threshold,
    };
    use crate::always::AlwaysConfig;

    static CHUNK_TARGET_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    fn extended_silence_doubles_but_caps() {
        // 0.9s default → 30 frames → doubled to 60 (1.8s), under the
        // +1.5s (50 frame) cap... 30+50=80 > 60, so factor wins.
        assert_eq!(extended_silence_frames(30), 60);
        // Large user window (3s → 100 frames): cap wins, 100+50=150 < 200.
        assert_eq!(extended_silence_frames(100), 150);
    }

    /// The early window is a FLOOR clamped to the configured window: it may
    /// only ever shorten the wait, never lengthen it.
    #[test]
    fn complete_utterance_window_shortens_but_never_lengthens() {
        // User's 0.6s window → 20 frames → early cut at 300 ms = 10 frames.
        assert_eq!(complete_utterance_silence_frames(20), 10);
        // 0.9s default → 30 frames → still 10.
        assert_eq!(complete_utterance_silence_frames(30), 10);
        // A user who configured something SHORTER than the early window keeps
        // their own setting — the clamp must not stretch it.
        assert_eq!(complete_utterance_silence_frames(7), 7);
        assert_eq!(complete_utterance_silence_frames(1), 1);
        for base in 1..=120usize {
            assert!(
                complete_utterance_silence_frames(base) <= base,
                "early window must never exceed the configured window (base {base})"
            );
            assert!(complete_utterance_silence_frames(base) >= 1);
        }
    }

    /// The early cut fires only on text that reads as a finished thought.
    /// Every case here is the difference between pasting now and waiting out
    /// the rest of the configured silence, so the truth table is explicit.
    #[test]
    fn looks_complete_utterance_truth_table() {
        let loc = &crate::always::localization::Localization::ENGLISH;

        // Finished sentences: terminator, no trailing connector, 3+ words.
        assert!(looks_complete_utterance(loc, "Send the email now."));
        assert!(looks_complete_utterance(loc, "Is that correct?"));
        assert!(looks_complete_utterance(loc, "Stop the build now!"));
        assert!(looks_complete_utterance(
            loc,
            "  I went to the store yesterday.  "
        ));

        // Unpunctuated but finished. Nemotron does not reliably punctuate
        // dictation, so requiring a terminator made this path unreachable:
        // measured `early_finalize_decision` = 0 across a live session while
        // the user's real transcripts read "How do you think it working" and
        // "That still took like five to six seconds".
        assert!(looks_complete_utterance(loc, "I went to the store"));
        assert!(looks_complete_utterance(loc, "How do you think it working"));
        assert!(looks_complete_utterance(
            loc,
            "That still took like five to six seconds"
        ));
        // Unfinished: explicit continuation punctuation.
        assert!(!looks_complete_utterance(loc, "first item,"));
        assert!(!looks_complete_utterance(loc, "the following items:"));
        // Terminator present but the last word is a connector — the decoder's
        // habitual period, not the user's full stop.
        assert!(!looks_complete_utterance(
            loc,
            "I want to change the file and."
        ));
        // Hesitation fillers must NOT finalize early: they are the user's own
        // signal that they are still thinking.
        for filler in ["uh", "um", "hmm", "er", "like"] {
            assert!(
                !looks_complete_utterance(loc, &format!("so the thing is {filler}.")),
                "trailing filler {filler:?} must not trigger an early cut"
            );
        }

        // Too short to trust: a lone "Okay." is what a mid-thought pause looks
        // like to the decoder. Short utterances have their own fast path.
        assert!(!looks_complete_utterance(loc, "Okay."));
        assert!(!looks_complete_utterance(loc, "Got it."));
        // Three words is the threshold, not four.
        assert!(looks_complete_utterance(loc, "Yes it is."));

        // Degenerate input.
        assert!(!looks_complete_utterance(loc, ""));
        assert!(!looks_complete_utterance(loc, "   "));
    }

    /// The two verdicts must never both fire on the same text: one lengthens
    /// the window, the other shortens it.
    #[test]
    fn complete_never_fires_while_the_user_is_still_going() {
        // These are NOT inverses any more. `looks_mid_sentence` treats a
        // missing terminator as unfinished (right for EXTENDING a window);
        // `looks_complete_utterance` does not (required for finalizing EARLY,
        // since dictation is largely unpunctuated -- measured
        // `early_finalize_decision` = 0 while the terminator was required).
        // The invariant that still holds is one-directional.
        let loc = &crate::always::localization::Localization::ENGLISH;
        for text in [
            "so the thing is uh",
            "I want to change the file and",
            "first item,",
            "the following items:",
            "",
            "   ",
        ] {
            assert!(
                !looks_complete_utterance(loc, text),
                "must not finalize early on {text:?}"
            );
        }
        // And unpunctuated real dictation MUST now be finalizable.
        for text in [
            "I went to the store",
            "How do you think it working",
            "That still took like five to six seconds",
        ] {
            assert!(
                looks_complete_utterance(loc, text),
                "unpunctuated dictation must finalize early: {text:?}"
            );
        }
    }

    /// The live path's extension rule must not depend on punctuation.
    ///
    /// This is the regression that made `early_finalize_decision` = 0. The
    /// verdict ladder checks the mid-sentence branch FIRST, so as long as
    /// `looks_mid_sentence` was used there, any unpunctuated transcript
    /// latched `midsentence_extended` and the early branch was dead code —
    /// and the window silently DOUBLED (2 x 0.9 s) instead of shortening.
    #[test]
    fn live_mid_sentence_ignores_missing_punctuation() {
        let loc = &crate::always::localization::Localization::ENGLISH;
        for text in [
            "I went to the store",
            "How do you think it working",
            "That still took like five to six seconds",
        ] {
            // The old rule: unfinished purely because Nemotron did not
            // punctuate. This is the blocker, asserted so it cannot come back.
            assert!(
                looks_mid_sentence(loc, text),
                "precondition of the bug: {text:?}"
            );
            assert!(
                !looks_mid_sentence_live(loc, text),
                "live path must not call unpunctuated dictation mid-sentence: {text:?}"
            );
        }
    }

    /// Everything that genuinely signals "still talking" must keep extending.
    /// Cutting the user off mid-thought is far worse than waiting, so this is
    /// the half of the rule that must never be relaxed.
    #[test]
    fn live_mid_sentence_still_extends_on_real_signals() {
        let loc = &crate::always::localization::Localization::ENGLISH;
        for text in [
            "first item,",
            "the following items:",
            "one thing;",
            "I want to change the file and",
            "I want to change the file and.",
            "we need to",
            "the reason is because",
        ] {
            assert!(
                looks_mid_sentence_live(loc, text),
                "must keep waiting on {text:?}"
            );
        }
        // Hesitation fillers: the user says "uh" WHILE THINKING. Punctuated or
        // not, they buy time and never finalize.
        for filler in ["uh", "um", "uhh", "umm", "er", "erm", "hmm", "mmm", "like"] {
            assert!(
                looks_mid_sentence_live(loc, &format!("so the thing is {filler}")),
                "bare filler {filler:?} must extend the window"
            );
            assert!(
                looks_mid_sentence_live(loc, &format!("so the thing is {filler}.")),
                "punctuated filler {filler:?} must extend the window"
            );
        }
        // Empty transcript is not a verdict either way.
        assert!(!looks_mid_sentence_live(loc, ""));
        assert!(!looks_mid_sentence_live(loc, "   "));
    }

    /// The live ladder is `extend` -> else `finalize early` -> else wait.
    /// Both branches firing on one text would be a contradiction; neither
    /// firing is the safe default (configured window, unchanged).
    #[test]
    fn live_verdicts_are_mutually_exclusive() {
        let loc = &crate::always::localization::Localization::ENGLISH;
        for text in [
            "I went to the store",
            "How do you think it working",
            "so the thing is uh",
            "I want to change the file and",
            "first item,",
            "Okay.",
            "Yes it is.",
            "",
        ] {
            assert!(
                !(looks_mid_sentence_live(loc, text) && looks_complete_utterance(loc, text)),
                "contradictory verdict on {text:?}"
            );
        }
        // And on ordinary dictation the ladder must actually REACH the early
        // branch — the whole point of the fix.
        assert!(!looks_mid_sentence_live(loc, "How do you think it working"));
        assert!(looks_complete_utterance(loc, "How do you think it working"));
    }

    fn tick(silence: usize) -> super::VerdictTick {
        super::VerdictTick {
            live_invalidated: false,
            voice_logged: true,
            speculation_speaker_ok: true,
            consecutive_silence: silence,
            tentative_frames: 8,
            base_frames: 30,
            midsentence_decided: false,
        }
    }

    /// The gate's cadence: nothing before the tentative mark, then every
    /// second frame, plus one guaranteed last look before the base cut.
    #[test]
    fn verdict_tick_cadence() {
        for f in 0..8 {
            assert!(!verdict_tick_allowed(tick(f)), "too early at {f}");
        }
        assert!(verdict_tick_allowed(tick(8)));
        assert!(!verdict_tick_allowed(tick(9)));
        assert!(verdict_tick_allowed(tick(10)));
        // Last look: odd frame immediately before the base cut, only while no
        // verdict has been reached yet.
        assert!(verdict_tick_allowed(tick(29)));
        let mut decided = tick(29);
        decided.midsentence_decided = true;
        assert!(!verdict_tick_allowed(decided));
    }

    /// Each hard precondition, one at a time.
    #[test]
    fn verdict_tick_hard_preconditions() {
        let mut t = tick(10);
        t.live_invalidated = true;
        assert!(!verdict_tick_allowed(t));

        let mut t = tick(10);
        t.voice_logged = false;
        assert!(!verdict_tick_allowed(t));

        let mut t = tick(10);
        t.speculation_speaker_ok = false;
        assert!(!verdict_tick_allowed(t));
    }

    /// REGRESSION: the gate must not depend on "has speech landed since the
    /// last chunk flush".
    ///
    /// `voiced_since_flush` is cleared by every chunk flush and set back to
    /// true only by a VOICED frame. When the flushing pause is also the end of
    /// the utterance — the ordinary case, since the flush happens at a
    /// tentative pause — nothing can ever set it again, so the verdict was
    /// unreachable for the whole remaining silence run and neither
    /// `early_finalize_decision` nor its skip reason could be logged. The gate
    /// therefore takes no such input at all: `VerdictTick` has no field for it,
    /// which this test pins by construction.
    #[test]
    fn verdict_tick_survives_a_chunk_flush() {
        // Post-flush state differs from pre-flush state ONLY in fields the
        // gate does not read, so the answer must be identical.
        assert!(verdict_tick_allowed(tick(10)));
        assert!(verdict_tick_allowed(tick(12)));
    }

    #[test]
    fn live_carrying_reason_names_the_failure() {
        assert_eq!(super::live_carrying_reason(None, false), "no_session");
        assert_eq!(super::live_carrying_reason(None, true), "no_session");
    }

    #[test]
    fn chunk_target_defaults_to_liveish_chunks() {
        let _guard = CHUNK_TARGET_ENV_LOCK
            .lock()
            .expect("CHUNK_TARGET_ENV_LOCK poisoned");
        unsafe { std::env::remove_var("ALWAYS_CHUNK_TARGET_SECS") };

        assert_eq!(chunk_target_secs(), 6);
    }

    /// A live session must suppress rolling chunking: every flush resets the
    /// session, so chunking a streaming utterance converts a flat ~130 ms
    /// finalization into one from-scratch decode per 6 s of speech.
    #[test]
    fn live_session_suppresses_rolling_chunking() {
        let _guard = CHUNK_TARGET_ENV_LOCK
            .lock()
            .expect("CHUNK_TARGET_ENV_LOCK poisoned");
        unsafe { std::env::remove_var("ALWAYS_CHUNK_TARGET_SECS") };

        // No live session (cloud engine, or a non-streaming local one):
        // unchanged — 6 s target, 15 s mid-speech ceiling.
        assert_eq!(effective_chunk_target_secs(false), 6);
        assert_eq!(effective_chunk_hard_max_secs(false), 15);

        // Live session carrying the utterance: effectively no chunking until
        // the 120 s safety valve.
        assert_eq!(effective_chunk_target_secs(true), 120);
        assert_eq!(effective_chunk_hard_max_secs(true), 120);

        // The ceiling can never sit below the target, or every chunk would
        // flush mid-speech instead of at a pause.
        for streaming in [false, true] {
            assert!(
                effective_chunk_hard_max_secs(streaming) >= effective_chunk_target_secs(streaming)
            );
        }
    }

    /// The test override has to keep working even on a streaming engine, or
    /// the end-to-end chunk test can never flush.
    #[test]
    fn chunk_env_override_still_wins_while_streaming() {
        let _guard = CHUNK_TARGET_ENV_LOCK
            .lock()
            .expect("CHUNK_TARGET_ENV_LOCK poisoned");
        unsafe { std::env::set_var("ALWAYS_CHUNK_TARGET_SECS", "4") };

        assert_eq!(effective_chunk_target_secs(true), 4);
        assert_eq!(effective_chunk_target_secs(false), 4);
        assert_eq!(effective_chunk_hard_max_secs(true), 15);

        unsafe { std::env::remove_var("ALWAYS_CHUNK_TARGET_SECS") };
    }

    /// Both live-session failure modes must hand the utterance back to the
    /// chunker: it is the only path that still produces text.
    #[test]
    fn broken_live_session_restores_chunking() {
        // Healthy session, buffer intact: the only case that suppresses
        // chunking.
        assert!(live_carrying_state(Some(false), false));
        // Worker dead or hopelessly behind — finalization goes one-shot, so
        // chunks are worth their cost again.
        assert!(!live_carrying_state(Some(true), false));
        // Audio truncated out from under the session (speaker-gate cut): its
        // decoded state no longer describes the buffer.
        assert!(!live_carrying_state(Some(false), true));
        assert!(!live_carrying_state(Some(true), true));
        // No session at all — every cloud engine, and every local engine
        // except Nemotron.
        assert!(!live_carrying_state(None, false));
        assert!(!live_carrying_state(None, true));

        // The Option-taking wrapper agrees on the no-session cases.
        assert!(!live_session_carrying(None, false));
        assert!(!live_session_carrying(None, true));
    }

    #[test]
    fn chunk_target_env_override_is_floored() {
        let _guard = CHUNK_TARGET_ENV_LOCK
            .lock()
            .expect("CHUNK_TARGET_ENV_LOCK poisoned");
        unsafe { std::env::set_var("ALWAYS_CHUNK_TARGET_SECS", "1") };

        assert_eq!(chunk_target_secs(), 3);

        unsafe { std::env::remove_var("ALWAYS_CHUNK_TARGET_SECS") };
    }

    /// "uh" is the user's own tell that they are still thinking. Cutting on
    /// it truncates the thought; it must extend the window like any other
    /// connector -- and must never survive into the pasted text.
    #[test]
    fn hesitation_fillers_extend_the_window_but_are_stripped() {
        let loc = &crate::always::localization::Localization::ENGLISH;
        for f in ["uh", "um", "hmm", "erm"] {
            assert!(
                looks_mid_sentence(loc, &format!("so the thing is {f}")),
                "{f} must extend the silence window"
            );
        }
        assert_eq!(
            super::strip_trailing_filler("so the thing is uh"),
            "so the thing is"
        );
        assert_eq!(
            super::strip_trailing_filler("send it now, um."),
            "send it now"
        );
        assert_eq!(super::strip_trailing_filler("wait uh um"), "wait");
        // A real trailing word the user meant must survive untouched.
        assert_eq!(
            super::strip_trailing_filler("meet me at the"),
            "meet me at the"
        );
        assert_eq!(super::strip_trailing_filler("done."), "done.");
        // Never strip a filler that is the ENTIRE utterance into nothing
        // unexpected -- callers already reject empty transcripts.
        assert_eq!(super::strip_trailing_filler("uh"), "");
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

    #[cfg(feature = "macos")]
    #[test]
    fn scoring_uses_the_closest_enrolled_style_not_just_the_blend() {
        use super::best_voiceprint_match;
        use crate::always::speaker_embed::combine_embeddings;
        use crate::always::voiceprint::VoiceProfile;
        use std::collections::BTreeMap;

        // Two deliberately different styles, as enrollment produces.
        // Already unit-length, so they stand in for real embeddings.
        let normal = vec![1.0f32, 0.0, 0.0];
        let louder = vec![0.0f32, 1.0, 0.0];
        let combined = combine_embeddings(&[normal.clone(), louder.clone()]).unwrap();

        let mut steps = BTreeMap::new();
        steps.insert("normal".to_string(), normal.clone());
        steps.insert("louder".to_string(), louder.clone());
        let profile = VoiceProfile {
            version: 1,
            model: "test".to_string(),
            steps,
            voiceprint: combined.clone(),
            created_at: String::new(),
            updated_at: String::new(),
        };

        // Speaking in exactly one enrolled style: against the blend this
        // caps at ~0.707, which is the delay-causing ceiling. Against the
        // matching style it is a perfect 1.0.
        let blend_only = crate::always::speaker_embed::cosine(&normal, &combined);
        assert!(
            blend_only < 0.75,
            "blend should visibly penalise a single style, got {blend_only}"
        );
        let (best, matched) = best_voiceprint_match(&normal, &profile);
        assert!(best > 0.99, "matching style should score ~1.0, got {best}");
        assert_eq!(matched, "normal");
        assert!(best > blend_only);

        // Never worse than the old centroid-only behaviour.
        let midway = vec![
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
        ];
        let (best_mid, _) = best_voiceprint_match(&midway, &profile);
        assert!(best_mid >= crate::always::speaker_embed::cosine(&midway, &combined) - 1e-6);

        // An unrelated voice stays far away — the max does not smuggle
        // impostors past the threshold.
        let impostor = vec![0.0f32, 0.0, 1.0];
        let (best_impostor, _) = best_voiceprint_match(&impostor, &profile);
        assert!(
            best_impostor < 0.1,
            "impostor must stay low, got {best_impostor}"
        );
    }

    #[cfg(feature = "macos")]
    #[test]
    fn mic_conflict_keeps_the_words_but_suppresses_the_paste() {
        use super::{RecordResult, UtteranceTiming, apply_mic_conflict_preemption};

        let speech = || RecordResult::Speech {
            text: "half a sentence".to_string(),
            energy: 0.05,
            transcription: crate::stt::TranscriptionResult::default(),
            timing: UtteranceTiming {
                speech_end_at: std::time::Instant::now(),
                stt_done_at: std::time::Instant::now(),
                speculation_used: false,
            },
        };

        // No conflict: the utterance stays on the paste path untouched.
        assert!(matches!(
            apply_mic_conflict_preemption(speech(), false),
            RecordResult::Speech { .. }
        ));

        // Conflict: same words, but off the paste path.
        match apply_mic_conflict_preemption(speech(), true) {
            RecordResult::PreemptedByMicConflict { text } => {
                assert_eq!(text, "half a sentence", "the words must survive the cut");
            }
            _ => panic!("expected PreemptedByMicConflict"),
        }

        // Nothing was said before the cut — nothing to keep, nothing to
        // suppress; a drop stays a drop.
        assert!(matches!(
            apply_mic_conflict_preemption(RecordResult::Silence, true),
            RecordResult::Silence
        ));
        assert!(matches!(
            apply_mic_conflict_preemption(RecordResult::DroppedSpeaker { score: 0.1 }, true),
            RecordResult::DroppedSpeaker { .. }
        ));
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

    #[test]
    fn speaker_gate_media_bypass_requires_every_dependency() {
        assert!(speaker_gate_dependencies_ready(true, true, true));
        assert!(!speaker_gate_dependencies_ready(false, true, true));
        assert!(!speaker_gate_dependencies_ready(true, false, true));
        assert!(!speaker_gate_dependencies_ready(true, true, false));
    }

    #[test]
    fn speaker_gate_scoring_error_rejects_utterance() {
        assert!(speaker_gate_allows_score(Some(0.75), 0.50));
        assert!(!speaker_gate_allows_score(Some(0.25), 0.50));
        assert!(!speaker_gate_allows_score(None, 0.50));
    }

    #[test]
    fn requested_speaker_gate_without_embedder_rejects_speech() {
        assert!(!speaker_gate_allows_transcription(true, None, 0.50));
        assert!(speaker_gate_allows_transcription(false, None, 0.50));
    }

    #[test]
    fn speaker_gate_enforces_only_me() {
        // Enforcement ON: unverified speech is blocked from STT so a foreign
        // voice (media / other person) never transcribes or pastes.
        assert!(speaker_gate_allows_stt(false, false));
        assert!(speaker_gate_allows_stt(true, true));
        assert!(!speaker_gate_allows_stt(true, false));

        let threshold = super::SPEAKER_GATE_EARLY_SAMPLES;
        assert!(!speaker_gate_should_reject_unavailable(
            true,
            false,
            threshold - 1
        ));
        assert!(speaker_gate_should_reject_unavailable(
            true, false, threshold
        ));
        assert!(!speaker_gate_should_reject_unavailable(
            false, false, threshold
        ));
        assert!(!speaker_gate_should_reject_unavailable(
            true, true, threshold
        ));
    }

    #[test]
    fn streaming_preview_is_cumulative_and_preserves_chunk_text() {
        let mut accumulated = String::new();
        assert!(!super::append_streaming_preview(&mut accumulated, "  "));
        assert!(super::append_streaming_preview(
            &mut accumulated,
            "Bonjour, ça va ?"
        ));
        assert!(super::append_streaming_preview(
            &mut accumulated,
            "Très bien."
        ));
        assert_eq!(accumulated, "Bonjour, ça va ? Très bien.");
        assert!(!accumulated.contains(", B, o"));
    }

    #[test]
    fn preview_cadence_fast_path_for_consume_mode_and_streaming_engines() {
        // Consume mode keeps the original fast cadence regardless of
        // engine or pref — external consumers (Iris) depend on it.
        for (streaming, pref) in [(false, false), (false, true), (true, false), (true, true)] {
            let cadence = super::preview_cadence(true, streaming, false, false, pref)
                .expect("consume mode always arms the preview");
            assert_eq!(cadence.interval_ms, super::CONSUME_STREAM_INTERVAL_MS);
            assert_eq!(cadence.min_new_samples, 0);
            assert!(!cadence.require_speculation_idle);
            assert!(cadence.flip_overlay);
            assert!(!cadence.prefix_settled_chunks);
        }
        // A REMOTE streaming engine keeps the fast path: its own round-trip
        // latency is what limits the rate. (This used to read "streaming
        // previews are local + free" — that assumption is what let a local
        // streaming engine run previews back-to-back and peg every core.)
        let cadence = super::preview_cadence(false, true, false, false, false)
            .expect("streaming engine always arms the preview");
        assert_eq!(cadence.interval_ms, super::CONSUME_STREAM_INTERVAL_MS);
    }

    #[test]
    fn preview_cadence_slow_cloud_path_gated_on_pref() {
        // Groq (non-streaming) + pref ON → slow, starvation-safe cadence.
        let cadence = super::preview_cadence(false, false, false, false, true)
            .expect("live-preview pref arms the slow cloud cadence");
        assert_eq!(cadence.interval_ms, super::LIVE_PREVIEW_INTERVAL_MS);
        assert_eq!(cadence.min_new_samples, super::LIVE_PREVIEW_MIN_NEW_SAMPLES);
        assert!(cadence.require_speculation_idle);
        assert!(!cadence.flip_overlay);
        assert!(cadence.prefix_settled_chunks);
        // The slow cadence must actually be slow: a full order of
        // magnitude above the local/consume interval, and it must demand
        // real new audio before burning another round trip.
        assert!(cadence.interval_ms >= 1_000);
        assert!(cadence.min_new_samples >= 8_000);

        // Pref OFF (and no consume/streaming) → no live preview at all.
        assert_eq!(super::preview_cadence(false, false, false, false, false), None);
    }

    /// A LOCAL streaming engine (Nemotron) must never get the 200ms cloud
    /// cadence. That cadence explicitly relies on network round-trip latency
    /// to self-limit; a local engine has none, so previews ran back-to-back,
    /// each re-decoding up to 10s of audio on this machine's own cores.
    /// Observed: load average >90 and `rec_coreaudio_overrun` as the
    /// recorder was starved.
    /// A voice coming out of the speakers must not clear the gate just
    /// because the threshold was lowered to help the user's own quieter
    /// speech. Observed leak: a YouTube narrator scored 0.404 against a
    /// 0.35 threshold and was pasted as if the user had spoken it.
    #[test]
    fn audio_playing_raises_the_speaker_gate() {
        let base = 0.35f32;
        let bumped = super::speaker_gate_window_threshold(base, true);
        // The observed leak must not clear the raised bar.
        assert!(0.404 < bumped, "the exact score that leaked must now fail");
        // The user's own typical score must still clear it, so dictating
        // over music keeps working.
        assert!(0.55 > bumped, "user's own voice must still pass");
        // And the bump must exceed the margin by which the leak passed.
        assert!(super::AUDIO_PLAYING_GATE_BUMP > 0.404 - base);
    }

    /// Single-window scores measured off the incident log
    /// (~/Library/Logs/Always/always.2026-08-31, 18:40:30-18:43:30 UTC,
    /// Hindi YouTube playing, `speaker_gate_threshold` = 0.35). Each of
    /// these leaked an entire utterance to the paste path.
    const MEDIA_LEAKING_WINDOW_SCORES: [f32; 3] = [0.3622653, 0.3654983, 0.3751205];
    /// Highest WHOLE-UTTERANCE score media reached over the same window
    /// (n = 42, from the `speaker_gate_early_reject` /
    /// `speaker_gate_tentative_reject` lines, which score the full
    /// buffer). Every one of the 42 was below the 0.35 threshold.
    const MEDIA_MAX_WHOLE_UTTERANCE_SCORE: f32 = 0.3407;
    /// Lowest score the USER's own speech reached while verifying on the
    /// same day (`speaker_gate_verified`, 14:46-14:57 UTC), and their
    /// typical range dictating over media.
    const USER_MIN_OBSERVED_SCORE: f32 = 0.3552;

    /// The bump must land on the single-window bar ONLY.
    ///
    /// Raising the whole-utterance bar to 0.50 would reject the user's
    /// own Nepali (0.45-0.52 against their 0.35 pref) — that is the
    /// "dictate while music plays" promise, and it must survive.
    #[test]
    fn audio_playing_bump_never_raises_the_whole_utterance_bar() {
        let base = 0.35f32;
        let window = super::speaker_gate_window_threshold(base, true);
        assert!(window > base, "window bar rises while media plays");
        for user_nepali in [0.45f32, 0.47, 0.52] {
            assert_eq!(
                super::speaker_confirmation(true, Some(user_nepali), base),
                super::SpeakerConfirmation::Confirmed(user_nepali),
                "dictating over music must still be confirmed at {user_nepali}"
            );
        }
        // Sanity: had the bump reached the whole-utterance bar, it would
        // have rejected exactly that speech. This is the regression.
        assert!(
            0.45 < window,
            "0.45 vs the bumped bar is why the bump must not apply here"
        );
    }

    /// With no media playing, nothing changes for the user.
    #[test]
    fn window_bar_is_permissive_when_nothing_else_is_playing() {
        // The per-window bar decides only WHEN to start transcribing; the
        // whole-utterance confirmation decides whether the text is kept.
        // Keeping them equal cost the user 12.6s of discarded speech while
        // their own windows scored 0.024-0.318 before one hit 0.617.
        for base in [0.30f32, 0.35, 0.45, 0.50] {
            let w = super::speaker_gate_window_threshold(base, false);
            assert!(w < base, "window bar must be permissive vs the real bar");
            assert!(
                w <= super::WINDOW_THRESHOLD_CEILING,
                "raising the main threshold must not restore the stall"
            );
            // Must still sit above the noise floor: ambient rejects measured
            // -0.10..0.10, so a bar at/below 0 would admit silence.
            assert!(w > 0.0, "window bar must stay above the noise floor");
        }
        // The user's own worst observed windows must now be admitted.
        let w = super::speaker_gate_window_threshold(0.35, false);
        for observed in [0.318f32, 0.617, 0.532, 0.502] {
            assert!(observed > w, "{observed} was the user and must pass");
        }
    }

    /// Whole-buffer scores logged by `speaker_gate_early_reject` while the
    /// USER was demonstrably mid-sentence, 2026-09-01 11:37:17-11:37:22 UTC.
    /// The utterance either side of them was `speaker_gate_verified` at
    /// 0.5764 and pasted, so these are the same voice, ~4s apart. Each one
    /// destroyed the in-flight buffer; together they cost 5.7s of a 12.5s
    /// sentence, and what reached the user was "end to achieve this vision."
    const USER_PREFIX_SCORES_THAT_WERE_DESTROYED: [f32; 2] = [0.1448, 0.1931];

    /// THE TRUNCATION. The ~2s early abort is the only speaker check that
    /// throws away an in-flight recording, and it was judging a 2s prefix
    /// against the WHOLE-UTTERANCE bar. Measured over 11,541 firings on
    /// 2026-08-31/09-01, the whole-buffer score reached 0.35 five times
    /// (0 times on 09-01, max 0.3330) — it had become an unconditional
    /// kill, and the words it killed were the user's.
    #[test]
    fn early_abort_does_not_destroy_the_user_mid_sentence() {
        let base = 0.35f32;
        let abort = super::speaker_gate_window_threshold(base, false);

        // The regression, stated directly: at the whole-utterance bar every
        // one of these prefixes was destroyed.
        for score in USER_PREFIX_SCORES_THAT_WERE_DESTROYED {
            assert!(
                !super::speaker_gate_allows_score(Some(score), base),
                "{score} is why the whole-utterance bar cannot govern the abort"
            );
            assert!(
                super::speaker_gate_allows_score(Some(score), abort),
                "the user mid-sentence at {score} must survive to finalization"
            );
        }

        // And the abort must still do its job: it exists so background
        // dialogue cannot hold the recorder hostage for a whole scene.
        // Media measured p50 0.0116 / p90 0.1240 over 6,951 aborts, so the
        // bulk of them still fire on exactly the same audio.
        for media in [-1.0f32, 0.0, 0.0116, 0.05, 0.10] {
            assert!(
                !super::speaker_gate_allows_score(Some(media), abort),
                "media at {media} must still abort the recording"
            );
        }
        // An embedder error is still no opinion, and must still fail closed
        // here — this branch cannot verify anyone.
        assert!(!super::speaker_gate_allows_score(None, abort));
    }

    /// The abort is destructive, so the media bump must not reach it: it
    /// would delete the buffer of anyone dictating over music, which is the
    /// exact promise `audio_playing_bump_never_raises_the_whole_utterance_bar`
    /// protects on the other bar.
    #[test]
    fn early_abort_bar_is_never_raised_by_system_audio() {
        let base = 0.35f32;
        let abort = super::speaker_gate_window_threshold(base, false);
        let bumped = super::speaker_gate_window_threshold(base, true);
        assert!(abort < bumped, "the abort bar must be the unbumped one");
        for score in USER_PREFIX_SCORES_THAT_WERE_DESTROYED {
            assert!(
                !super::speaker_gate_allows_score(Some(score), bumped),
                "{score} shows what bumping the abort bar would cost"
            );
        }
    }

    /// Relaxing the abort must not paste anything new. Anything that now
    /// survives it still meets the unchanged whole-utterance confirmation,
    /// whose measured media ceiling is `MEDIA_MAX_WHOLE_UTTERANCE_SCORE`.
    #[test]
    fn early_abort_relaxation_cannot_leak_media_to_the_paste() {
        let base = 0.35f32;
        let abort = super::speaker_gate_window_threshold(base, false);
        // The loudest media score ever observed clears the relaxed abort...
        assert!(super::speaker_gate_allows_score(
            Some(MEDIA_MAX_WHOLE_UTTERANCE_SCORE),
            abort
        ));
        // ...and is still refuted where it counts.
        assert_eq!(
            super::speaker_confirmation(true, Some(MEDIA_MAX_WHOLE_UTTERANCE_SCORE), base),
            super::SpeakerConfirmation::Refuted(MEDIA_MAX_WHOLE_UTTERANCE_SCORE),
            "the authoritative gate is unchanged and still blocks the leak"
        );
        // The user's own whole utterance is unaffected in both directions.
        assert_eq!(
            super::speaker_confirmation(true, Some(USER_MIN_OBSERVED_SCORE), base),
            super::SpeakerConfirmation::Confirmed(USER_MIN_OBSERVED_SCORE)
        );
    }

    /// The self-contradicting utterance, 2026-09-01 11:37:22-11:37:30Z.
    /// `speaker_gate_verified` put the whole thing at 0.5764 — the user,
    /// unambiguously — and eight seconds later `speaker_gate_tail_cut`
    /// deleted its last 1.02s on a window scoring 0.1447.
    const USER_TAIL_SCORE_THAT_WAS_CUT: f32 = 0.1447;
    const SAME_UTTERANCE_WHOLE_SCORE: f32 = 0.5764;

    /// THE MISSING END OF THE SENTENCE. The tail monitor truncates the
    /// buffer, so it is destructive, so it must not run on a bar derived
    /// from the whole-utterance statistic. 185 cuts over 2026-08-31/09-01
    /// discarded 243.9s of audio and fired on 98% of tails examined.
    #[test]
    fn tail_cut_does_not_delete_the_end_of_the_users_sentence() {
        let base = 0.35f32;
        let old = base * super::SPEAKER_TAIL_THRESHOLD_FACTOR;
        let new = super::speaker_gate_window_threshold(base, false);

        // The stale arithmetic: the old comment claimed 0.30, but against
        // the user's real pref it was 0.21.
        assert!((old - 0.21).abs() < 1e-6, "old bar was 0.21, not 0.30");

        // The contradiction, in one utterance.
        assert!(
            !super::speaker_gate_allows_score(Some(USER_TAIL_SCORE_THAT_WAS_CUT), old),
            "0.1447 is why the old bar deleted the user's last words"
        );
        assert!(
            super::speaker_gate_allows_score(Some(USER_TAIL_SCORE_THAT_WAS_CUT), new),
            "the tail of a 0.5764 utterance must survive"
        );
        assert_eq!(
            super::speaker_confirmation(true, Some(SAME_UTTERANCE_WHOLE_SCORE), base),
            super::SpeakerConfirmation::Confirmed(SAME_UTTERANCE_WHOLE_SCORE),
            "the same utterance was simultaneously confirmed as the user"
        );

        // Media tails measure ~0.05 through real speakers, and observed cut
        // scores sit at p25 0.0454 — those must still be cut, or background
        // dialogue reopens the end-of-utterance problem this monitor solves.
        for media_tail in [-0.1263f32, 0.0, 0.0454, 0.05, 0.10] {
            assert!(
                !super::speaker_gate_allows_score(Some(media_tail), new),
                "media tail at {media_tail} must still be cut"
            );
        }
    }

    /// Keeping more tail cannot leak media into the paste: the kept buffer
    /// still meets the mandatory whole-utterance confirmation, which judges
    /// a media-dominated utterance as a whole rather than trimming it.
    #[test]
    fn relaxed_tail_cut_cannot_leak_media_to_the_paste() {
        let base = 0.35f32;
        let new = super::speaker_gate_window_threshold(base, false);
        // A media tail loud enough to survive the relaxed cut...
        let loud_media_tail = 0.2649f32; // max observed cut score
        assert!(super::speaker_gate_allows_score(Some(loud_media_tail), new));
        // ...still cannot carry the utterance past the authoritative gate.
        assert_eq!(
            super::speaker_confirmation(true, Some(MEDIA_MAX_WHOLE_UTTERANCE_SCORE), base),
            super::SpeakerConfirmation::Refuted(MEDIA_MAX_WHOLE_UTTERANCE_SCORE)
        );
    }

    /// Both destructive decisions now share one bar. If they ever diverge
    /// again, one of them is judging a short window by a whole-utterance
    /// statistic — which is the bug this pair of fixes exists to remove.
    #[test]
    fn every_destructive_speaker_decision_uses_the_permissive_bar() {
        for base in [0.30f32, 0.35, 0.45, 0.50] {
            let permissive = super::speaker_gate_window_threshold(base, false);
            assert!(
                permissive < base,
                "a destructive decision must never borrow the authoritative bar"
            );
            assert!(permissive > 0.0, "and must stay above the noise floor");
        }
    }

    #[test]
    fn window_bar_stays_strict_while_audio_is_playing() {
        // With media present the fast bar must NOT be relaxed -- that is the
        // path a YouTube narrator used to latch an utterance through.
        let base = 0.35f32;
        let playing = super::speaker_gate_window_threshold(base, true);
        assert!(playing > base);
        assert!(0.404 < playing, "the score that leaked must still fail");
    }

    /// THE REGRESSION. A single 1.5s window crossing the bar latches
    /// `speaker_checked` for the rest of the utterance, and the tail
    /// monitor then judges everything after it at 0.6x — which ordinary
    /// media speech clears. The end-of-utterance confirmation re-scores
    /// the kept buffer AS A WHOLE, and that is the statistic that
    /// separates the two populations.
    #[test]
    fn whole_utterance_confirmation_refutes_media_that_won_one_window() {
        let threshold = 0.35f32;
        for leak in MEDIA_LEAKING_WINDOW_SCORES {
            // Each of these DID pass the single-window check...
            assert!(
                super::speaker_gate_allows_score(Some(leak), threshold),
                "{leak} passed the window check in the field"
            );
        }
        // ...but the same audio, scored whole, never reached the bar.
        assert_eq!(
            super::speaker_confirmation(true, Some(MEDIA_MAX_WHOLE_UTTERANCE_SCORE), threshold),
            super::SpeakerConfirmation::Refuted(MEDIA_MAX_WHOLE_UTTERANCE_SCORE),
            "the worst media utterance must be refuted"
        );
        // And a latched `speaker_checked` is no defence: STT is still
        // "allowed" by the ladder, so the confirmation is the only thing
        // standing between media audio and the clipboard.
        assert!(super::speaker_gate_allows_stt(true, true));
    }

    /// The confirmation must not become a new way to lose the user's
    /// speech.
    #[test]
    fn whole_utterance_confirmation_keeps_the_users_own_speech() {
        let threshold = 0.35f32;
        assert_eq!(
            super::speaker_confirmation(true, Some(USER_MIN_OBSERVED_SCORE), threshold),
            super::SpeakerConfirmation::Confirmed(USER_MIN_OBSERVED_SCORE),
            "the user's quietest verified utterance must still pass"
        );
        // Exactly at the bar counts as a match, same as every other
        // speaker-gate comparison.
        assert_eq!(
            super::speaker_confirmation(true, Some(threshold), threshold),
            super::SpeakerConfirmation::Confirmed(threshold)
        );
    }

    /// No evidence is not evidence of guilt: too little voiced audio, or
    /// an embedder error, must defer to the ladder rather than drop.
    #[test]
    fn whole_utterance_confirmation_defers_when_it_cannot_judge() {
        let threshold = 0.35f32;
        assert_eq!(
            super::speaker_confirmation(false, None, threshold),
            super::SpeakerConfirmation::Insufficient,
            "a tail too short to embed must not discard committed speech"
        );
        assert_eq!(
            super::speaker_confirmation(false, Some(0.01), threshold),
            super::SpeakerConfirmation::Insufficient
        );
        assert_eq!(
            super::speaker_confirmation(true, None, threshold),
            super::SpeakerConfirmation::Insufficient,
            "an embed failure must not newly drop a verified utterance"
        );
    }

    /// The two populations, as measured, must be separable by the
    /// whole-utterance bar the user already configured. If this ever
    /// stops holding, the fix is a better statistic — not a threshold
    /// nudge, which provably cannot separate the single-window scores
    /// (media 0.362-0.375 sits inside the user's 0.355-0.451).
    #[test]
    fn media_and_user_are_separable_whole_utterance_but_not_per_window() {
        let threshold = 0.35f32;
        // Per window: the populations overlap, so NO threshold works.
        let media_window_max = MEDIA_LEAKING_WINDOW_SCORES
            .iter()
            .copied()
            .fold(f32::MIN, f32::max);
        assert!(
            media_window_max > USER_MIN_OBSERVED_SCORE,
            "single-window scores overlap — a threshold alone can never fix this"
        );
        // Whole utterance: they separate, with the bar inside the gap.
        assert!(
            MEDIA_MAX_WHOLE_UTTERANCE_SCORE < threshold,
            "every media utterance scored below the bar"
        );
        assert!(
            USER_MIN_OBSERVED_SCORE >= threshold,
            "the user's own speech scored at or above it"
        );
    }

    #[test]
    fn preview_cadence_throttles_local_streaming_engine() {
        for consume in [false, true] {
            for pref in [false, true] {
                let cadence = super::preview_cadence(consume, true, true, false, pref)
                    .expect("local streaming engine arms a throttled preview");

                // The three throttles the cloud path gives up.
                assert_eq!(cadence.interval_ms, super::LOCAL_STREAM_INTERVAL_MS);
                assert_eq!(cadence.min_new_samples, super::LOCAL_STREAM_MIN_NEW_SAMPLES);
                assert!(
                    cadence.require_speculation_idle,
                    "a local preview must yield to speculation — they share the engine"
                );

                // The properties that actually bound CPU. Stated as
                // inequalities so tuning the constants can't silently
                // reintroduce the unthrottled behaviour.
                assert!(
                    cadence.interval_ms >= 560,
                    "must not fire faster than one Nemotron chunk period"
                );
                assert!(
                    cadence.interval_ms > super::CONSUME_STREAM_INTERVAL_MS,
                    "must be strictly slower than the network-bound cadence"
                );
                assert!(
                    cadence.min_new_samples > 0,
                    "min_new_samples: 0 re-decodes identical audio on a compute-bound engine"
                );
            }
        }
    }
}
