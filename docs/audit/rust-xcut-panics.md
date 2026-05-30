# Cross-Cutting Panic Audit — scope: xcut-panics

Release profile uses `panic = "abort"` (verified `Cargo.toml:109`), so any panic
reachable in the running daemon is an immediate, unrecoverable process abort —
the user's always-on voice-to-text silently dies until relaunched.

## Method

Grepped all of `src/` for `.unwrap()`, `.expect(`, `panic!`, `unreachable!`, slice
`[i]` indexing, and integer arithmetic / casts in the hot and network-facing
paths. 82 total `unwrap`/`expect`/`panic`/`unreachable` matches. I READ each
production (non-`#[cfg(test)]`) site in full and traced its inputs to decide
whether it is fallible at RUNTIME in the daemon. Result: the codebase is unusually
disciplined here — essentially every match is either inside a `#[cfg(test)]` /
`mod tests` block, uses a non-panicking variant
(`unwrap_or`/`unwrap_or_else`/`unwrap_or_default`), or is impossible-by-construction.
Every slice index in the per-utterance text/audio hot paths and the untrusted-UDS
parser is explicitly bounds-guarded.

## Findings

### [LOW] HTTP client builder `.expect()` aborts instead of degrading
- file: src/http_client.rs:26 and src/http_client.rs:42
- category: error-handling
- confidence: high
- impact: If `reqwest::{blocking,}::Client::builder().build()` ever fails (TLS backend init), the daemon aborts on first network use rather than returning an error.
- problem: Production code inside `OnceLock::get_or_init` closures:
  ```rust
  .build()
  .expect("failed to build HTTP client")        // line 26
  .expect("failed to build async HTTP client")  // line 42
  ```
  The builder is fed only static, valid config (timeouts, pool sizes), so failure is effectively impossible on a correctly built macOS binary. A real panic site, but not realistically reachable.
- fix: Acceptable as-is. For a never-crash daemon, have `blocking()`/`async_client()` return `Result<&Client>`, or fall back to `Client::new()` on builder failure, instead of `.expect`. Low priority.

### [LOW] thread-spawn `.expect()` at transcriber init can abort under thread exhaustion
- file: src/always/event_loop.rs:98
- category: crash-panic
- confidence: medium
- impact: On daemon startup the transcriber-init thread is spawned with `.expect`. If the OS refuses the thread (EAGAIN — process/thread/FD exhaustion on a long-lived machine), the whole daemon aborts at launch.
- problem:
  ```rust
  std::thread::Builder::new()
      .name("transcriber-init".into())
      .spawn(move || { ... })
      .expect("spawn transcriber-init thread");
  ```
  `Builder::spawn` returns `io::Result`; it fails at runtime only under resource exhaustion — rare but real for an always-on daemon. This fires only at startup (one-shot), so the blast radius is "daemon never comes up" rather than "dies mid-session".
- fix: Handle the error instead of `.expect` — log it and degrade:
  ```rust
  if let Err(e) = std::thread::Builder::new()
      .name("transcriber-init".into())
      .spawn(move || { ... })
  {
      tracing::error!(error = %e, "failed to spawn transcriber-init thread");
      // degrade: build transcriber inline, or mark init-failed
  }
  ```

### [LOW] Default keyboard-shortcut parse `.expect()` (static literals)
- file: src/always/keyboard.rs:98-107
- category: crash-panic
- confidence: high
- impact: None in practice; a latent dev-error trap.
- problem: `Combo::from_str("ctrl+alt+p").expect("default pause shortcut is valid")` and four siblings in `default_shortcuts()`. Arguments are compile-time literals → impossible-by-construction at runtime; only fails if a developer introduces a typo. (User-supplied overrides correctly use `.and_then(Combo::from_str).unwrap_or(default_…)` in `resolve_shortcuts`, which cannot panic.)
- fix: Acceptable. Optionally add a unit test asserting every default combo parses, converting the latent dev-error into a CI failure rather than a possible runtime abort.

## Verified clean — NOT defects (each read in full and reasoned through)

- **src/always/postprocess.rs:107** — `json["choices"][0]["message"]["content"].as_str().context(...)?`. serde_json `Index` on `Value` returns `Value::Null` (never panics) for missing keys / out-of-range; `.as_str()` then yields `None` and `.context()?` propagates a clean `Err`. The whole `correct_grammar` result is also consumed via `.unwrap_or_else(|_| text.to_string())` (line 65), so even the `Err` can't abort. Fully safe on malformed/error LLM responses.
- **src/always/text_match.rs** (per-utterance substitution on transcribed text — highest-stakes hot path) — all indexing bounds-checked:
  - `window[0]` / `window[n-1]` (87-89): `window = &words[i..i+n]` guarded by `if i + n > words.len() { continue; }` (line 75), `n >= 1`.
  - `words[i]` (107): inside `while i < words.len()`.
  - `custom_words[i]` (171): `i` from `custom_words_nospace.iter().enumerate()`, and `custom_words_nospace` is built 1:1 from `custom_words` (58-61).
  - `alpha[0]` (191): guarded by `if alpha.is_empty() { return ...; }` (185) — empty-after-filter (punctuation/number/emoji-only token) handled.
  - `(candidate.len() as i32 - custom_word_nospace.len() as i32)` (155): `MAX_CANDIDATE_LEN = 50`, no i32 overflow.
