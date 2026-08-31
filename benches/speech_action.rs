//! Microbenchmarks for the pure speech-decision and dictation-merge
//! path. These functions are on the hot path of every transcription
//! the daemon emits — `classify_transcription` runs once per
//! utterance, `merge_dictation_with` runs once per resumed paste.
//! Both are pure (no I/O, no globals) which is exactly what makes
//! them benchmarkable in isolation.
//!
//! Run with: `cargo bench --bench speech_action` (or `--quick`).

use std::time::{Duration, Instant};

use criterion::{Criterion, black_box, criterion_group, criterion_main};

use always::always::config::{
    AlwaysConfig, PostprocessConfig, VadMode, VocabConfig,
};
use always::always::localization::Localization;
use always::always::speech_action::{
    classify_transcription, in_cooldown, merge_dictation, merge_dictation_with,
};
use always::stt::TranscriptionResult;
use always::stt_dispatch::TranscriberBackendChoice;

fn bench_config() -> AlwaysConfig {
    AlwaysConfig {
        lang: "en".to_string(),
        timeout_secs: 30,
        silence_secs: 2.0,
        adaptive_silence_enabled: true,
        auto_enter: false,
        filter_enabled: true,
        energy_threshold: 0.05,
        hear_energy_threshold: 0.01,
        onset_ms: 50,
        cooldown_ms: 1500,
        log_path: std::path::PathBuf::from("always.log"),
        post_processor: None,
        project_root: None,
        learning_enabled: false,
        groq_stt_api_key: Some("test-key".to_string()),
        // Pre-existing drift: `AlwaysConfig` grew this field and the bench
        // fixture was never updated, so `cargo bench` did not compile.
        stt_live_preview: true,
        transcriber_backend: TranscriberBackendChoice::Groq,
        vad_mode: VadMode::Local,
        silero_threshold: 0.5,
        vocab_config: VocabConfig::default(),
        postprocess_config: PostprocessConfig::default(),
        auto_enter_delay_ms: 0,
        idle_pause_secs: 0,
        transcript_stream_enabled: false,
        speaker_gate_enabled: false,
        speaker_gate_threshold: 0.5,
        audible_status_sound: Default::default(),
        localization: Localization::ENGLISH,
    }
}

fn empty_transcription(text: &str) -> TranscriptionResult {
    TranscriptionResult {
        text: text.to_string(),
        duration: 1.0,
        language: "en".to_string(),
        segments: vec![],
    }
}

fn bench_in_cooldown(c: &mut Criterion) {
    let now = Instant::now();
    let last = now - Duration::from_millis(500);
    c.bench_function("in_cooldown_inside_window", |b| {
        b.iter(|| in_cooldown(black_box(now), black_box(last), black_box(1500)));
    });

    let outside_last = now - Duration::from_secs(10);
    c.bench_function("in_cooldown_outside_window", |b| {
        b.iter(|| in_cooldown(black_box(now), black_box(outside_last), black_box(1500)));
    });
}

fn bench_classify(c: &mut Criterion) {
    let cfg = bench_config();
    let now = Instant::now();
    let last = now - Duration::from_secs(10);
    let tr = empty_transcription("open the file please");

    c.bench_function("classify_accept_path", |b| {
        b.iter(|| {
            classify_transcription(
                black_box(&cfg),
                black_box("open the file please"),
                black_box(&tr),
                black_box(now),
                black_box(last),
            )
        });
    });

    let in_cd_last = now;
    c.bench_function("classify_cooldown_path", |b| {
        b.iter(|| {
            classify_transcription(
                black_box(&cfg),
                black_box("open the file please"),
                black_box(&tr),
                black_box(now),
                black_box(in_cd_last),
            )
        });
    });

    let filler = empty_transcription("thanks for watching");
    c.bench_function("classify_filter_reject_path", |b| {
        b.iter(|| {
            classify_transcription(
                black_box(&cfg),
                black_box("thanks for watching"),
                black_box(&filler),
                black_box(now),
                black_box(last),
            )
        });
    });
}

