# Audit: stt-cloud

Scope: `src/stt.rs`, `src/stt_dispatch.rs`, `src/http_client.rs`
Focus: Cloud STT (Groq) — HTTP errors, timeouts, retry/backoff, partial/empty
responses, multipart correctness, API key leakage, JSON parse failures,
mid-stream drops, rate-limit handling, blocking reqwest in async.

All three assigned files were read in full. Callers traced to ground each
finding:
- `src/always/vad.rs::record_utterance` calls `transcribe_from_bytes` both on a
  `std::thread::spawn` speculation thread (lines 439-464) and inline on the
  daemon's main loop thread (line 574) — never on a Tokio worker.
- `src/always/event_loop.rs::run` drives the loop on the main daemon thread.
- `src/always/uds_server.rs::switch_active_backend` (lines 687-709) rebuilds the
  transcriber via `build_transcriber` on `SetActiveTranscriber` / model
  add/delete. There is NO UDS command that updates the Groq API key at runtime.
- `src/always/config.rs::from_cli` (lines 301-307) resolves the Groq key ONCE at
  daemon start; it never changes for the life of the process.

---

### [HIGH] `auth_header` ignores its `api_key` argument after the first call — the per-call/per-instance key plumbing is a silent no-op (latent wrong-key bug)
- file: src/stt.rs:33, 86-88
- category: correctness
- confidence: high
- impact: The Authorization header is memoized process-wide in a `OnceLock` and frozen on first use. `transcribe_from_bytes(audio, api_key)` and `GroqTranscriber { api_key }` both accept a key that is silently discarded on every call after the first. Today the user-facing blast radius is limited because `config.rs::from_cli` resolves the key exactly once at startup and there is no runtime key-update command — so within one process the key never differs. BUT this is a live footgun: (a) the entire `&str api_key` API lies about its contract, (b) the moment anyone adds a "update Groq API key in Settings" UDS handler (a very natural feature for this app, mirroring `SetActiveTranscriber`), every transcription will keep using the OLD key with no compile-time or runtime warning, producing permanent 401s until full daemon restart, and (c) tests that construct two `GroqTranscriber`s with different keys in one process will see the first key for both.
- problem:
  ```rust
  static AUTH_HEADER: OnceLock<String> = OnceLock::new();
  fn auth_header(api_key: &str) -> &'static str {
      AUTH_HEADER.get_or_init(|| format!("Bearer {}", api_key))
  }
  ```
  `get_or_init` runs the closure exactly once per process; later keys are ignored.
- fix: Remove the global `AUTH_HEADER`. Memoize per `GroqTranscriber` instance (it is rebuilt by `switch_active_backend`), so a future key change Just Works:
  ```rust
  pub struct GroqTranscriber { api_key: String, auth: String }
  impl GroqTranscriber {
      pub fn new(api_key: impl Into<String>) -> Self {
          let api_key = api_key.into();
          let auth = format!("Bearer {api_key}");
          Self { api_key, auth }
      }
  }
  ```
  Thread `&self.auth` into the request (or just format per request — negligible vs. a network round-trip).

---

### [HIGH] Circuit breaker / `should_fall_back` never actually falls back to local — the documented resilience contract is not implemented; utterances are silently dropped during a Groq outage
- file: src/stt.rs:9-12, 51-52, 81-83; src/always/vad.rs:574-583
- category: error-handling
- confidence: high
- impact: The module doc (stt.rs:9-12) and `Unavailable` docstring (stt.rs:51-52) promise the breaker lets "the daemon fall back to local heuristics instead of stalling the user on an outage," and `should_fall_back()` exists for that. But NO caller acts on it. `vad.rs:574-583` just maps any `transcribe_from_bytes` error to `return Err(err).context("failed to transcribe utterance")`; the speculation path (vad.rs:449-451) likewise only stores the `Err`. Grep across `vad.rs`/`event_loop.rs` shows zero references to `should_fall_back`, `build_groq`, or `LocalTranscriber` for recovery. So during a Groq outage the breaker only converts "slow failure" into "fast failure" — the user still loses every utterance for the 60 s cool-down, with no local recovery. The advertised graceful degradation does not exist.
- problem:
  ```rust
  // vad.rs:579-582
  Err(err) => {
      event::global_broadcaster().transcribing_stopped();
      return Err(err).context("failed to transcribe utterance");  // no fallback, utterance lost
  }
  ```
