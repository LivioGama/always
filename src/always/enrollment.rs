//! "My Voice" guided enrollment — recording orchestration.
//!
//! The UDS server can't record directly: the microphone pipeline is
//! single-threaded by invariant (see `vad.rs` / `GLOBAL_RECORDER`), so
//! enrollment requests are queued here and drained by the event loop
//! on its own thread. To keep the Settings UI responsive, `vad.rs`
//! also polls [`is_pending`] each frame and bails out of any capture
//! in progress the moment a request lands — otherwise a request could
//! sit behind a 30s pre-voice timeout.
//!
//! One recording captures a guided sample for one step (normal /
//! lower / louder tone): frames stream from the shared recorder,
//! Silero VAD counts voiced audio, level events feed the UI meter at
//! ~10 Hz, and once enough voice accumulates the sample is embedded
//! and folded into the persisted voiceprint.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use parking_lot::Mutex;

use crate::always::audio::{self, FRAME_BYTES, FRAME_MS, FRAME_SAMPLES};
use crate::always::config::AlwaysConfig;
use crate::always::vad_silero::SileroVad;
use crate::always::voiceprint::{self, EnrollStep};
use crate::always::{event, speaker_embed};

/// Voiced audio required per guided sample. 5s of actual voice gives
/// the embedding a stable identity while keeping each step short.
const TARGET_VOICED_MS: u32 = 5_000;
/// Hard wall-clock cap per recording — covers "user clicked record and
/// walked away" without wedging the event loop forever.
const MAX_RECORDING_SECS: u64 = 45;
/// Level-meter throttle (frames): 30ms frames × 3 ≈ 10 Hz.
const LEVEL_EVERY_FRAMES: u32 = 3;

static PENDING: Mutex<Option<EnrollStep>> = Mutex::new(None);
static CANCEL: AtomicBool = AtomicBool::new(false);

/// Queue an enrollment recording. Returns `false` if one is already
/// queued (the UI should never double-fire, but the socket is open to
/// any client).
pub fn request_step(step: EnrollStep) -> bool {
    let mut slot = PENDING.lock();
    if slot.is_some() {
        return false;
    }
    CANCEL.store(false, Ordering::SeqCst);
    *slot = Some(step);
    true
}

pub fn request_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
    // A queued-but-not-started request is dropped outright.
    *PENDING.lock() = None;
}

/// Cheap per-frame check used by `vad.rs` to abandon an in-flight
/// capture so the event loop can service the enrollment promptly.
pub fn is_pending() -> bool {
    PENDING.lock().is_some()
}

/// Take the queued request, if any. Called by the event loop only.
pub fn take_pending() -> Option<EnrollStep> {
    PENDING.lock().take()
}

/// Current profile snapshot for `VoiceProfileStatus` broadcasts.
pub fn profile_status(cfg_enabled: bool) -> (bool, bool, Vec<String>) {
    let (enrolled, steps) = match voiceprint::current() {
        Some(p) => (p.is_complete(), p.recorded_steps()),
        None => (false, Vec::new()),
    };
    (enrolled, cfg_enabled, steps)
}

/// Record one guided sample and fold it into the voiceprint. Runs on
/// the event-loop thread. Broadcasts progress/terminal events itself;
/// the returned Result is for the caller's log line only.
pub fn run_enrollment(cfg: &AlwaysConfig, step: EnrollStep) -> Result<()> {
    let broadcaster = event::global_broadcaster();
    let result = record_and_store(cfg, step);
    match &result {
        Ok(()) => {
            broadcaster.voice_enrollment_sample_captured(step.as_str());
            let (enrolled, enabled, steps) = profile_status(cfg.speaker_gate_enabled);
            broadcaster.voice_profile_status(enrolled, enabled, steps);
            tracing::info!(step = step.as_str(), "voice_enrollment_captured");
        }
        Err(e) => {
            broadcaster.voice_enrollment_failed(step.as_str(), format!("{e:#}"));
            tracing::warn!(step = step.as_str(), error = %e, "voice_enrollment_failed");
        }
    }
    result
}

