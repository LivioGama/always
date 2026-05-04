use anyhow::{Context, Result};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use voice_activity_detector::VoiceActivityDetector;

use crate::always::audio::{self, FRAME_BYTES, FRAME_MS, FRAME_SAMPLES};
use crate::always::config::AlwaysConfig;
use crate::always::event;
use crate::always::log::{Event, Logger};

/// Speculative transcription result, populated by a background thread.
type SpeculationSlot = Arc<Mutex<Option<Result<crate::stt::TranscriptionResult>>>>;

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

pub fn record_utterance(cfg: &AlwaysConfig, log: &mut Logger) -> Result<RecordResult> {
    record_with_local_vad(cfg, log)
}

fn record_with_local_vad(cfg: &AlwaysConfig, log: &mut Logger) -> Result<RecordResult> {
    let silence_frames = ((cfg.silence_secs * 1000.0) / FRAME_MS as f64).ceil() as usize;
    let max_frames = (cfg.timeout_secs as usize * 1000) / FRAME_MS as usize;
    let min_speech_frames = (cfg.onset_ms / FRAME_MS).max(1) as usize;
    // Two-stage end-of-utterance detection (Option B):
    // - At `tentative_silence_frames`, kick off a SPECULATIVE transcription in a
    //   background thread using the audio captured so far.
    // - Continue recording until `silence_frames` (final).
    // - If speech resumes during the tentative window, discard the speculation.
    // - At final, if speculation is still valid (no resume), use its result —
    //   transcription has been running in parallel during the silence wait, so
    //   the user gets snappy paste with no extra latency cost.
    // Tentative at 75% of final window (not 50%) to allow user to finish thought before speculation.
    // If model returns early on partial audio, we still wait for final silence before accepting result.
    let tentative_silence_secs = (cfg.silence_secs * 0.75).clamp(0.6, 2.0);
    let tentative_silence_frames =
        ((tentative_silence_secs * 1000.0) / FRAME_MS as f64).ceil() as usize;
    // Pre-buffer: keep 50ms of audio before speech detection to catch first words
    let pre_buffer_frames = (50 / FRAME_MS as usize).max(1);
    let mut vad = VoiceActivityDetector::builder()
        .sample_rate(16000_i64)
        .chunk_size(512_usize)
        .build()
        .context("failed to build Silero VAD")?;
    let speech_threshold: f32 = cfg.silero_threshold;
    // Hysteresis: easier to STAY in speech than to ENTER it.
    // Silero's probability naturally dips below 0.5 during voiceless consonants
    // (h, s, f), inter-syllable pauses, and quiet syllables — without hysteresis,
    // these brief dips accumulate consecutive_silence and prematurely cut the
    // utterance mid-sentence. Tighter threshold (75% vs 60%) prevents cutoff on
    // trailing soft consonants like 's' in "models", 'f' in "leaf", 'h' in "with".
    let silence_threshold: f32 = speech_threshold * 0.75;
    let mut vad_accum: Vec<i16> = Vec::with_capacity(1024);
    let mut last_prob: f32 = 0.0;

    // Use persistent recorder to avoid process spawning overhead
    let recorder_arc = audio::RecChild::get_or_spawn()?;
    let mut frame_buf = [0u8; FRAME_BYTES];
    // Pre-allocate ~4 seconds of audio (16000 Hz * 4s = 64000 samples) to avoid Vec growth
    let mut speech_samples: Vec<i16> = Vec::with_capacity(64_000);
    let mut consecutive_silence = 0usize;
    let mut consecutive_speech = 0usize;
    let mut in_speech = false;
    let mut voice_logged = false;
    let mut total_frames = 0usize;
    let mut pre_buffer: VecDeque<Vec<i16>> = VecDeque::with_capacity(pre_buffer_frames);

    // Optimization for very low energy thresholds (≤ 0.01): use fast energy check
    let use_fast_energy_check = cfg.energy_threshold <= 0.01;
    let fast_energy_threshold = cfg.energy_threshold;

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
    let speculation_slot: SpeculationSlot = Arc::new(Mutex::new(None));
    let mut speculation_pending = false;

    loop {
        let read = {
            let mut recorder = recorder_arc.lock();
            if let Some(ref mut rec) = recorder.as_mut() {
                rec.read_frame(&mut frame_buf)?
            } else {
                return Err(anyhow::anyhow!("Audio recorder not available"));
            }
        };

        if read < FRAME_BYTES {
            break;
        }

        for (i, chunk) in frame_buf.chunks_exact(2).enumerate() {
            sample_buf[i] = i16::from_le_bytes([chunk[0], chunk[1]]);
        }
        let samples = &sample_buf[..];

        vad_accum.extend_from_slice(samples);
        if vad_accum.len() >= 512 {
            let chunk: Vec<i16> = vad_accum.drain(..512).collect();
            last_prob = vad.predict(chunk);
        }
        // Hysteresis: while already in speech, only exit on the lower threshold;
        // while not yet in speech, require the higher threshold to enter.
        let is_speech = if in_speech {
            last_prob >= silence_threshold
        } else {
            last_prob >= speech_threshold
        };

        if is_speech {
            // Speech resumed: discard any pending speculation (its audio snapshot
            // is now stale because more speech will be appended).
            if speculation_pending {
                speculation_pending = false;
                *speculation_slot.lock() = None;
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
                        current_energy >= cfg.energy_threshold * 0.5 // Use 50% of threshold for early check
                    };

                    if passes_energy_check {
                        log.write(Event::VoiceDetected);
                        voice_logged = true;

                        // Send voice activity detected event
                        event::global_broadcaster().voice_activity_detected();
                    }
                }
                // Prepend pre-buffer to capture audio before VAD triggered
                for buffered_samples in pre_buffer.drain(..) {
                    speech_samples.extend_from_slice(&buffered_samples);
                }
            }
            if in_speech {
                speech_samples.extend_from_slice(samples);
            }
        } else if in_speech {
            consecutive_silence += 1;
            consecutive_speech = 0;
            speech_samples.extend_from_slice(samples);

            // At tentative silence, kick off speculative transcription in the
            // background so the result is ready (or nearly so) by the time we
            // hit final silence. If the user resumes, we discard it above.
            if !speculation_pending && consecutive_silence >= tentative_silence_frames {
                speculation_pending = true;
                let audio_snapshot = speech_samples.clone();
                let api_key = cfg.groq_stt_api_key.clone();
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
                            crate::stt::transcribe_from_bytes(wav, &api_key)
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
                    *slot.lock() = Some(outcome);
                });
            }

            if consecutive_silence >= silence_frames {
                break;
            }
        } else {
            consecutive_speech = 0;
            // Maintain pre-buffer: add new frame, drop oldest if full
            pre_buffer.push_back(samples.to_vec());
            if pre_buffer.len() > pre_buffer_frames {
                pre_buffer.pop_front();
            }
        }

        total_frames += 1;
        if total_frames >= max_frames {
            break;
        }
    }

    if speech_samples.is_empty() {
        // Send voice activity ended event if no speech was detected
        event::global_broadcaster().voice_activity_ended();
        // Don't set listening to false here - let it time out naturally
        return if total_frames >= max_frames {
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

    // Send transcribing started event
    event::global_broadcaster().transcribing_started();

    // Try to use the speculative transcription if it was kicked off and
    // wasn't invalidated by a speech resume. Wait briefly if it's still in
    // flight — it has been running for up to (silence_secs - tentative) seconds
    // already, so it's likely complete or close to it.
    let speculation = if speculation_pending {
        let started_wait = std::time::Instant::now();
        let max_wait = std::time::Duration::from_secs(10);
        let mut taken: Option<Result<crate::stt::TranscriptionResult>> = None;
        loop {
            if let Some(r) = speculation_slot.lock().take() {
                taken = Some(r);
                break;
            }
            if started_wait.elapsed() >= max_wait {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
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
            match crate::stt::transcribe_from_bytes(wav_data, &cfg.groq_stt_api_key) {
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

    if raw.is_empty() {
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

#[cfg(test)]
mod tests {
    use super::{fast_energy_check, fast_normalized_energy, normalized_energy};

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
}
