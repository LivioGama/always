use anyhow::{Context, Result};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

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
}

pub enum RecordResult {
    Speech {
        text: String,
        energy: f64,
        transcription: crate::stt::TranscriptionResult,
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
) -> Result<RecordResult> {
    record_with_local_vad(cfg, log, transcriber)
}

/// Single-frame energy probe used while idle-paused so the user can wake
/// listening by speaking without manually lifting pause.
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
const NORMAL_SILENCE_CAP_SECS: f64 = 0.50;
const NORMAL_SILENCE_FLOOR_SECS: f64 = 0.30;
const EARLY_VOICE_ENERGY_RATIO: f64 = 0.70;
const EARLY_VOICE_FALSE_START_MS: u32 = 150;

fn record_with_local_vad(
    cfg: &AlwaysConfig,
    log: &mut Logger,
    transcriber: &Arc<dyn Transcriber>,
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
    let max_speech_frames = (300_usize * 1000) / FRAME_MS as usize;
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
            if let Some(ref mut rec) = recorder.as_mut() {
                rec.read_frame(&mut frame_buf)?
            } else {
                return Err(anyhow::anyhow!("Audio recorder not available"));
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
        let credible_early_voice = !in_speech
            && !voice_activity_announced
            && frame_energy >= early_voice_energy_threshold
            && last_prob >= speech_threshold * 0.55;
        if credible_early_voice {
            announce_voice_activity!();
            tentative_voice_silence = 0;
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
                // Same anchor refresh for the case where we were already in
                // speech (consecutive_speech overflows min_speech_frames
                // immediately) — keep the watchdog clock pinned to "right
                // now" for as long as audio is genuinely voice.
                pause::mark_voice_seen();
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

            // At tentative silence, kick off speculative transcription in the
            // background so the result is ready (or nearly so) by the time we
            // hit final silence. If the user resumes, we discard it above.
            if voice_logged && !speculation_pending && consecutive_silence >= eff_tentative_frames {
                speculation_pending = true;
                speculation_slot.invalidate();
                let captured_gen = speculation_slot.current_generation();
                // Overlay → Transcribing when speculative STT starts.
                flip_to_transcribing!();
                let audio_snapshot = speech_samples.clone();
                let transcriber_for_spec = Arc::clone(transcriber);
                let slot = Arc::clone(&speculation_slot);
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
                    slot.store_if_current(captured_gen, outcome);
                });
            }

            if consecutive_silence >= eff_silence_frames {
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

    if speech_samples.is_empty() {
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
    let speech_energy = if use_fast_energy_check {
        fast_normalized_energy(&speech_samples)
    } else {
        normalized_energy(&speech_samples)
    };

    // Only log drops if we actually logged voice detection
    if speech_energy < cfg.energy_threshold {
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

    // Try to use the speculative transcription if it was kicked off and
    // wasn't invalidated by a speech resume. Wait briefly if it's still in
    // flight — it has been running for up to (silence_secs - tentative) seconds
    // already, so it's likely complete or close to it.
    let speculation = if speculation_pending {
        let started_wait = std::time::Instant::now();
        let max_wait = std::time::Duration::from_secs(10);
        let mut taken: Option<Result<crate::stt::TranscriptionResult>> = None;
        loop {
            if let Some(r) = speculation_slot.take() {
                taken = Some(r);
                break;
            }
            if started_wait.elapsed() >= max_wait {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        taken
    } else {
        None
    };

    let result = match speculation {
        Some(Ok(r)) => {
            event::global_broadcaster().transcribing_stopped();
            r
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
            match transcriber.transcribe_from_bytes(wav_data) {
                Ok(result) => {
                    event::global_broadcaster().transcribing_stopped();
                    result
                }
                Err(err) => {
                    event::global_broadcaster().transcribing_stopped();
                    return Err(err).context("failed to transcribe utterance");
                }
            }
        }
    };

    let raw = result.text.clone();

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

fn normal_silence_frames(cfg: &AlwaysConfig) -> usize {
    let secs = cfg
        .silence_secs
        .clamp(NORMAL_SILENCE_FLOOR_SECS, NORMAL_SILENCE_CAP_SECS);
    ((secs * 1000.0) / FRAME_MS as f64).ceil() as usize
}

fn tentative_silence_frames(final_silence_frames: usize) -> usize {
    (final_silence_frames / 3).max(1)
}

#[cfg(test)]
mod tests {
    use super::{
        fast_energy_check, fast_normalized_energy, normal_silence_frames, normalized_energy,
        tentative_silence_frames, voice_activity_energy_threshold,
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
    fn normal_silence_window_is_capped_for_responsive_finalize() {
        let cfg = AlwaysConfig {
            silence_secs: 2.0,
            ..Default::default()
        };
        assert_eq!(normal_silence_frames(&cfg), 17);

        let cfg = AlwaysConfig {
            silence_secs: 0.1,
            ..Default::default()
        };
        assert_eq!(normal_silence_frames(&cfg), 10);

        let cfg = AlwaysConfig {
            silence_secs: 0.45,
            ..Default::default()
        };
        assert_eq!(normal_silence_frames(&cfg), 15);
    }

    #[test]
    fn tentative_silence_starts_before_final_cutoff() {
        assert_eq!(tentative_silence_frames(17), 5);
        assert_eq!(tentative_silence_frames(1), 1);
    }
}
