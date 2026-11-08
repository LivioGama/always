//! `always test`: run the real pipeline once and say which step breaks.

// A terminal diagnostic: stdout is its interface.
#![allow(clippy::print_stdout)]

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::always::log::Logger;
use crate::always::{AlwaysConfig, audio, daemon, paste, vad};
use crate::managers::model_registry::ModelRegistry;
use crate::stt_dispatch::build_transcriber;

const LEVEL_WINDOW: Duration = Duration::from_secs(3);
const PASTE_COUNTDOWN_SECS: u64 = 5;

pub fn run(cfg: &AlwaysConfig) -> Result<()> {
    if daemon::is_running() || daemon::accepts_commands() {
        bail!(
            "the Always daemon is running and would dictate this test too. \
             Run `always stop`, then `always test` again"
        );
    }
    let mut cfg = cfg.clone();
    // Only step 3 may write into another app.
    cfg.live_typing = false;

    println!("Step 1/3: microphone");
    audio::get_or_spawn()
        .with_context(|| format!("could not open the microphone. {}", mic_hint()))?;
    let registry = ModelRegistry::new().context("could not read the speech models")?;
    #[cfg(target_os = "linux")]
    {
        cfg.transcriber_backend = crate::stt_dispatch::linux_startup_backend(&cfg, &registry);
    }
    let transcriber =
        build_transcriber(&cfg, &registry).context("could not load the speech engine")?;

    println!("  Opened. Say one short sentence now.");
    let Some(peak) = peak_input_level()? else {
        bail!(
            "the microphone opened but delivered no audio. {}",
            mic_hint()
        );
    };
    let threshold = vad::voice_activity_energy_threshold(&cfg);
    println!("  Loudest input: {peak:.4} (speech needs about {threshold:.4})");
    if peak < threshold {
        println!(
            "  That is very quiet: check that this is the right microphone and it isn't muted."
        );
    }

    println!("Step 2/3: transcription with {}", cfg.transcriber_backend);
    let rt = tokio::runtime::Runtime::new()?;
    let mut log = Logger::open(&cfg.log_path)?;
    let result = vad::record_utterance(&cfg, &mut log, &transcriber, rt.handle())
        .context("transcription failed")?;
    let text = match result {
        vad::RecordResult::Speech { text, .. } if !text.trim().is_empty() => text,
        other => bail!("{}", no_text_reason(&other, cfg.timeout_secs)),
    };
    println!("  Heard: {text}");

    println!("Step 3/3: writing into an app");
    println!("  Click where the text should go.");
    for remaining in (1..=PASTE_COUNTDOWN_SECS).rev() {
        println!("  Pasting in {remaining}...");
        std::thread::sleep(Duration::from_secs(1));
    }
    paste::copy_to_clipboard(text).context("could not write to the clipboard")?;
    paste::paste_text(false).context("could not send the paste keystroke")?;
    println!("  Sent. If nothing appeared, press the paste shortcut yourself:");
    println!("  if the text appears then, only the automatic keystroke is failing.");
    Ok(())
}

/// Loudest frame over [`LEVEL_WINDOW`], or `None` if no audio ever arrived.
fn peak_input_level() -> Result<Option<f64>> {
    let started = Instant::now();
    let mut peak = None;
    while started.elapsed() < LEVEL_WINDOW {
        if let Some(energy) = vad::newest_frame_energy()? {
            peak = Some(energy.max(peak.unwrap_or(0.0)));
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Ok(peak)
}

fn mic_hint() -> &'static str {
    if cfg!(target_os = "linux") {
        "`arecord -l` lists your microphones; with PipeWire, install `pipewire-alsa` so ALSA can reach it"
    } else {
        "Check that a microphone is connected and allowed in your system's privacy settings"
    }
}

fn no_text_reason(result: &vad::RecordResult, timeout_secs: u32) -> String {
    match result {
        vad::RecordResult::Speech { .. } => "the speech engine returned no text".to_string(),
        vad::RecordResult::Silence | vad::RecordResult::Timeout => {
            format!("no speech detected within {timeout_secs} s")
        }
        vad::RecordResult::DroppedLowEnergy { energy } => {
            format!("speech was too quiet (level {energy:.4})")
        }
        vad::RecordResult::DroppedNoise { raw } => {
            format!("the speech engine only heard noise: {raw:?}")
        }
        vad::RecordResult::DroppedSpeaker { score } => {
            format!("My Voice rejected this voice (score {score:.2})")
        }
        vad::RecordResult::PreemptedByMicConflict { .. } => {
            "another app took the microphone".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_speech_names_the_timeout() {
        assert!(no_text_reason(&vad::RecordResult::Timeout, 15).contains("15 s"));
    }
}