fn bench_merge(c: &mut Criterion) {
    c.bench_function("merge_dictation_mid_sentence_lowercase", |b| {
        b.iter(|| {
            merge_dictation(
                black_box("I went to the store"),
                black_box("And bought milk"),
            )
        });
    });

    c.bench_function("merge_dictation_after_sentence_terminator", |b| {
        b.iter(|| merge_dictation(black_box("Done."), black_box("And now we continue")));
    });

    c.bench_function("merge_dictation_proper_noun_preserved", |b| {
        b.iter(|| merge_dictation(black_box("I work on"), black_box("Kubernetes clusters")));
    });

    let loc = Localization::ENGLISH;
    c.bench_function("merge_dictation_with_explicit_locale", |b| {
        b.iter(|| {
            merge_dictation_with(
                black_box(&loc),
                black_box("I went to the store"),
                black_box("And bought milk"),
            )
        });
    });
}

/// Script normalisation sits inside `classify_transcription`, so it is on the
/// critical path between the last audio frame and the paste. The decode it
/// follows costs ~870 ms; the budget here is single-digit milliseconds, and
/// the point of these three cases is to show where the real cost lands:
///
///   - `romanize_latin_only` — every English utterance. One scan, no
///     allocation, `Cow::Borrowed` returned.
///   - `romanize_mixed_utterance` — the reported failure, code-switched.
///   - `romanize_pure_devanagari` — the reported failure again, all
///     Devanagari. Three of its nine words miss the cache, so this is also
///     the measured cost of ONE batched ONNX call.
///   - `romanize_all_cached` — pure Devanagari, every word in the table. This
///     is the cache-only cost, and the number the model must not regress.
///   - `romanize_four_cache_misses` — four out-of-vocabulary words in one
///     utterance. Compared against `romanize_one_cache_miss` it shows that
///     batching makes the model cost per UTTERANCE, not per word.
///   - `romanize_one_cache_miss` — the single-miss case.
fn bench_romanize(c: &mut Criterion) {
    use always::always::translit::{forget_memoized_words, romanize};

    let english = "send the quarterly report to Bob before five and cc the team";
    c.bench_function("romanize_latin_only", |b| {
        b.iter(|| romanize(black_box(english)));
    });

    let mixed = "so the हुन्छ thing is मलाई going to वायो work fine tomorrow";
    c.bench_function("romanize_mixed_utterance", |b| {
        b.iter(|| romanize(black_box(mixed)));
    });

    let devanagari = "अच्छी बात है, एक स्पीकिंग ना चाह इट गोस";
    c.bench_function("romanize_pure_devanagari", |b| {
        b.iter_batched(
            forget_memoized_words,
            |()| romanize(black_box(devanagari)),
            criterion::BatchSize::PerIteration,
        );
    });

    // Every word here is in `dev_roman.tsv`, so no ONNX session is consulted.
    let cached = "हुन्छ मलाई धेरै राम्रो काम गर्न भयो छैन पर्छ";
    c.bench_function("romanize_all_cached", |b| {
        b.iter(|| romanize(black_box(cached)));
    });

    // COLD: the memo is cleared before every sample, so this is the cost of
    // meeting a word for the first time. Warm, the same call is a hash lookup.
    let one_miss = "हुन्छ मलाई धेरै राम्रो ट्रान्सलिटरेसन गर्न भयो छैन पर्छ";
    c.bench_function("romanize_one_cache_miss_cold", |b| {
        b.iter_batched(
            forget_memoized_words,
            |()| romanize(black_box(one_miss)),
            criterion::BatchSize::PerIteration,
        );
    });

    c.bench_function("romanize_one_cache_miss_memoized", |b| {
        romanize(one_miss);
        b.iter(|| romanize(black_box(one_miss)));
    });

    let four_misses =
        "हुन्छ ट्रान्सलिटरेसन धेरै माइक्रोसफ्टको सङ्ग्रहालयहरूमा गर्न अन्तर्राष्ट्रियकरण छैन पर्छ";
    c.bench_function("romanize_four_cache_misses_cold", |b| {
        b.iter_batched(
            forget_memoized_words,
            |()| romanize(black_box(four_misses)),
            criterion::BatchSize::PerIteration,
        );
    });
}

criterion_group!(
    benches,
    bench_in_cooldown,
    bench_classify,
    bench_merge,
    bench_romanize
);
criterion_main!(benches);
