//! Real Silero VAD benchmark using the same `voice_activity_detector` crate
//! and chunk size that the production daemon (`src/always/vad.rs`) uses.
//!
//! Run with: `cargo bench --bench vad` (or `--quick` for a fast smoke test).

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use voice_activity_detector::VoiceActivityDetector;

fn bench_silero_predict(c: &mut Criterion) {
    let mut vad = VoiceActivityDetector::builder()
        .sample_rate(16000_i64)
        .chunk_size(512_usize)
        .build()
        .expect("failed to build VAD");

    // Pure silence — represents the no-speech codepath.
    let silence: Vec<i16> = vec![0i16; 512];

    c.bench_function("silero_predict_512_samples", |b| {
        b.iter(|| {
            // Cloning the Vec is required because predict() consumes it.
            // The clone is part of the per-call cost the daemon also pays.
            let _ = vad.predict(black_box(silence.clone()));
        });
    });

    // Synthetic "speech-ish" sine wave — represents the speech-detected path
    // through the model. ~1 kHz tone at moderate amplitude.
    let speech: Vec<i16> = (0..512)
        .map(|i| ((i as f32 / 16.0).sin() * 8000.0) as i16)
        .collect();

    c.bench_function("silero_predict_speech_signal", |b| {
        b.iter(|| {
            let _ = vad.predict(black_box(speech.clone()));
        });
    });
}

criterion_group!(benches, bench_silero_predict);
criterion_main!(benches);
