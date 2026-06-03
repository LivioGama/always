use anyhow::{Context, Result};
use std::collections::VecDeque;
use webrtc_vad::{Vad, VadMode};

use crate::always::AlwaysConfig;
use crate::always::audio::{self, FRAME_BYTES, FRAME_MS};
use crate::always::loading;
use crate::always::log::{Event, Logger};

pub enum RecordResult {
    Speech { text: String, energy: f64 },
    Silence,
    DroppedLowEnergy { energy: f64 },
    DroppedNoise { raw: String },
    Timeout,
}

pub fn record_utterance(cfg: &AlwaysConfig, log: &mut Logger) -> Result<RecordResult> {
    record_with_local_vad(cfg, log)
}

fn record_with_local_vad(cfg: &AlwaysConfig, log: &mut Logger) -> Result<RecordResult> {
    let silence_frames = ((cfg.silence_secs * 1000.0) / FRAME_MS as f64).ceil() as usize;
    let max_frames = (cfg.timeout_secs as usize * 1000) / FRAME_MS as usize;
    let min_speech_frames = (cfg.onset_ms / FRAME_MS).max(1) as usize;
    // Pre-buffer: keep 200ms of audio before speech detection to catch first words
    let pre_buffer_frames = (200 / FRAME_MS as usize).max(1);
    let mut vad =
        Vad::new_with_rate_and_mode(webrtc_vad::SampleRate::Rate16kHz, VadMode::LowBitrate);

    // Use persistent recorder to avoid process spawning overhead
    let recorder_arc = audio::RecChild::get_or_spawn()?;
    let mut frame_buf = [0u8; FRAME_BYTES];
    let mut speech_samples = Vec::new();
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
        (fast_energy_threshold as f64 * 32768.0).powi(2) as i64
    } else {
        0
    };

    loop {
        let read = {
            let mut recorder = recorder_arc.lock().unwrap();
            if let Some(ref mut rec) = recorder.as_mut() {
                rec.read_frame(&mut frame_buf)?
            } else {
                return Err(anyhow::anyhow!("Audio recorder not available"));
            }
        };

        if read < FRAME_BYTES {
            break;
        }

        let samples: Vec<i16> = frame_buf
            .chunks_exact(2)
            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();

        let is_speech = vad.is_voice_segment(&samples).unwrap_or(false);

        if is_speech {
            consecutive_speech += 1;
            consecutive_silence = 0;
            if consecutive_speech >= min_speech_frames {
                in_speech = true;
                if !voice_logged {
                    // Fast energy check for very low thresholds to minimize latency
                    let passes_energy_check = if use_fast_energy_check {
                        fast_energy_check(&samples, fast_energy_threshold_sq)
                    } else {
                        let current_energy = normalized_energy(&samples);
                        current_energy >= cfg.energy_threshold * 0.5 // Use 50% of threshold for early check
                    };

                    if passes_energy_check {
                        log.write(Event::VoiceDetected);
                        voice_logged = true;
                        if let Err(e) = loading::show_loading_overlay() {
                            eprintln!("Failed to show voice detection overlay: {}", e);
                        }
                    }
                }
                // Prepend pre-buffer to capture audio before VAD triggered
                for buffered_samples in pre_buffer.drain(..) {
                    speech_samples.extend_from_slice(&buffered_samples);
                }
            }
            if in_speech {
                speech_samples.extend_from_slice(&samples);
            }
        } else if in_speech {
            consecutive_silence += 1;
            consecutive_speech = 0;
            speech_samples.extend_from_slice(&samples);
            if consecutive_silence >= silence_frames {
                break;
            }
        } else {
            consecutive_speech = 0;
            // Maintain pre-buffer: add new frame, drop oldest if full
            pre_buffer.push_back(samples.clone());
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
        // Hide any loading overlay if no speech was detected
        loading::stop_loading();
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
        loading::stop_loading();
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

    // Show loading overlay while transcribing
    if let Err(e) = loading::show_persistent_overlay() {
        eprintln!("Failed to show loading overlay: {}", e);
    }

    // Create WAV data in memory instead of writing to disk
    let wav_data = match audio::create_wav_bytes_i16_mono_16k(&speech_samples) {
        Ok(data) => data,
        Err(err) => {
            loading::stop_loading();
            return Err(err).context("failed to create WAV data in memory");
        }
    };

    let raw = match crate::stt::transcribe_from_bytes(wav_data, Some(&cfg.lang), &cfg.groq_stt_api_key, "whisper-large-v3") {
        Ok(raw) => {
            // Hide loading overlay on success
            loading::stop_loading();
            raw
        },
        Err(err) => {
            // Hide loading overlay on error
            loading::stop_loading();
            return Err(err).context("failed to transcribe utterance");
        }
    };

    if raw.is_empty() {
        return Ok(RecordResult::DroppedNoise { raw });
    }

    Ok(RecordResult::Speech {
        text: raw,
        energy: speech_energy,
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
    use super::{normalized_energy, fast_normalized_energy, fast_energy_check};

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
            1000, -800, 1200, -900, 600, -700, 1500, -1100,
            400, -300, 800, -600, 200, -100, 900, -750,
        ];

        let standard_energy = normalized_energy(&test_samples);
        let fast_energy = fast_normalized_energy(&test_samples);

        // Fast method should be very close to standard method
        assert!((fast_energy - standard_energy).abs() < 0.001);
    }
}
