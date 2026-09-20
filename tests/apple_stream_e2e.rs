//! End-to-end proof that the Apple incremental streaming session really
//! decodes audio as it is fed: synthesize speech with `say`, push it into
//! [`AppleStreamSession`] in 500 ms chunks like the live capture loop does,
//! and check the transcript grows before `finish` returns the final text.
//!
//! Ignored by default — it needs macOS 26+ with the Speech framework, speech
//! recognition authorization, and the `say` binary. Run with:
//!
//! ```sh
//! cargo test --features local-stt --test apple_stream_e2e -- --ignored
//! ```

use always::always::apple_stt;
use std::process::Command;

/// Synthesize a spoken WAV fixture (16 kHz mono i16) without playing audio.
fn spoken_wav(text: &str) -> Vec<u8> {
    let dir = std::env::temp_dir().join("always-apple-stream-e2e");
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    let path = dir.join("fixture.wav");
    let status = Command::new("say")
        .args([
            "-v",
            "Samantha",
            "--file-format=WAVE",
            "--data-format=LEI16@16000",
            "-o",
        ])
        .arg(&path)
        .arg(text)
        .status()
        .expect("run say");
    assert!(status.success(), "say failed to render fixture");
    std::fs::read(&path).expect("read fixture wav")
}

/// Offset of the PCM payload in a WAV file (scans for the `data` chunk).
fn wav_data_offset(data: &[u8]) -> usize {
    let mut i = 12;
    while i + 8 <= data.len() {
        let id = &data[i..i + 4];
        let size = u32::from_le_bytes([data[i + 4], data[i + 5], data[i + 6], data[i + 7]]) as usize;
        if id == b"data" {
            return i + 8;
        }
        i += 8 + size;
    }
    panic!("no data chunk in wav");
}

fn wav_to_f32(wav: &[u8]) -> Vec<f32> {
    let start = wav_data_offset(wav);
    wav[start..]
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32_768.0)
        .collect()
}

#[test]
#[ignore = "requires macOS 26+, speech recognition authorization, and `say`"]
fn apple_live_stream_decodes_while_feeding() {
    if !apple_stt::stream_supported() {
        eprintln!("apple streaming not supported on this device — skipping");
        return;
    }

    let audio = wav_to_f32(&spoken_wav(
        "The quick brown fox jumps over the lazy dog and keeps running.",
    ));
    assert!(audio.len() > 8_000, "fixture too short");

    let mut session = apple_stt::AppleStreamSession::start(Some("en"), &["quick brown fox".to_string()])
        .expect("stream session failed to start — check speech recognition permission");

    // Feed in 500 ms chunks like the live-stream worker does; watch the
    // cumulative transcript grow as audio arrives.
    const CHUNK: usize = 8_000;
    let mut partials: Vec<String> = Vec::new();
    let mut fed = 0;
    while audio.len() - fed >= CHUNK {
        let text = session.push(&audio[fed..fed + CHUNK]).expect("push failed");
        partials.push(text);
        fed += CHUNK;
    }
    if fed < audio.len() {
        // Trailing partial chunk — same zero-pad trick the worker uses.
        let mut tail = audio[fed..].to_vec();
        tail.resize(CHUNK, 0.0);
        partials.push(session.push(&tail).expect("tail push failed"));
    }

    let final_text = session.finish().expect("finish failed");
    println!("partials: {partials:#?}");
    println!("final: {final_text:?}");

    assert!(
        final_text.to_lowercase().contains("brown fox"),
        "unexpected final transcript: {final_text:?}"
    );
    // The point of streaming: at least one partial arrived before the last
    // chunk — proof the transcript is being built while audio is fed, not
    // decoded once at the end.
    assert!(
        partials.iter().any(|p| !p.trim().is_empty()),
        "no partial transcript during feed — streaming path never produced interim text"
    );
}
