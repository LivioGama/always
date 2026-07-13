//! Silero VAD benchmark — same wrapper the daemon uses in production.
//!
//! Run with: `cargo bench --bench vad` (or `--quick` for a fast smoke test).

use always::always::vad_silero::SileroVad;
use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn bench_silero_predict(c: &mut Criterion) {
    let vad = SileroVad::new().expect("failed to load Silero VAD");

    // Pure silence — represents the no-speech codepath.
    let silence: Vec<i16> = vec![0i16; 480];

    c.bench_function("silero_predict_480_samples", |b| {
        b.iter(|| {
            let _ = vad.predict(black_box(&silence));
        });
    });

    // Synthetic "speech-ish" sine wave — represents the speech-detected path
    // through the model. ~1 kHz tone at moderate amplitude.
    let speech: Vec<i16> = (0..480)
        .map(|i| ((i as f32 / 16.0).sin() * 8000.0) as i16)
        .collect();

    c.bench_function("silero_predict_speech_signal", |b| {
        b.iter(|| {
            let _ = vad.predict(black_box(&speech));
        });
    });
}

criterion_group!(benches, bench_silero_predict);
criterion_main!(benches);
