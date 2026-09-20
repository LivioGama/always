//! Offline harness: transcribe a WAV through a local engine at a chosen
//! language, without the daemon, the mic, or the GUI.
//!
//!   cargo run --release --features local-stt --example transcribe_file -- <wav> <lang>
//!
//! `lang` is a bare ISO 639-1 code ("en", "ne", "ko") or "auto".
use always::always::stt_local::LocalTranscriber;
use always::managers::model_registry::EngineType;
use always::stt::Transcriber;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let wav = args.next().expect("usage: transcribe_file <wav> <lang>");
    let lang = args.next().unwrap_or_else(|| "auto".into());

    let model_id = args.next().unwrap_or_else(|| "nemotron".into());
    let (engine, file, streaming) = match model_id.as_str() {
        "large" => (EngineType::Whisper, "ggml-large-v3-q5_0.bin", false),
        "parakeet" => (EngineType::Parakeet, "parakeet-tdt-0.6b-v2-int8", false),
        _ => (
            EngineType::Nemotron,
            "nemotron-3.5-asr-streaming-0.6b",
            true,
        ),
    };
    let model = dirs::data_dir().unwrap().join("always/models").join(file);
    let language = if lang == "auto" {
        None
    } else {
        Some(lang.clone())
    };

    let started = std::time::Instant::now();
    let t = LocalTranscriber::load(engine, &PathBuf::from(model), language, streaming)?;
    let load_ms = started.elapsed().as_millis();

    let audio = std::fs::read(&wav)?;
    let audio_secs = (audio.len().saturating_sub(44)) as f64 / 32_000.0;
    let started = std::time::Instant::now();
    let out = t
        .transcribe_from_bytes(audio)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let ms = started.elapsed().as_millis();

    println!(
        "model={model_id} lang={lang}  load={load_ms}ms  decode={ms}ms  audio={audio_secs:.1}s  rtf={:.2}x",
        (ms as f64 / 1000.0) / audio_secs.max(0.001)
    );
    println!("TEXT: {}", out.text.trim());
    Ok(())
}
