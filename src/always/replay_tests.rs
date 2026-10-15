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
#[allow(clippy::field_reassign_with_default)] // env-driven replay knobs
mod tests {
    use std::io::{BufRead, Write};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::always::config::AlwaysConfig;
    use crate::always::event::{DaemonEvent, global_broadcaster};
    use crate::always::log::Logger;
    use crate::always::vad::{self, RecordResult};
    use crate::stt::Transcriber;

    /// Whole-file transcription through the Parakeet preview helper
    /// (`B`/`A`/`E` frames, `final` JSON line). Replay-only.
    struct ParakeetFileTranscriber {
        io: parking_lot::Mutex<(
            std::process::ChildStdin,
            std::io::BufReader<std::process::ChildStdout>,
            u32,
        )>,
    }

    impl ParakeetFileTranscriber {
        fn spawn() -> Self {
            let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("preview-asr/.build/release/always-preview-asr");
            let mut child = std::process::Command::new(bin)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn preview helper");
            let stdin = child.stdin.take().unwrap();
            let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
            let mut line = String::new();
            while out.read_line(&mut line).unwrap() > 0 {
                if line.contains("\"ready\"") {
                    break;
                }
                line.clear();
            }
            std::mem::forget(child);
            Self {
                io: parking_lot::Mutex::new((stdin, out, 0)),
            }
        }
    }

    fn frame(op: u8, seq: u32, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![op];
        f.extend_from_slice(&seq.to_le_bytes());
        f.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        f.extend_from_slice(payload);
        f
    }

    impl Transcriber for ParakeetFileTranscriber {
        fn transcribe_from_bytes(
            &self,
            audio: Vec<u8>,
        ) -> Result<crate::stt::TranscriptionResult, crate::stt::SttError> {
            let pcm = &audio[44.min(audio.len())..];
            let mut io = self.io.lock();
            io.2 += 1;
            let seq = io.2;
            if let Ok(dir) = std::env::var("REPLAY_DUMP_DIR") {
                let _ = std::fs::write(format!("{dir}/call-{seq:03}.wav"), &audio);
            }
            io.0.write_all(&frame(b'B', seq, &[])).unwrap();
            for part in pcm.chunks(16_000 * 2) {
                io.0.write_all(&frame(b'A', seq, part)).unwrap();
            }
            io.0.write_all(&frame(b'E', seq, &[])).unwrap();
            io.0.flush().unwrap();
            let mut line = String::new();
            loop {
                line.clear();
                if io.1.read_line(&mut line).unwrap() == 0 {
                    panic!("preview helper exited");
                }
                let v: serde_json::Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if v["ev"] == "final" && v["seq"] == seq {
                    let text = v["text"].as_str().unwrap_or_default().to_string();
                    eprintln!(
                        "parakeet_file_final secs={:.2} text={text:?}",
                        pcm.len() as f64 / 32_000.0
                    );
                    return Ok(crate::stt::TranscriptionResult {
                        text,
                        ..Default::default()
                    });
                }
            }
        }

        fn transcribe_streaming(
            &self,
            audio: Vec<u8>,
        ) -> std::pin::Pin<
            Box<
                dyn futures::Stream<
                        Item = Result<
                            crate::stt::StreamingTranscriptionResult,
                            crate::stt::SttError,
                        >,
                    > + Send,
            >,
        > {
            let r = self.transcribe_from_bytes(audio).map(|r| {
                crate::stt::StreamingTranscriptionResult {
                    text: r.text,
                    is_final: true,
                    is_interim: false,
                }
            });
            Box::pin(futures::stream::once(async move { r }))
        }
    }