- fix: Either (a) implement the promised fallback — when `should_fall_back()` is true (or any cloud failure occurs while a downloaded local model exists), retry the utterance through a local transcriber before giving up; or (b) if no fallback is intended, delete the "fall back to local heuristics" claims from stt.rs:9-12 and 51-52 so maintainers don't rely on a guarantee that isn't there.

---

### [HIGH] HTTP 200 with an unparseable body fails permanently: returns `Other`, skips `record_failure()`, never retries, never opens the breaker
- file: src/stt.rs:249-251
- category: error-handling
- confidence: high
- impact: A mid-stream network drop, a CDN/proxy HTML error page returned as 200, a truncated/gzip-mismatched body, or a malformed `ALWAYS_GROQ_BASE_URL` proxy all produce HTTP 200 + non-JSON. `resp.json()` then errors and the `?` returns `SttError::Other` immediately — WITHOUT calling `record_failure()`. Consequences: (1) no retry — a transient truncation is treated as fatal; (2) `should_fall_back()` is false for `Other`; (3) the breaker never opens because the failure is never recorded. Every affected utterance fails identically with a generic "failed to parse Groq Whisper response" and zero recovery. This is exactly the "mid-stream network drop / partial response" failure mode in the focus lens, mishandled.
- problem:
  ```rust
  let mut result: TranscriptionResult = resp
      .json()
      .context("failed to parse Groq Whisper response")?;   // early return: no record_failure(), no retry
  ```
  Contrast the 4xx path (stt.rs:271-276) and network path (stt.rs:278-288), both of which record/retry appropriately.
- fix: Treat a parse failure on a 2xx as transient — retry like a network error and record the failure on final exhaustion:
  ```rust
  match resp.json::<TranscriptionResult>() {
      Ok(mut result) => { result.text = result.text.trim().to_string(); record_success(); return Ok(result); }
      Err(e) => {
          tracing::warn!(attempt = attempt + 1, error = %e, "Groq 200 with unparseable body; will retry");
          last_network_err = Some(e);
          if attempt + 1 < MAX_ATTEMPTS { std::thread::sleep(backoff_with_jitter(attempt)); }
          continue;
      }
  }
  ```

---

### [MEDIUM] 429 retry ignores `Retry-After`; fixed ~0.6 s total backoff burns the entire retry budget inside the rate-limit window
- file: src/stt.rs:258-268
- category: error-handling
- confidence: high
- impact: On 429 the code sleeps the fixed exponential backoff (only two inter-attempt gaps: ~200 ms + ~400 ms ≈ 0.6–0.75 s total; the 3rd attempt has no trailing sleep), then surfaces `RateLimited`. Groq returns `Retry-After` telling you how long to wait (often seconds). Ignoring it means all 3 attempts can fire inside a window where the server already said "wait N s," instantly exhausting the budget and increasing the chance of a harder/longer rate-limit under sustained dictation.
- problem:
  ```rust
  if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
      ...
      if attempt + 1 < MAX_ATTEMPTS { std::thread::sleep(backoff_with_jitter(attempt)); } // ignores Retry-After
      continue;
  }
  ```
- fix: Read `resp.headers().get(reqwest::header::RETRY_AFTER)` (delta-seconds form) before consuming the body on 429/503, and sleep `max(retry_after, backoff_with_jitter(attempt))`, capped at a ceiling near the request timeout.

---

### [MEDIUM] Captured error body is unbounded and unredacted before being stored/logged in `ClientError`
- file: src/stt.rs:271-276, 65-66
- category: security
- confidence: medium
- impact: On a non-429 4xx, `resp.text()` is captured with no size cap and embedded verbatim into `SttError::ClientError { body }`, whose `Display` (stt.rs:65-66) is then surfaced by the caller (vad.rs wraps it into `anyhow!`-style context that gets logged). Groq does not echo the bearer token in error bodies today (so not a confirmed key leak — confidence medium), but with a user-overridable base URL (`ALWAYS_GROQ_BASE_URL`) pointing at arbitrary upstreams/proxies, an unbounded body can bloat always-on daemon logs (which may propagate to central collectors) and could surface sensitive request context.
- problem:
  ```rust
  let body = resp.text().unwrap_or_default();
  record_failure();
  return Err(SttError::ClientError { status: status.as_u16(), body });
  ```
