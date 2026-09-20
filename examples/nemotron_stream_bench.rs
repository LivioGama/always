//! Latency + accuracy benchmark for the Nemotron cache-aware streaming path.
//!
//! Answers three questions with real numbers on the real model:
//!   1. What does a from-scratch one-shot decode of a whole utterance cost?
//!   2. What does a PERSISTENT streaming session cost per 560 ms chunk, and
//!      does that cost grow with buffer length (parakeet-rs recomputes the mel
//!      spectrogram over the ENTIRE buffer on every `transcribe_chunk`)?
//!   3. Is the accumulated streaming transcript equivalent to the one-shot one?
//!
//! Usage:
//!   cargo run --release --features local-stt --example nemotron_stream_bench -- <wav> [<wav>...]
#[cfg(feature = "local-stt")]
fn main() -> anyhow::Result<()> {
    use parakeet_rs::{Nemotron, NemotronHandle};
    use std::time::Instant;

    const CHUNK: usize = 8_960; // 560 ms @ 16 kHz — parakeet-rs CHUNK_SIZE * HOP_LENGTH

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: nemotron_stream_bench <wav> [<wav>...]");
        std::process::exit(2);
    }

    if args[0] == "--e2e" {
        return e2e::run(&args[1..]);
    }

    let model_dir = dirs::data_dir()
        .unwrap()
        .join("always/models/nemotron-3.5-asr-streaming-0.6b");
    let t0 = Instant::now();
    let handle =
        NemotronHandle::load(&model_dir, None).map_err(|e| anyhow::anyhow!("load: {e}"))?;
    println!("model loaded in {} ms", t0.elapsed().as_millis());

    // Warm: compile the ONNX graph so measurement 1 isn't a cold start.
    {
        let mut warm = Nemotron::from_shared(&handle);
        let _ = warm.transcribe_audio(&vec![0.0f32; 8_000]);
    }

    for path in &args {
        let samples = read_wav_f32(path)?;
        let secs = samples.len() as f64 / 16_000.0;
        println!("\n=== {path}  ({secs:.1}s, {} samples) ===", samples.len());

        // ---- A. one-shot, from scratch (what finalization does today) ----
        let mut oneshot_ms = Vec::new();
        let mut oneshot_text = String::new();
        for _ in 0..3 {
            let mut n = Nemotron::from_shared(&handle);
            let t = Instant::now();
            oneshot_text = n
                .transcribe_audio(&samples)
                .map_err(|e| anyhow::anyhow!("oneshot: {e}"))?;
            oneshot_ms.push(t.elapsed().as_millis() as u64);
        }
        println!("A one-shot transcribe_audio: {oneshot_ms:?} ms");

        // ---- B. persistent streaming session, fed 560 ms at a time ----
        let mut n = Nemotron::from_shared(&handle);
        let mut per_chunk = Vec::new();
        let mut incremental = String::new();
        let t_all = Instant::now();
        let nchunks = samples.len().div_ceil(CHUNK);
        for i in 0..nchunks {
            let start = i * CHUNK;
            let end = (start + CHUNK).min(samples.len());
            let mut c = samples[start..end].to_vec();
            c.resize(CHUNK, 0.0);
            let t = Instant::now();
            let piece = n
                .transcribe_chunk(&c)
                .map_err(|e| anyhow::anyhow!("chunk {i}: {e}"))?;
            per_chunk.push(t.elapsed().as_millis() as u64);
            if !piece.is_empty() {
                if !incremental.is_empty() {
                    incremental.push(' ');
                }
                incremental.push_str(piece.trim());
            }
        }
        // Flush: one extra silent chunk drains the decoder for the tail.
        let t_flush = Instant::now();
        let tail = n
            .transcribe_chunk(&vec![0.0f32; CHUNK])
            .map_err(|e| anyhow::anyhow!("flush: {e}"))?;
        let flush_ms = t_flush.elapsed().as_millis() as u64;
        if !tail.trim().is_empty() {
            incremental.push(' ');
            incremental.push_str(tail.trim());
        }
        let stream_total = t_all.elapsed().as_millis() as u64;
        let acc = n.get_transcript();

        let first5: Vec<u64> = per_chunk.iter().take(5).copied().collect();
        let last5: Vec<u64> = per_chunk.iter().rev().take(5).rev().copied().collect();
        let sum: u64 = per_chunk.iter().sum();
        println!(
            "B streaming: {nchunks} chunks, total {stream_total} ms (sum {sum} ms), flush {flush_ms} ms"
        );
        println!("   per-chunk first5={first5:?}  last5={last5:?}");
        println!(
            "   realtime factor: {:.2}x  (chunk cost / 560 ms budget, mean of last 5 = {:.0} ms)",
            last5.iter().sum::<u64>() as f64 / 5.0 / 560.0,
            last5.iter().sum::<u64>() as f64 / 5.0
        );
        println!("   TAIL LATENCY IF STREAM KEPT UP = flush only = {flush_ms} ms");

        println!("\n--- one-shot text ---\n{oneshot_text}");
        println!("\n--- streaming get_transcript() ---\n{acc}");
        println!("\n--- streaming concatenated pieces ---\n{incremental}");
        println!(
            "\nidentical(one-shot, get_transcript) = {}",
            norm(&oneshot_text) == norm(&acc)
        );
        println!(
            "wer(get_transcript vs one-shot) = {:.3}",
            wer(&norm(&oneshot_text), &norm(&acc))
        );
    }
    Ok(())
}

