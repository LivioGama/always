# Audit — event-loop scope

Files read in full (with line numbers): `src/always/event_loop.rs`,
`src/always/speech_action.rs`. Supporting modules read to verify the
state-machine contracts: `src/always/pause.rs` (paste-in-flight lock,
dictation buffer, effective-pause), `src/always/paste.rs`
(`copy_to_clipboard`, `paste_text`, `replace_via_undo`),
`src/always/auto_enter_countdown.rs`. All line numbers below are verified
against the actual files.

---

### [CRITICAL] `copy_to_clipboard(...)?` leaks the paste-in-flight lock forever — daemon goes permanently mute
- file: src/always/event_loop.rs:495
- category: concurrency
- confidence: high
- impact: After `try_begin_paste()` acquires the global `PASTE_IN_FLIGHT` lock, the very next line can fail with `?` and propagate the error out of `handle_speech` WITHOUT releasing the lock. From then on every `try_begin_paste()` returns false, so every future utterance is dropped at line 461 as "duplicate_paste_suppressed_in_flight". The daemon keeps listening, transcribing, and heart-beating, but never pastes again until it is restarted. This is the worst class of bug for an always-on background daemon: silent, permanent, indistinguishable from "working" in the UI.
- problem: `try_begin_paste()` is taken at line 461 and `copy_to_clipboard` can fail before any release path runs:
  ```rust
  if !pause::try_begin_paste() {            // line 461 — lock ACQUIRED below this point
      ...
      return Ok(());
  }
  ...
  paste::copy_to_clipboard(paste_clipboard)?;   // line 495 — `?` returns WITHOUT end_paste()
  ```
  `copy_to_clipboard` genuinely fails in practice (`paste.rs:52`): it spawns `pbcopy`, writes to its stdin, and `bail!`s if pbcopy is missing / exits non-zero / the pipe breaks. The lock is held by an `AtomicBool` (`pause.rs:267`) with no RAII guard and no release on this path. The error bubbles to `run()`'s loop (event_loop.rs:233-246), which just logs and sleeps 100ms — the lock is never reset.
- fix: Wrap everything after a successful `try_begin_paste()` in an RAII guard that calls `end_paste()` on drop, then `disarm()` it only when ownership is deliberately transferred to the async grammar task:
  ```rust
  if !pause::try_begin_paste() { ...; return Ok(()); }
  struct PasteGuard(bool);
  impl Drop for PasteGuard { fn drop(&mut self){ if self.0 { pause::end_paste(); } } }
  let mut guard = PasteGuard(true);
  ...
  paste::copy_to_clipboard(paste_clipboard)?;   // now releases on error
  ...
  if grammar_patch_async { guard.0 = false; spawn_grammar_patch(...); } // task owns it
  ```
  (`spawn_grammar_patch` already uses exactly this RAII pattern internally at lines 679-685 — mirror it here.)

---

### [HIGH] `apply_grammar_blocking` runs an untimed cloud LLM call via `rt.block_on` on the main loop thread — a hung endpoint freezes the whole daemon
- file: src/always/event_loop.rs:623
- category: performance
- confidence: high
- impact: On the merge path, and whenever async-grammar is disabled, the synchronous loop thread blocks on the cloud post-processor (`pp.process`) for the full network round-trip. While blocked, no new utterance can be recorded or transcribed. There is no timeout, so a stalled/hanging LLM connection blocks the loop indefinitely — the daemon stops responding to speech entirely (the in-flight grammar call cannot be cancelled).
- problem:
  ```rust
  fn apply_grammar_blocking(text: &str, cfg: &AlwaysConfig, rt: &Handle) -> String {
      if cfg.postprocess_config.grammar_correction_enabled
          && let Some(ref pp) = cfg.post_processor
      {
          let started = Instant::now();
          match rt.block_on(pp.process(text, None)) {   // line 623 — no timeout
  ```
  This runs on the same thread that drives `record_utterance` → `handle_speech`. The existing `Err` arm (line 644) already falls back to the acoustic text gracefully, so a timeout fallback would be a natural, low-risk addition.