fn record_and_store(cfg: &AlwaysConfig, step: EnrollStep) -> Result<()> {
    // Model download happens lazily on the FIRST enrollment (26.5 MB).
    // Do it before announcing the recording so the UI's "speak now"
    // cue never fires while we're still fetching.
    speaker_embed::ensure_model().context("speaker model unavailable")?;
    let embedder =
        speaker_embed::global().context("speaker embedding engine failed to load")?;

    let vad = SileroVad::new().context("failed to load Silero VAD")?;
    let broadcaster = event::global_broadcaster();
    broadcaster.voice_enrollment_started(step.as_str());

    #[cfg(feature = "macos")]
    {
        let recorder_arc = audio::RecChild::get_or_spawn()?;
        let mut frame_buf = [0u8; FRAME_BYTES];
        let mut sample_buf = [0i16; FRAME_SAMPLES];
        // Collect everything once voice starts (natural pauses included —
        // the embedding recipe tolerates them; only VOICED time counts
        // toward the target).
        let mut samples: Vec<i16> = Vec::with_capacity(16_000 * 8);
        let mut voiced_ms: u32 = 0;
        let mut started = false;
        let mut frames_since_level: u32 = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(MAX_RECORDING_SECS);

        loop {
            if CANCEL.load(Ordering::SeqCst) {
                anyhow::bail!("cancelled");
            }
            if std::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "not enough speech captured ({}s of voice needed)",
                    TARGET_VOICED_MS / 1000
                );
            }

            let read = {
                let mut recorder = recorder_arc.lock();
                let rec = recorder
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("audio recorder not available"))?;
                match rec.read_frame(&mut frame_buf) {
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                        recorder.take();
                        anyhow::bail!("recorder wedged during enrollment");
                    }
                    Err(e) => return Err(e.into()),
                }
            };
            if read == 0 {
                recorder_arc.lock().take();
                anyhow::bail!("recorder stopped during enrollment");
            }
            if read < FRAME_BYTES {
                continue;
            }
            for (i, chunk) in frame_buf.chunks_exact(2).enumerate() {
                sample_buf[i] = i16::from_le_bytes([chunk[0], chunk[1]]);
            }

            let voiced = vad
                .predict(&sample_buf)
                .map(|p| p >= cfg.silero_threshold)
                .unwrap_or(false);
            if voiced {
                started = true;
                voiced_ms += FRAME_MS;
            }
            if started {
                samples.extend_from_slice(&sample_buf);
            }

            frames_since_level += 1;
            if frames_since_level >= LEVEL_EVERY_FRAMES {
                frames_since_level = 0;
                let energy = frame_energy(&sample_buf);
                broadcaster.voice_enrollment_level(energy, voiced_ms, TARGET_VOICED_MS);
            }

            if voiced_ms >= TARGET_VOICED_MS {
                break;
            }
        }

        let embedding = embedder
            .embed(&samples)
            .context("failed to compute voice embedding")?;
        voiceprint::set_step(step, embedding).context("failed to persist voiceprint")?;
    }
    
    #[cfg(not(feature = "macos"))]
    {
        anyhow::bail!("voice enrollment is only supported on macOS");
    }

    Ok(())
}

/// Same normalized RMS the daemon's energy gates use (see
/// `vad::normalized_energy`), duplicated in miniature to avoid making
/// that helper public just for a meter.
fn frame_energy(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum_sq / samples.len() as f64).sqrt() / 32768.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_queue_single_slot() {
        // Ensure clean state (tests share process globals).
        take_pending();
        assert!(request_step(EnrollStep::Normal));
        assert!(!request_step(EnrollStep::Lower), "second queue must fail");
        assert!(is_pending());
        assert_eq!(take_pending(), Some(EnrollStep::Normal));
        assert!(!is_pending());
    }

    #[test]
    fn cancel_clears_queue() {
        take_pending();
        assert!(request_step(EnrollStep::Louder));
        request_cancel();
        assert!(!is_pending());
        assert_eq!(take_pending(), None);
    }

    #[test]
    fn frame_energy_of_silence_is_zero() {
        assert_eq!(frame_energy(&[0i16; 480]), 0.0);
        assert!(frame_energy(&[16384i16; 480]) > 0.4);
    }
}