- **src/always/hallucination.rs** (runs on every transcript) — all indexing bounds-guarded:
  - `tokens[0]` (231): guarded by `total_tokens >= 2`.
  - `tokens[i]`/`tokens[i-1]` (239): `i` in `1..tokens.len()`.
  - `tokens[i..i+n]` / `tokens[j..j+n]` (258, 261): guarded by `i + n <= tokens.len()` and `j + n <= tokens.len()`.
  - `w[0]..w[3]` (307): `chars.windows(4)` yields slices of exactly length 4.
- **src/always/correction.rs:320** — `.expect("just inserted")`. The line directly above inserts `serde_json::Value::Array(...)` into `entry["mistranscriptions"]`, then re-fetches it with `.get_mut(...).and_then(as_array_mut)`. Local, serial, no concurrency — impossible-by-construction.
- **src/always/uds_server.rs:283** — `&line[colon + 1..]` where `colon = line.find(':')`. `:` is single-byte ASCII so `colon + 1` is always a valid UTF-8 boundary `<= line.len()`; valid even though `line` comes from the untrusted Swift socket.
- **src/always/audio.rs** (read in full) — only non-test "unwrap" patterns are `unwrap_or_else`/`unwrap_or_default` (47, 387, 390). The `unsafe` i16→u8 slice in `create_wav_bytes_i16_mono_16k` is sound. `reuse_count += 1` is u32 reset every 1000 uses → no overflow. The 5 raw `.unwrap()` are all `#[cfg(test)]`.
- **src/always/vad_silero.rs** (read in full) — per-frame `frame[i] = (*s as f32)/32768.0` bounded by the `samples.len() != 480` bail (81); `.compute()` errors propagate via `?`. All 3 `.expect()` are in tests.
- **src/db.rs** — no production `unwrap`/`expect`/`panic`/`unreachable`. The `row.get::<_, Option<i64>>(N)?.map(|v| v as u32)` casts (243-244) are Rust `as` casts (never panic; saturating/wrapping semantics only).
- **src/stt.rs** — the only `.expect()` (355/364 poison) and `panic!`/`unwrap` (467/468/477) are inside `#[cfg(test)] mod stt_unit_tests`.
- **src/glossary.rs:281,336** — both inside `#[cfg(test)] mod tests`.
- **src/always/config.rs:146-162** — `SensitivityPreset::from_str("low"/"Normal"/"HIGH"/"medium").unwrap()` on compile-time string literals (impossible-by-construction); these sit in the test-assertion block style and the input is static regardless.
- Remaining test-only files (verified by line context, out of scope): correction_queue.rs (21), focus_state.rs (4), stt_local.rs (4), paste.rs (3), stt_dispatch.rs (3), speech_action.rs (3 `panic!`), pause.rs (2).
- Files with NO panic-ish tokens at all (scanned): main.rs, cli/mod.rs, cli/menu_bar.rs, cli/logs.rs, managers/model_registry.rs, daemon.rs, clipboard_watcher.rs, per_app.rs, vocab.rs, vocab/plugins.rs, hud.rs, idle_watcher.rs, mic_monitor.rs, notification.rs, keyring.rs, log.rs, telemetry.rs, localization.rs, filter.rs, json_listener.rs, auto_enter_countdown.rs, config.rs (root), lib.rs, mod.rs, event.rs, filter_config.rs.

## Summary

No CRITICAL or HIGH panic risk found. The three LOW items above
(`http_client` builder `.expect`, `event_loop` thread-spawn `.expect`, `keyboard`
default-combo `.expect`) are all either impossible-by-construction or only
reachable under extreme resource exhaustion. The per-utterance text/audio hot
paths (`text_match.rs`, `hallucination.rs`, `vad_silero.rs`, `audio.rs`) and the
untrusted-UDS-socket parser (`uds_server.rs`) are all correctly bounds- and
error-checked. The single defensive hardening worth doing is **event_loop.rs:98**
— handle the thread-spawn error instead of `.expect` so a thread-exhausted
machine produces a logged degraded startup rather than an abort.