- fix: Bound it: `rt.block_on(async { tokio::time::timeout(Duration::from_secs(N), pp.process(text, None)).await })` and fall back to `text` on `Elapsed`, reusing the existing error-fallback path. Long term, route even the merge path through the async patch mechanism so the loop thread never blocks on the network.

---

### [HIGH] Paste-failure / clipboard-failure paths leave the dictation merge buffer stale, corrupting the next utterance
- file: src/always/event_loop.rs:495,517
- category: data-loss
- confidence: high
- impact: If a prior successful paste set the dictation buffer (auto-enter path, line 536) and a later utterance's `copy_to_clipboard` (495) or `paste_text` (517) fails, the buffer is NOT cleared. The next utterance computes `merge_active = pause::dictation_buffer_text().is_some()` (line 364) → true, and merges the new transcript onto text that was never actually pasted — producing a corrupted, mis-joined paste (`merge_dictation_with` prepends a delta to phantom "previous" text).
- problem: The clean reject/drop branches all call `pause::dictation_buffer_clear()` (lines 486, 507), but the two error/`?` paste paths do not:
  ```rust
  paste::copy_to_clipboard(paste_clipboard)?;          // line 495 — buffer NOT cleared
  ...
  if let Err(err) = paste::paste_text(false) {         // line 517
      pause::end_paste();
      return Err(err);                                  // buffer NOT cleared
  }
  ```
- fix: On any paste/copy failure, call `pause::dictation_buffer_clear()` before returning so the next utterance starts a clean paste instead of merging onto text that never landed. Fold this into the RAII guard from the CRITICAL finding.

---

### [HIGH] Error/`?` paste paths skip `voice_activity_ended`, leaving the overlay stuck "transcribing"
- file: src/always/event_loop.rs:495,517
- category: ux
- confidence: high
- impact: Every terminal branch in `handle_speech` that ends an utterance emits a UI-clearing event (`voice_activity_ended()` / `transcription_filtered(...)` — lines 329, 349, 457, 466, 484, 506). The two paste-failure paths emit none. When clipboard copy or paste fails, the Swift overlay (driven entirely by these UDS events) can stay showing the "voice/transcribing" state for that utterance until the next event happens to clear it — the user sees a stuck overlay with no indication the paste was dropped.
- problem:
  ```rust
  paste::copy_to_clipboard(paste_clipboard)?;     // line 495 — no UI-clear event on err
  ...
  if let Err(err) = paste::paste_text(false) {    // line 517
      pause::end_paste();
      return Err(err);                            // no UI-clear event on err
  }
  ```
- fix: On these failure paths emit `event::global_broadcaster().voice_activity_ended()` and a `transcription_filtered("paste failed — try again")` so the overlay always resets and the user is told the utterance was heard but not pasted.

---

### [MEDIUM] `process_one` holds the config read-lock across `record_utterance` + the blocking grammar call — settings writes contend for seconds
- file: src/always/event_loop.rs:257
- category: concurrency
- confidence: high
- impact: `let cfg = active_cfg.read();` holds a `parking_lot::RwLock` read guard for the ENTIRE utterance: audio capture (seconds), transcription (network), AND `apply_grammar_blocking`'s `rt.block_on` network call. The UDS server takes a write lock on the same `ActiveConfig` when the user changes settings/model. parking_lot's RwLock is writer-preferring, so a queued settings write blocks for the whole utterance, and can in turn briefly stall the next loop read. Settings changes appear to "hang" while the user is mid-sentence.
- problem:
  ```rust
  fn process_one(active_cfg: &ActiveConfig, ...) -> Result<()> {
      let cfg = active_cfg.read();                       // read guard held across everything below
      match vad::record_utterance(&cfg, log, transcriber)... {
          ... => handle_speech(&cfg, ...)?,              // includes rt.block_on(pp.process(..))
      }
  }
  ```
