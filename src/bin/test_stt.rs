use anyhow::Result;
use std::fs;
use std::path::PathBuf;

use always::glossary;
use always::stt::{Transcriber, GroqTranscriber};

/// Test script to debug STT pipeline:
/// Records audio, transcribes it, and shows each correction stage.
///
/// Usage:
///   cargo run --bin test_stt -- <audio_file.wav>
///
/// Example:
///   # Record 5 seconds and test
///   ffmpeg -f avfoundation -i ':0' -t 5 test.wav
///   cargo run --bin test_stt -- test.wav

fn main() -> Result<()> {
    // Setup tracing for visible logs
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <audio_file.wav>", args[0]);
        eprintln!("\nExample:");
        eprintln!("  # Record 5 seconds");
        eprintln!("  ffmpeg -f avfoundation -i ':0' -t 5 test.wav");
        eprintln!("  # Test the transcription pipeline");
        eprintln!("  cargo run --bin test_stt -- test.wav");
        std::process::exit(1);
    }

    let audio_path = PathBuf::from(&args[1]);
    if !audio_path.exists() {
        eprintln!("❌ Audio file not found: {}", audio_path.display());
        std::process::exit(1);
    }

    let groq_api_key = match always::always::keyring::get_groq_api_key() {
        Ok(Some(key)) => key,
        Ok(None) => {
            eprintln!("❌ GROQ_API_KEY not found in keychain");
            eprintln!("   Set it with: always config set groq_api_key YOUR_KEY");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("❌ Error reading keychain: {}", e);
            std::process::exit(1);
        }
    };

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║              STT Pipeline Test & Debug Tool                  ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    println!("📁 Audio file: {}\n", audio_path.display());

    // Read audio
    let audio_data = fs::read(&audio_path)?;
    println!("🎙️  Audio size: {} bytes\n", audio_data.len());

    // Stage 1: Raw Whisper transcription
    println!("┌─ Stage 1: Whisper Transcription ────────────────────────────┐");
    println!("│ Calling Groq API...");
    let _ = std::io::Write::flush(&mut std::io::stderr());

    let transcriber = GroqTranscriber::new(groq_api_key.clone());
    let result = match transcriber.transcribe_from_bytes(audio_data) {
        Ok(r) => {
            println!("│ ✓ Received response");
            r
        },
        Err(e) => {
            println!("❌ Transcription failed: {}", e);
            return Err(e.into());
        }
    };
    let raw_text = result.text.clone();
    println!("│");
    println!("│ Raw transcript:");
    println!("│   \"{}\"", raw_text);
    println!("│");
    println!("│ Language: {}", result.language);
    println!("│ Duration: {:.2}s", result.duration);
    if !result.segments.is_empty() {
        let avg_logprob: f32 = result
            .segments
            .iter()
            .map(|s| s.avg_logprob)
            .sum::<f32>()
            / result.segments.len() as f32;
        println!("│ Confidence (avg_logprob): {:.4}", avg_logprob);
    }
    println!("└────────────────────────────────────────────────────────────────┘\n");

    // Stage 2: Acoustic matching (glossary-based)
    println!("┌─ Stage 2: Glossary Acoustic Matching ────────────────────────┐");
    let custom_words = glossary::user_glossary_terms();
    let (acoustic_text, acoustic_subs) = always::always::text_match::apply_custom_words(
        &raw_text,
        &custom_words,
        always::always::text_match::DEFAULT_THRESHOLD,
    );
    println!("│");
    if acoustic_subs.is_empty() {
        println!("│ ✓ No glossary corrections needed");
        println!("│   \"{}\"", acoustic_text);
    } else {
        println!("│ {} corrections applied:", acoustic_subs.len());
        for (original, fixed) in &acoustic_subs {
            println!("│   \"{}\" → \"{}\"", original, fixed);
        }
        println!("│");
        println!("│ Result: \"{}\"", acoustic_text);
    }
    println!("└────────────────────────────────────────────────────────────────┘\n");

    // Stage 3: Grammar correction (LLM)
    println!("┌─ Stage 3: Grammar Correction (LLM) ──────────────────────────┐");

    // For now, skip LLM correction in test (it requires async/tokio)
    // In production, this is handled by the daemon with proper async context
    let final_text = acoustic_text.clone();

    println!("│");
    println!("│ ⓘ Grammar correction disabled in test mode");
    println!("│   (Uses LLM via Groq, requires async runtime)");
    println!("│");
    println!("│ Text to be corrected: \"{}\"", acoustic_text);
    println!("└────────────────────────────────────────────────────────────────┘\n");

    // Summary
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                         FINAL RESULT                         ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");
    println!("📝 \"{}\"", final_text);
    println!("\n✅ Pipeline complete\n");

    Ok(())
}
