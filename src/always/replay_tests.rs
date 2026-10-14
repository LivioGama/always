//! Offline replay of recorded / generated speech through the REAL capture
//! pipeline (`vad::record_utterance`: VAD, My Voice gate, Apple streaming
//! final, Parakeet live preview, voice level) — no microphone, no app.
//!
//! The audio source is a fake capture helper (`ALWAYS_CAPTURE_BIN`) that
//! streams WAVs in real time; see the scratch `replay.py`. Run with:
//!
//! ```text
//! ALWAYS_CAPTURE_BIN=…/capture.sh REPLAY_WAVS=a.wav,b.wav REPLAY_LOG=onsets.jsonl \
//! REPLAY_OUT=report.json REPLAY_GATE=1 \
//!   cargo test --features local-stt --lib replay_pipeline -- --ignored --nocapture
//! ```
//!
//! The report holds every broadcast event with its wall time plus each
//! `RecordResult`; latency/WER are computed from it outside the test.

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::always::config::AlwaysConfig;
    use crate::always::event::{DaemonEvent, global_broadcaster};
    use crate::always::log::Logger;
    use crate::always::vad::{self, RecordResult};
    use crate::stt::Transcriber;

    fn wall_ms() -> f64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64() * 1000.0
    }

    #[test]
    #[ignore = "needs REPLAY_* env + a fake capture helper; run explicitly"]
    fn replay_pipeline() {
        let Ok(out) = std::env::var("REPLAY_OUT") else {
            eprintln!("REPLAY_OUT not set; skipping");
            return;
        };
        let n_files = std::env::var("REPLAY_WAVS").unwrap_or_default().split(',').filter(|s| !s.is_empty()).count();
        let _ = tracing_subscriber::fmt()
            .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
            .with_writer(std::io::stderr)
            .try_init();

        let mut cfg = AlwaysConfig::default();
        cfg.transcriber_backend = crate::stt_dispatch::TranscriberBackendChoice::Apple;
        cfg.lang = std::env::var("REPLAY_LANG").unwrap_or_else(|_| "en".into());
        cfg.speaker_gate_enabled = std::env::var("REPLAY_GATE").as_deref() == Ok("1");
        cfg.silence_secs = std::env::var("REPLAY_SILENCE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
        cfg.timeout_secs = 4;


        // Warm the preview helper exactly as the daemon does at boot.
        crate::always::preview_asr::prewarm(&cfg.lang, true);
        let t_warm = std::time::Instant::now();
        while crate::always::preview_asr::global().is_some_and(|a| !a.is_ready())
            && t_warm.elapsed().as_secs() < 120
        {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let events: Arc<parking_lot::Mutex<Vec<serde_json::Value>>> = Arc::default();
        let mut rx = global_broadcaster().subscribe();
        let sink = Arc::clone(&events);
        std::thread::spawn(move || {
            while let Ok(ev) = rx.blocking_recv() {
                let t = wall_ms();
                let mut v = serde_json::to_value(&ev).unwrap_or_default();
                if matches!(ev, DaemonEvent::VoiceLevel { .. }) {
                    // keep levels compact
                    v["data"]["bands"] = serde_json::Value::Null;
                }
                v["t"] = serde_json::json!(t);
                sink.lock().push(v);
            }
        });

        let transcriber: Arc<dyn Transcriber> = Arc::new(
            crate::always::stt_apple::AppleTranscriber::new(Some(cfg.lang.clone()))
                .expect("Apple STT available"),
        );
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut log = Logger;
        let mut results = Vec::new();
        let mut idle = 0;
        while results.len() < n_files * 3 && idle < 2 {
            let started = wall_ms();
            let r = vad::record_utterance(&cfg, &mut log, &transcriber, rt.handle());
            let ended = wall_ms();
            let (kind, text, score) = match &r {
                Ok(RecordResult::Speech { text, .. }) => ("speech", text.clone(), None),
                Ok(RecordResult::DroppedSpeaker { score }) => ("dropped_speaker", String::new(), Some(*score)),
                Ok(RecordResult::Silence) => ("silence", String::new(), None),
                Ok(RecordResult::Timeout) => ("timeout", String::new(), None),
                Ok(RecordResult::DroppedLowEnergy { .. }) => ("low_energy", String::new(), None),
                Ok(RecordResult::DroppedNoise { raw }) => ("noise", raw.clone(), None),
                Ok(RecordResult::PreemptedByMicConflict { text }) => ("preempted", text.clone(), None),
                Err(e) => ("error", format!("{e:#}"), None),
            };
            eprintln!("replay result: {kind} {text:?} score={score:?}");
            if matches!(kind, "silence" | "timeout" | "error") {
                idle += 1;
            } else {
                idle = 0;
            }
            if kind == "error" {
                // recorder ended (helper exited) — done
                break;
            }
            results.push(serde_json::json!({
                "kind": kind, "text": text, "score": score, "started": started, "ended": ended,
            }));
            if kind == "dropped_speaker"
                && let Some(stash) = crate::always::rejected::take_stashed()
            {
                eprintln!("  (rejected audio stashed: {:.2}s)", stash.samples.len() as f64 / 16_000.0);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        let report = serde_json::json!({ "results": results, "events": *events.lock() });
        std::fs::write(&out, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        eprintln!("report written to {out}");
    }
}