- fix: Clone the `AlwaysConfig` (or just the fields needed) at the top and drop the read guard before `record_utterance`. The loop already snapshots the transcriber per-iteration for exactly this reason (lines 229-232); do the same for config.

---

### [MEDIUM] Transcriber init failure is logged once and never recovered — daemon "listens" forever but can never transcribe
- file: src/always/event_loop.rs:95
- category: error-handling
- confidence: high
- impact: If `build_transcriber` fails at launch (bad/expired Groq key, missing local model file, no network), the init thread logs `transcriber_init_failed` and exits without ever calling `ready_signal.set(...)`. The `PendingTranscriber` placeholder (lines 55-58) stays pending forever. Per its own comment, the first utterance "waits for init instead of being dropped" — so the user speaks, the overlay shows activity, and nothing ever pastes, with no user-facing reason and no retry.
- problem:
  ```rust
  match build_transcriber(&cfg_for_build, &registry_for_build) {
      Ok(transcriber) => { ...; ready_signal.set(transcriber); ... }
      Err(e) => tracing::error!(error = %e, "transcriber_init_failed"),  // line 95 — dead end
  }
  ```
  No UDS event, no retry/backoff, the pending placeholder is never resolved to an error.
- fix: On failure, broadcast a degraded/error UDS event (so the GUI can surface "Speech engine failed to load — check your API key") and resolve the `PendingTranscriber` into an error-returning transcriber so utterances fail fast with a clear message, and/or schedule a bounded retry with backoff.

---

### [MEDIUM] Async grammar patch (Cmd+Z + repaste) does not re-check pause/focus — can corrupt text in the wrong app after a focus change
- file: src/always/event_loop.rs:713-728
- category: correctness
- confidence: medium
- impact: After paste-first, the async task issues Cmd+Z then repastes corrected text (`paste::replace_via_undo`, which is undo + copy + Cmd+V — paste.rs:112). It guards on `pasted_at.elapsed() > GRAMMAR_PATCH_MAX_AGE` (4s) and `keyboard::is_cmd_held()`, but does NOT re-check `pause::is_paused()` or that focus is still the original app. If, within 4s, the user paused, switched apps, or moved to another field, the Cmd+Z undoes whatever is focused now and pastes corrected text into the wrong place — data corruption in an unrelated window.
- problem:
  ```rust
  if pasted_at.elapsed() > paste::GRAMMAR_PATCH_MAX_AGE { ... return; }  // line 713
  if keyboard::is_cmd_held() { ... return; }                            // line 722
  let clipboard = format!("{cleaned} ");
  if let Err(err) = paste::replace_via_undo(&clipboard) { ... }         // line 728
  ```
  The main paste path explicitly re-checks `pause::is_paused()` (line 476) right before pasting for exactly this "focus moved to a paused app" reason; the async patch path omits the equivalent guard.
- fix: Before `replace_via_undo`, add `if pause::is_paused() { return; }` and, if `focus_state` is available, verify the focused app/bundle matches what it was at `pasted_at`; bail (keep the acoustic paste) on mismatch.

---

### [MEDIUM] `auto_enter_countdown::schedule(delay_ms == 0)` sleeps + presses Return synchronously on the loop thread
- file: src/always/event_loop.rs:538 (callsite); src/always/auto_enter_countdown.rs:22-31
- category: performance
- confidence: high
- impact: When `auto_enter_delay_ms == 0` (the documented "legacy immediate-Return" default), `schedule` takes the fast path: it `std::thread::sleep(50ms)` then presses Return synchronously, all on the daemon's main loop thread (it is called inline from `handle_speech` at line 538). Combined with the 50ms sleep already inside `paste_text`, each paste blocks the capture loop ~100ms+ doing nothing but sleeping, during which speech can be missed. The doc comment on `schedule` ("Spawns a tokio task so the daemon main loop is free to record the next utterance") is false for the 0-delay path.
- problem:
  ```rust
  // auto_enter_countdown.rs
  pub fn schedule(rt: &Handle, delay_ms: u32) {
      if delay_ms == 0 {
          std::thread::sleep(Duration::from_millis(50)); // blocks the CALLER (loop thread)
          let _ = press_return_now();
          pause::dictation_buffer_clear();
          return;
      }
      ...
  }
  ```
