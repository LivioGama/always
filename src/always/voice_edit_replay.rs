//! Safety replay: every real utterance in the user's Always logs is run
//! through the voice-edit grammar + paste-window gate, exactly as the
//! daemon would see it. Proves genuine dictation is never claimed.
//!
//! Corpus: `~/Library/Logs/Always/always.2026-10-1*` (override with
//! `VOICE_EDIT_REPLAY_GLOB_DIR` + `VOICE_EDIT_REPLAY_PREFIX`). Each
//! `whisper_raw` line is an utterance; each `final_result` line is what was
//! pasted. The previous paste (≤ the edit window earlier) is the anchor, so
//! the "same field, right after a paste" gate is simulated with real timing.
//! The test is skipped (passes) when the corpus is absent, e.g. on CI.
//!
//! `cargo test --lib voice_edit_replay -- --nocapture` prints every claim.

#[cfg(test)]
mod tests {
    use crate::always::voice_edit::{Anchor, GateInput, gate_verdict};
    use crate::always::voice_edit_grammar::parse_addressed;

    #[derive(Debug)]
    struct Row {
        ts_ms: i64,
        kind: &'static str,
        text: String,
    }

    fn parse_ts_ms(ts: &str) -> Option<i64> {
        // 2026-10-13T04:22:56.789532Z
        let d = ts.get(0..10)?;
        let t = ts.get(11..23)?;
        let mut dp = d.split('-').map(|x| x.parse::<i64>().ok());
        let (y, mo, da) = (dp.next()??, dp.next()??, dp.next()??);
        let h: i64 = t.get(0..2)?.parse().ok()?;
        let mi: i64 = t.get(3..5)?.parse().ok()?;
        let s: f64 = t.get(6..)?.parse().ok()?;
        // Days since an arbitrary epoch are enough for ordering/deltas
        // within one month window.
        let days = y * 372 + mo * 31 + da;
        Some(((days * 24 + h) * 60 + mi) * 60_000 + (s * 1000.0) as i64)
    }

    fn load_corpus() -> Vec<Row> {
        let dir = std::env::var("VOICE_EDIT_REPLAY_GLOB_DIR").unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| h.join("Library/Logs/Always").to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let prefix =
            std::env::var("VOICE_EDIT_REPLAY_PREFIX").unwrap_or_else(|_| "always.2026-10-1".into());
        let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
        let mut files: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix))
            })
            .collect();
        files.sort();
        let mut rows = Vec::new();
        for f in files {
            let Ok(body) = std::fs::read_to_string(&f) else { continue };
            for line in body.lines() {
                // Format conversion only: JSON log line → (ts, stage, text).
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
                let fields = &v["fields"];
                let (kind, text) = match fields["stage"].as_str() {
                    Some("whisper_raw") => ("utterance", fields["transcript"].as_str()),
                    Some("final_result") => ("paste", fields["text"].as_str()),
                    _ => continue,
                };
                let (Some(text), Some(ts)) = (text, v["timestamp"].as_str()) else { continue };
                let Some(ts_ms) = parse_ts_ms(ts) else { continue };
                rows.push(Row { ts_ms, kind, text: text.to_string() });
            }
        }
        rows.sort_by_key(|r| r.ts_ms);
        rows
    }

    /// Run every utterance through grammar + gate with the previous paste
    /// as the anchor. Returns the claims a manual read did not whitelist.
    fn replay(label: &str, rows: &[Row]) -> Vec<(String, &'static str, Option<String>)> {
        let utterances = rows.iter().filter(|r| r.kind == "utterance").count();
        let mut grammar_hits = Vec::new();
        let mut claims = Vec::new();
        let mut last_paste: Option<&Row> = None;
        for r in rows {
            if r.kind == "paste" {
                last_paste = Some(r);
                continue;
            }
            // What the daemon parses: post local cleanup + wake normalisation.
            let cleaned = crate::always::postprocess::local_cleanup(&r.text);
            let cleaned = crate::always::det_correct::normalize_leading_wake(&cleaned);
            let Some((cmd, addressed)) = parse_addressed(&cleaned) else { continue };
            grammar_hits.push((r.text.clone(), cmd.name()));
            if addressed {
                // Addressed to Iris by name: no paste-window gate.
                claims.push((r.text.clone(), cmd.name(), None));
                continue;
            }
            let anchor = last_paste.map(|p| Anchor::for_replay(&p.text, r.ts_ms - p.ts_ms));
            let verdict = gate_verdict(&GateInput {
                cmd: &cmd,
                anchor: anchor.as_ref(),
                same_app: true,
                same_field: Some(true),
                committed_since_paste: false,
                user_input_since_paste: false,
            });
            if verdict.is_ok() {
                claims.push((r.text.clone(), cmd.name(), last_paste.map(|p| p.text.clone())));
            }
        }
        eprintln!(
            "voice_edit_replay({label}): {} utterances, {} grammar hits, {} gated claims",
            utterances,
            grammar_hits.len(),
            claims.len()
        );
        for (t, n) in &grammar_hits {
            eprintln!("  grammar_hit {n}: {t:?}");
        }
        for (t, n, prev) in &claims {
            eprintln!("  CLAIM {n}: {t:?}  (after paste {prev:?})");
        }
        // Every claim must be a deliberate edit phrase. The corpus predates
        // voice editing, so a claim is either a real (then-unsupported)
        // edit attempt — listed in KNOWN_EDIT_ATTEMPTS after a manual read —
        // or a false positive, which fails the test.
        const KNOWN_EDIT_ATTEMPTS: &[&str] = &[];
        let false_claims: Vec<_> = claims
            .iter()
            .filter(|(t, _, _)| !KNOWN_EDIT_ATTEMPTS.contains(&t.as_str()))
            .collect();
        false_claims.into_iter().cloned().collect()
    }

    /// `~/.always/transcripts.jsonl`: every accepted final over a longer
    /// history (each is both an utterance and the next one's anchor).
    fn load_transcripts() -> Vec<Row> {
        let path = std::env::var("VOICE_EDIT_REPLAY_TRANSCRIPTS").unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| h.join(".always/transcripts.jsonl").to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let Ok(body) = std::fs::read_to_string(path) else { return Vec::new() };
        let mut rows = Vec::new();
        for line in body.lines() {
            // Format conversion only: JSON line → (ts, text).
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let (Some(text), Some(ts)) = (v["text"].as_str(), v["ts"].as_i64()) else { continue };
            rows.push(Row { ts_ms: ts, kind: "utterance", text: text.to_string() });
            rows.push(Row { ts_ms: ts, kind: "paste", text: text.to_string() });
        }
        rows.sort_by_key(|r| r.ts_ms);
        rows
    }

    #[test]
    fn voice_edit_replay_claims_no_genuine_dictation() {
        let rows = load_corpus();
        if rows.is_empty() {
            eprintln!("voice_edit_replay: corpus absent, skipped");
            return;
        }
        let bad = replay("logs", &rows);
        assert!(bad.is_empty(), "genuine dictation claimed: {bad:#?}");
    }

    #[test]
    fn voice_edit_replay_transcripts_history() {
        let rows = load_transcripts();
        if rows.is_empty() {
            eprintln!("voice_edit_replay(transcripts): corpus absent, skipped");
            return;
        }
        let bad = replay("transcripts", &rows);
        assert!(bad.is_empty(), "genuine dictation claimed: {bad:#?}");
    }
}