- fix: Truncate (`body.chars().take(512).collect::<String>()`) and scrub any `Bearer ...` substring before storing. Add a regression test asserting the key never appears in any `SttError` variant's `Display`.

---

### [MEDIUM] Tiny retry envelope vs. 60 s breaker lockout: a brief blip costs the user a full minute of cloud STT (and the local fallback is not implemented — see HIGH above)
- file: src/stt.rs:28-31, 264-266
- category: error-handling
- confidence: medium
- impact: Worst-case in-call backoff is ~0.6 s across two gaps, so a 1–2 s upstream blip exhausts retries; three such utterances trip the breaker, which then short-circuits cloud STT for 60 s (`CIRCUIT_OPEN_DURATION`). Because the "fall back to local" path is not wired (HIGH finding), the user simply loses dictation for up to a minute. The asymmetry (tiny retry, long hard lockout) is aggressive for an interactive tool.
- problem: `MAX_ATTEMPTS = 3`, `BASE_BACKOFF_MS = 200`, `CIRCUIT_OPEN_DURATION = 60s`.
- fix: Use half-open probing (after a short cool-down, allow one trial request; only extend the cool-down on repeated failure), and/or widen the retry envelope (4–5 attempts, base 300–500 ms). Most importantly, wire the real local fallback so a tripped breaker degrades instead of going dark.

---

### [MEDIUM] Per-call request timeout × attempts can block the recording thread for ~45 s with no intermediate user feedback; breaker is checked only once
- file: src/stt.rs:218-220, 229, 239-243; src/http_client.rs:7
- category: performance
- confidence: medium
- impact: `transcribe_from_bytes` runs synchronously (on the speculation thread or the main loop thread). Each of the 3 attempts can hang up to the shared 15 s `REQUEST_TIMEOUT` (http_client.rs:7) on a stalled connection, so one call can block ~45 s + backoff. The breaker is read only once at entry (stt.rs:218-220), never between attempts, so a concurrent failure cannot cut a long in-flight call short. 15 s is generous for a short dictation upload; meanwhile the user stares at a stuck "Transcribing" badge.
- problem: No STT-specific (shorter) timeout; `circuit_block_remaining_ms()` is consulted only before the loop.
- fix: Give the STT request a dedicated, shorter timeout via `RequestBuilder::timeout(Duration::from_secs(8))` instead of relying on the shared 15 s client default, and re-check `circuit_block_remaining_ms()` between attempts.

---

### [LOW] Circuit-breaker deadline uses wall-clock millis (`now_ms`) and swallows clock errors to `0`, making it vulnerable to clock jumps
- file: src/stt.rs:139-150, 157-161
- category: correctness
- confidence: low
- impact: `CIRCUIT_OPEN_UNTIL_MS` is a wall-clock deadline. An NTP step or manual clock change can shorten/extend the cooldown arbitrarily. And if `duration_since(UNIX_EPOCH)` ever fails, `now_ms()` returns 0, which with a set deadline reports the breaker as open for ~60000 ms regardless of real elapsed time. Rare on macOS, but the silent `unwrap_or(0)` hides it.
- problem:
  ```rust
  .map(|d| d.as_millis() as u64).unwrap_or(0)
  ```
- fix: Use a monotonic `std::time::Instant` for the breaker deadline (immune to clock jumps) instead of wall-clock millis.

---