    fn wall_ms() -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
            * 1000.0
    }

    /// A/B of the vocabulary bias on the real Swift file recognizer:
    /// each WAV in `BIAS_WAVS` is transcribed with the production
    /// `context_phrases()` and with the same list minus the "Iris"
    /// phrases, then passed through `normalize_leading_wake`.
    /// `BIAS_WAVS=a.wav,b.wav cargo test --lib bias_ab -- --ignored --nocapture`
    #[test]
    #[ignore = "needs BIAS_WAVS + Apple STT; run explicitly"]
    fn bias_ab() {
        let Ok(wavs) = std::env::var("BIAS_WAVS") else {
            return;
        };
        let with = crate::always::stt_apple::context_phrases();
        let without: Vec<String> = with
            .iter()
            .filter(|p| !p.contains("Iris"))
            .cloned()
            .collect();
        assert!(
            with.iter().any(|p| p == "Iris"),
            "Iris missing from context_phrases"
        );
        for wav in wavs.split(',').filter(|s| !s.is_empty()) {
            let path = std::path::Path::new(wav);
            let a = crate::always::apple_stt::transcribe_wav(path, Some("en"), &without);
            let b = crate::always::apple_stt::transcribe_wav(path, Some("en"), &with);
            let norm = b
                .as_deref()
                .map(crate::always::det_correct::normalize_leading_wake);
            eprintln!(
                "bias_ab {wav}\n  without Iris: {a:?}\n  with Iris:    {b:?}\n  normalised:   {norm:?}"
            );
        }
    }

    #[test]
    #[ignore = "needs REPLAY_* env + a fake capture helper; run explicitly"]
    fn replay_pipeline() {
        let Ok(out) = std::env::var("REPLAY_OUT") else {
            eprintln!("REPLAY_OUT not set; skipping");
            return;
        };
        let n_files = std::env::var("REPLAY_WAVS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .count();
        let _ = tracing_subscriber::fmt()
            .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
            .with_writer(std::io::stderr)
            .try_init();

        let mut cfg = AlwaysConfig::default();
        cfg.transcriber_backend = crate::stt_dispatch::TranscriberBackendChoice::Apple;
        cfg.lang = std::env::var("REPLAY_LANG").unwrap_or_else(|_| "en".into());
        cfg.speaker_gate_enabled = std::env::var("REPLAY_GATE").as_deref() == Ok("1");
        cfg.silence_secs = std::env::var("REPLAY_SILENCE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0);
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

        // `REPLAY_STT=parakeet`: finals from the Parakeet helper instead of
        // Apple STT, for hosts where this test binary has no Speech
        // Recognition grant (TCC aborts it). No Apple live session runs
        // then, so every chunk / tail text comes from the file path.
        let transcriber: Arc<dyn Transcriber> =
            if std::env::var("REPLAY_STT").as_deref() == Ok("parakeet") {
                cfg.transcriber_backend = crate::stt_dispatch::TranscriberBackendChoice::Groq;
                Arc::new(ParakeetFileTranscriber::spawn())
            } else {
                Arc::new(
                    crate::always::stt_apple::AppleTranscriber::new(Some(cfg.lang.clone()))
                        .expect("Apple STT available"),
                )
            };
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
                Ok(RecordResult::DroppedSpeaker { score }) => {
                    ("dropped_speaker", String::new(), Some(*score))
                }
                Ok(RecordResult::Silence) => ("silence", String::new(), None),
                Ok(RecordResult::Timeout) => ("timeout", String::new(), None),
                Ok(RecordResult::DroppedLowEnergy { .. }) => ("low_energy", String::new(), None),
                Ok(RecordResult::DroppedNoise { raw }) => ("noise", raw.clone(), None),
                Ok(RecordResult::PreemptedByMicConflict { text }) => {
                    ("preempted", text.clone(), None)
                }
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
                eprintln!(
                    "  (rejected audio stashed: {:.2}s)",
                    stash.samples.len() as f64 / 16_000.0
                );
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        let report = serde_json::json!({ "results": results, "events": *events.lock() });
        std::fs::write(&out, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        eprintln!("report written to {out}");
    }

    /// Fake capture helper: streams one WAV (16 kHz mono s16) to stdout in
    /// real time after 1.5 s of room tone, touches `<wav>.done` once the
    /// last speech sample is out, then streams room tone until the pipe
    /// closes. Pacing / format only.
    const PACER_PY: &str = r#"
import os, sys, time, wave, struct, random
out = sys.stdout.buffer
wav = sys.argv[1]
FRAME = 480
t0 = time.time(); sent = 0
def emit(pcm):
    global sent
    for i in range(0, len(pcm), FRAME * 2):
        d = t0 + (sent + FRAME) / 16000.0 - time.time()
        if d > 0: time.sleep(d)
        out.write(pcm[i:i + FRAME * 2]); out.flush()
        sent += len(pcm[i:i + FRAME * 2]) // 2
def quiet(secs):
    emit(b"".join(struct.pack("<h", random.randint(-30, 30)) for _ in range(int(secs * 16000))))
quiet(1.5)
wf = wave.open(wav)
emit(wf.readframes(wf.getnframes()))
open(wav + ".done", "w").close()
while True:
    quiet(1.0)
"#;

    /// Stand-in for Apple STT: committed chunks transcribe fine; every call
    /// that starts after the speech is over (the tail's speculation and its
    /// fresh pass) fails the way Apple did at 2026-10-14T17:48:45Z.
    struct TailTimesOut {
        speech_done: std::path::PathBuf,
        /// How long a failing call blocks first (Apple's timeout is 12 s).
        fail_after: std::time::Duration,
        ok_texts: parking_lot::Mutex<Vec<String>>,
        failed: std::sync::atomic::AtomicUsize,
    }

    impl Transcriber for TailTimesOut {
        fn transcribe_from_bytes(
            &self,
            audio: Vec<u8>,
        ) -> Result<crate::stt::TranscriptionResult, crate::stt::SttError> {
            let secs = audio.len().saturating_sub(44) as f64 / 32_000.0;
            if self.speech_done.exists() {
                self.failed
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(self.fail_after);
                eprintln!(
                    "mock_stt: FAIL (tail) secs={secs:.2} after {:?}",
                    self.fail_after
                );
                return Err(crate::stt::SttError::Other(anyhow::anyhow!(
                    "Apple STT failed: speech recognition timed out"
                )));
            }
            let mut texts = self.ok_texts.lock();
            let text = format!("committed chunk number {}", texts.len() + 1);
            eprintln!("mock_stt: ok secs={secs:.2} -> {text:?}");
            texts.push(text.clone());
            Ok(crate::stt::TranscriptionResult {
                text,
                ..Default::default()
            })
        }

        fn transcribe_streaming(
            &self,
            audio: Vec<u8>,
        ) -> std::pin::Pin<
            Box<
                dyn futures::Stream<
                        Item = Result<
                            crate::stt::StreamingTranscriptionResult,
                            crate::stt::SttError,
                        >,
                    > + Send,
            >,
        > {
            let r = self.transcribe_from_bytes(audio).map(|r| {
                crate::stt::StreamingTranscriptionResult {
                    text: r.text,
                    is_final: true,
                    is_interim: false,
                }
            });
            Box::pin(futures::stream::once(async move { r }))
        }
    }

    /// 2026-10-14T17:48:45Z: a 3-chunk dictation (189 + 196 + 95 chars
    /// already transcribed) was lost whole because Apple STT timed out on
    /// the ~1 s tail — the tail error returned before `finalize_chunked`.
    /// Real capture pipeline (VAD, hard-flush chunking, speculation, tail
    /// transcription) on a 15.5 s TTS sentence through a fake capture
    /// helper; only the recognizer is scripted. Needs `say` + python3:
    /// `cargo test --features local-stt --lib last_chunk_stt_timeout -- --ignored --nocapture`
    ///
    /// `LASTCHUNK_RESCUE=1` runs the production configuration instead
    /// (Apple backend, English, warm Parakeet preview helper; the failing
    /// tail call blocks 12 s like Apple's timeout) and expects Parakeet's
    /// words for the tail. The Apple live session needs the Speech grant:
    /// run it from a `com.always.v3`-signed harness bundle.
    #[test]
    #[ignore = "real-time capture replay (~25 s); needs `say` + python3; run explicitly"]
    fn last_chunk_stt_timeout_keeps_committed_chunks() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("info")
            .with_writer(std::io::stderr)
            .try_init();
        let dir = std::env::temp_dir().join(format!("always-lastchunk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("speech.wav");
        let said = std::process::Command::new("/usr/bin/say")
            .args(["-o", wav.to_str().unwrap(), "--data-format=LEI16@16000"])
            .arg(
                "So the plan for this afternoon is to walk through the whole release checklist \
                 with the team and make sure every single item on it has an owner and a date and \
                 a clear way to check it before we ship the new build to everyone who has been \
                 waiting on it because the last one had that nasty crash on startup",
            )
            .status()
            .unwrap();
        assert!(said.success());
        let pacer = dir.join("pacer.py");
        std::fs::write(&pacer, PACER_PY).unwrap();
        let capture = dir.join("capture.sh");
        std::fs::write(
            &capture,
            format!(
                "#!/bin/sh\nexec /usr/bin/python3 -I '{}' '{}'\n",
                pacer.display(),
                wav.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &capture,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        // SAFETY: set before the recorder is spawned; this test is the only
        // reader and is run on its own (`--ignored`, by name).
        let rescue = std::env::var("LASTCHUNK_RESCUE").as_deref() == Ok("1");
        unsafe {
            std::env::set_var("ALWAYS_CAPTURE_BIN", &capture);
            if !rescue {
                std::env::set_var("ALWAYS_PREVIEW_ASR", "0");
            }
        }

        let mut cfg = AlwaysConfig::default();
        cfg.transcriber_backend = if rescue {
            crate::stt_dispatch::TranscriberBackendChoice::Apple
        } else {
            crate::stt_dispatch::TranscriberBackendChoice::Groq
        };
        if rescue {
            crate::always::preview_asr::prewarm("en", true);
            let t = std::time::Instant::now();
            while crate::always::preview_asr::global().is_some_and(|a| !a.is_ready())
                && t.elapsed().as_secs() < 300
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            assert!(
                crate::always::preview_asr::global().is_some_and(|a| a.is_ready()),
                "helper warm"
            );
        }
        cfg.lang = "en".into();
        cfg.speaker_gate_enabled = false;
        // No periodic preview passes through the scripted recognizer.
        cfg.stt_live_preview = false;
        cfg.silence_secs = 1.0;
        cfg.timeout_secs = 4;

        let mock = Arc::new(TailTimesOut {
            speech_done: std::path::PathBuf::from(format!("{}.done", wav.display())),
            fail_after: std::time::Duration::from_secs(if rescue { 12 } else { 0 }),
            ok_texts: Default::default(),
            failed: Default::default(),
        });
        let transcriber: Arc<dyn Transcriber> = mock.clone();
        let spill_dir = std::env::temp_dir().join("always-test-failed-chunks");
        let spilled_before = std::fs::read_dir(&spill_dir)
            .map(|d| d.count())
            .unwrap_or(0);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut log = Logger;
        let mut outcome = None;
        for _ in 0..4 {
            let r = vad::record_utterance(&cfg, &mut log, &transcriber, rt.handle());
            match &r {
                Ok(RecordResult::Silence) | Ok(RecordResult::Timeout) => continue,
                _ => {
                    outcome = Some(r);
                    break;
                }
            }
        }
        let outcome = outcome.expect("an utterance was recorded");
        let since_speech_end = std::fs::metadata(&mock.speech_done)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok());
        eprintln!("result {since_speech_end:?} after the last speech sample");
        let committed = mock.ok_texts.lock().clone();
        let failed = mock.failed.load(std::sync::atomic::Ordering::SeqCst);
        eprintln!("committed={committed:?} failed_calls={failed}");
        assert!(
            !committed.is_empty(),
            "the dictation must have been chunked"
        );
        assert!(
            failed >= 1,
            "the tail transcription must have been attempted and failed"
        );

        let text = match outcome {
            Ok(RecordResult::Speech { text, .. }) => text,
            other => panic!(
                "committed chunks {committed:?} were lost: {:?}",
                other
                    .map(|_| "non-speech result")
                    .map_err(|e| format!("{e:#}"))
            ),
        };
        eprintln!("pasted: {text:?}");
        assert!(
            text.starts_with("committed chunk number"),
            "committed chunk text missing: {text:?}"
        );
        if rescue {
            // The tail is the end of the sentence ("…nasty crash on startup").
            let lower = text.to_lowercase();
            assert!(
                !text.contains("[audio saved"),
                "Parakeet rescue not used: {text:?}"
            );
            assert!(
                lower.contains("crash") || lower.contains("startup"),
                "tail words missing: {text:?}"
            );
        } else {
            assert!(
                text.ends_with("]") && text.contains("[audio saved: chunk "),
                "tail placeholder missing: {text:?}"
            );
        }
        let spilled_after = std::fs::read_dir(&spill_dir)
            .map(|d| d.count())
            .unwrap_or(0);
        assert!(
            spilled_after > spilled_before,
            "tail audio was not saved for recovery"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