#[cfg(feature = "local-stt")]
pub(crate) fn norm(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(feature = "local-stt")]
pub(crate) fn wer(a: &str, b: &str) -> f64 {
    let a: Vec<&str> = a.split_whitespace().collect();
    let b: Vec<&str> = b.split_whitespace().collect();
    if a.is_empty() {
        return if b.is_empty() { 0.0 } else { 1.0 };
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()] as f64 / a.len() as f64
}

#[cfg(feature = "local-stt")]
pub(crate) fn read_wav_f32(path: &str) -> anyhow::Result<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    // Minimal RIFF parse: find "data" chunk, assume 16-bit mono 16 kHz.
    let pos = bytes
        .windows(4)
        .position(|w| w == b"data")
        .ok_or_else(|| anyhow::anyhow!("no data chunk in {path}"))?;
    let start = pos + 8;
    Ok(bytes[start..]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect())
}

#[cfg(not(feature = "local-stt"))]
fn main() {
    eprintln!("build with --features local-stt");
}

// ---------------------------------------------------------------------------
// End-to-end check of the PRODUCTION path: LocalTranscriber::open_live_stream
// -> always::always::live_stream::LiveStream worker -> finish(), fed in the
// same 30 ms-frame pattern the VAD capture loop uses, versus the one-shot
// `Transcriber::transcribe_from_bytes` it replaces.
//
// Run with: --example nemotron_stream_bench -- --e2e <wav>...
#[cfg(feature = "local-stt")]
mod e2e {
    use always::always::live_stream::{FINISH_TIMEOUT, LiveStream};
    use always::always::stt_local::LocalTranscriber;
    use always::managers::model_registry::EngineType;
    use always::stt::Transcriber;
    use std::sync::Arc;
    use std::time::Instant;

    pub fn run(paths: &[String]) -> anyhow::Result<()> {
        let dir = dirs::data_dir()
            .unwrap()
            .join("always/models/nemotron-3.5-asr-streaming-0.6b");
        let t: Arc<dyn Transcriber> = Arc::new(LocalTranscriber::load(
            EngineType::Nemotron,
            &dir,
            Some("en".into()),
            true,
        )?);
        println!("supports_streaming = {}", t.supports_streaming());
        assert!(
            t.open_live_stream().is_some(),
            "production transcriber must offer a live session"
        );

        for path in paths {
            let samples = super::read_wav_f32(path)?;
            let pcm: Vec<i16> = samples.iter().map(|s| (s * 32768.0) as i16).collect();
            let secs = pcm.len() as f64 / 16_000.0;
            println!("\n=== E2E {path} ({secs:.1}s) ===");

            // One-shot, exactly as vad.rs's fallback does it.
            let wav = always::always::audio::create_wav_bytes_i16_mono_16k(&pcm)?;
            let t0 = Instant::now();
            let oneshot = t
                .transcribe_from_bytes(wav)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let oneshot_ms = t0.elapsed().as_millis() as u64;

            // Live session, fed 480-sample frames as the capture loop does,
            // in real time so the worker gets the same head start it does live.
            let mut live = LiveStream::start(&t).expect("live session");
            let mut buf: Vec<i16> = Vec::with_capacity(pcm.len());
            let start = Instant::now();
            for frame in pcm.chunks(480) {
                buf.extend_from_slice(frame);
                live.feed(&buf);
                let due = std::time::Duration::from_secs_f64(buf.len() as f64 / 16_000.0);
                if let Some(sleep) = due.checked_sub(start.elapsed()) {
                    std::thread::sleep(sleep);
                }
            }
            // Speech ends here — this is `speech_end_at`.
            let speech_end = Instant::now();
            let text = live.finish(&buf, FINISH_TIMEOUT);
            let finish_ms = speech_end.elapsed().as_millis() as u64;

            println!(
                "  one-shot   : {oneshot_ms:>6} ms  ({})",
                oneshot.text.trim()
            );
            println!(
                "  live finish: {finish_ms:>6} ms  ({})",
                text.as_deref().unwrap_or("<none>").trim()
            );
            let a = super::norm(&oneshot.text);
            let b = super::norm(text.as_deref().unwrap_or(""));
            println!(
                "  wer = {:.3}   speedup = {:.1}x",
                super::wer(&a, &b),
                oneshot_ms as f64 / finish_ms.max(1) as f64
            );
        }
        Ok(())
    }
}