### [LOW] `record_failure` computes the open decision from a stale `fetch_add` snapshot — "3 consecutive failures" is ill-defined under concurrency
- file: src/stt.rs:157-168
- category: concurrency
- confidence: low
- impact: The breaker statics are genuinely shared across threads — `transcribe_from_bytes` runs both on the main loop AND on the `std::thread::spawn` speculation thread (vad.rs:439, 574), and these CAN overlap (a speculation kicked off at tentative silence may still be in flight when the main loop falls through to a fresh transcription). The logic `let prev = fetch_add(1); if prev + 1 >= THRESHOLD` evaluated on a per-thread snapshot, combined with an independent `record_success` store-0, means "consecutive" is not well-defined when those two paths interleave: the breaker can open early/late, and a success on one path resets the counter mid-count on the other. Severity is LOW because in practice overlap is brief and the worst case is a slightly-mistimed breaker, not data loss.
- problem:
  ```rust
  let prev = CONSECUTIVE_FAILURES.fetch_add(1, Ordering::AcqRel);
  if prev + 1 >= CIRCUIT_OPEN_FAILURES { ... }
  ```
- fix: Guard count + open_until in one mutex-protected struct (or a CAS loop) so the open decision is atomic with respect to the success-reset, given the confirmed two-thread access.

---

### [LOW] Groq path hardcodes `language=en`, ignoring `cfg.lang` (the local path honors it) — non-English users get forced-English decoding
- file: src/stt.rs:198-200
- category: correctness
- confidence: medium
- impact: `build_local` threads `cfg.lang` (with an `auto` sentinel → `None`) into the engine (stt_dispatch.rs:198-203), but the Groq multipart form unconditionally sends `language=en`. A non-English user on the Groq backend gets English-forced Whisper decoding, degrading accuracy. Inconsistent with the rest of the pipeline.
- problem:
  ```rust
  .text("model", WHISPER_MODEL)
  .text("response_format", "verbose_json")
  .text("language", "en");
  ```
- fix: Plumb the configured language into `GroqTranscriber`/`transcribe_from_bytes`; omit the `language` field when `auto`/empty so Whisper auto-detects, mirroring the local path's `None`.

---

### [LOW] `http_client::blocking()` / `async_client()` panic via `.expect(...)` on build failure
- file: src/http_client.rs:25-26, 41-42
- category: crash-panic
- confidence: medium
- impact: If reqwest fails to construct the client (e.g. TLS backend init failure on a hardened system), `.expect("failed to build HTTP client")` panics. On the speculation thread the caller's `catch_unwind` (vad.rs:446) converts it to a failed utterance, but every subsequent utterance re-hits the same panic — the cloud path is permanently dead for the process with no degradation.
- problem:
  ```rust
  .build().expect("failed to build HTTP client")
  ```
- fix: Acceptable for a one-time init, but prefer returning a `Result`/`Lazy<Result<...>>` so a client-build failure degrades to "cloud unavailable" rather than panicking, especially given the (currently missing) local fallback.

---

## Notes / checked and found OK
- Empty/no-speech responses ARE handled correctly downstream: `vad.rs:587-591` trims and returns `RecordResult::DroppedNoise` on empty text (no spurious paste). No defect there.
- Blocking reqwest is SAFE here: `transcribe_from_bytes` runs on `std::thread::spawn` threads and the daemon's main loop — never on a Tokio worker — so it does not starve the async runtime. (Focus-lens "blocking reqwest in async" item: clean.)
- Runtime API-key update: there is currently NO UDS command to change the Groq key, so the `OnceLock` auth bug above is latent rather than actively user-triggered today (hence HIGH, not CRITICAL). Worth fixing before any key-update feature lands.
- 4xx (non-429) correctly fails fast without retry (stt.rs:270-276).
- Audio buffer cloned per attempt (stt.rs:231) — acknowledged in the doc, fine for MB-scale utterances.
- Backoff has jitter (stt.rs:179-183) — avoids thundering-herd alignment.
- `transcriptions_url()` trims trailing slashes on the override base (stt.rs:96-105) — correct.
- Connect/request/pool/keepalive timeouts set on both clients (http_client.rs:7-44) — no missing-timeout hang.
- `build_groq` rejects empty/missing key with a clear actionable error (stt_dispatch.rs:169-182) — good.
- `transcribe_from_bytes` calls on the speculation path are wrapped in `catch_unwind` (vad.rs:446) — a panic won't kill the VAD thread.
