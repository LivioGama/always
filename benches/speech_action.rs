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
    AlwaysConfig, IdlePauseAction, PostprocessConfig, VadMode, VocabConfig,
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
        transcriber_backend: TranscriberBackendChoice::Groq,
        vad_mode: VadMode::Local,
        silero_threshold: 0.5,
        vocab_config: VocabConfig::default(),
        postprocess_config: PostprocessConfig::default(),
        auto_enter_delay_ms: 0,
        idle_pause_secs: 0,
        idle_pause_action: IdlePauseAction::default(),
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

criterion_group!(benches, bench_in_cooldown, bench_classify, bench_merge);
criterion_main!(benches);