- fix: Spawn the 0-delay path on `rt` too (or `rt.spawn_blocking`) so the loop thread returns to recording immediately, matching the non-zero path and the function's own documentation.

---

### [LOW] `last_process` cooldown clock advances before the paste is known to have succeeded
- file: src/always/event_loop.rs:298-301
- category: correctness
- confidence: medium
- impact: `*last_process = now` is set as soon as the action is not `InCooldown` — i.e. before any paste is attempted. If the paste then fails or is dropped (copy/paste error, cmd-held, paused-mid-utterance, duplicate-suppressed), the post-paste cooldown clock has still advanced, so a quick legitimate re-utterance by the user within `cooldown_ms` is rejected as `InCooldown` even though nothing was ever pasted. The user has to wait out a cooldown for a paste that never happened.
- problem:
  ```rust
  if !matches!(action, SpeechAction::InCooldown) {
      *last_process = now;                         // advanced before paste outcome known
      log.write(Event::Transcribed { text, energy });
  }
  ```
- fix: Advance `last_process` only after a confirmed successful paste (after `paste_text` returns Ok), or roll it back on the drop/error/duplicate branches.

---

### [LOW] `idle_wake_on_voice` silently swallows audio-device errors on the 100ms poll
- file: src/always/event_loop.rs:210
- category: error-handling
- confidence: medium
- impact: While idle-auto-paused, the loop polls `vad::poll_speech_energy(&active_cfg.read()).unwrap_or(false)` every 100ms. `.unwrap_or(false)` collapses every audio error into "no speech", so a broken/disconnected input device is indistinguishable from silence and produces zero diagnostics — wake-on-voice silently never fires and there is nothing in the logs to explain why. This also re-acquires the config read lock 10×/sec.
- problem:
  ```rust
  && vad::poll_speech_energy(&active_cfg.read()).unwrap_or(false)
  ```
- fix: Log (rate-limited) on the `Err` case so a dead audio device is diagnosable, and consider a coarser idle poll interval.

---

### [LOW] `paste_similarity` mixes byte length with char-based edit distance in the normalization denominator
- file: src/always/speech_action.rs:100-105
- category: correctness
- confidence: medium
- impact: `max_len = na.len().max(nb.len())` counts BYTES, while `strsim::levenshtein` counts CHAR edits. For multi-byte text (accented Latin, CJK, emoji) the denominator is inflated relative to the edit distance, pushing the similarity ratio artificially low and weakening near-duplicate paste suppression for non-ASCII content — duplicate non-English pastes slip through the `PASTE_NEAR_DUPLICATE_SIMILARITY` gate.
- problem:
  ```rust
  let max_len = na.len().max(nb.len());            // bytes
  let dist = strsim::levenshtein(&na, &nb) as f64; // char edits
  1.0 - dist / max_len as f64
  ```
- fix: Use char counts for the denominator to match the unit of `levenshtein`: `let max_len = na.chars().count().max(nb.chars().count());`.

---

## speech_action.rs — overall

Aside from the byte-vs-char nit above, this file is genuinely clean for its
stated scope: pure, side-effect-free decision logic backed by strong
unit/property tests (the `joined == previous + delta` merge invariant,
referential-transparency check, no-double-space, empty-addition identity,
localization seam). No concurrency, no I/O, no panics on the paths read.
