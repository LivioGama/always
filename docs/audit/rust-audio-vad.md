# Audio / VAD audit — `src/always/audio.rs`, `src/always/vad.rs`, `src/always/vad_silero.rs`

Scope lens: real-time audio capture, ring/pre-buffer math, VAD frame sizing, device
disconnect/reconnect, stream restart, dropped samples, blocking in the read path,
Silero ONNX session reuse/thread-safety, windowing off-by-one, no-input-device case.

All findings derived from the full contents of the three files (read in full). Callers
traced via grep: live capture path is `event_loop.rs` (poll_speech_energy @210,
record_utterance @258); `json_listener.rs` is a stale second caller; `stt_local.rs`
reuses `create_wav_bytes_i16_mono_16k`.

---

### [MEDIUM] Stale second caller `json_listener.rs` uses an obsolete `record_utterance` / `RecordResult` API
- file: src/always/json_listener.rs:20-21 (against src/always/vad.rs:66-72 and vad.rs:50-64)
- category: maintainability
- confidence: medium
- impact: `vad.rs` defines `record_utterance(cfg, log, transcriber)` (3 args) and `RecordResult::Speech { text, energy, transcription }` (3 fields). `json_listener.rs:20` calls `vad::record_utterance(cfg, &mut log)` (2 args) and `json_listener.rs:21` matches `RecordResult::Speech { text, energy }` (2 fields). These cannot both compile against the current `vad.rs`. So `json_listener` is either excluded from the build (dead module) or it is a latent build break. Either way it is stale and will mislead/break a maintainer who re-enables JSON-lines mode.
- problem:
  ```rust
  // json_listener.rs:20
  match vad::record_utterance(cfg, &mut log) ...
  // json_listener.rs:21
  vad::RecordResult::Speech { text, energy } => { ... }
  ```
  vs the live API in vad.rs:66 / vad.rs:50.
- fix: Either update `json_listener.rs` to the 3-arg signature (passing a `transcriber`) and the 3-field `Speech` variant, or delete the module if JSON-lines mode is abandoned. Confirm whether it is listed in `src/always/mod.rs`; if not, it is dead code that should be removed.

---

### [HIGH] EOF / device-disconnect leaves the dead recorder in place mid-loop and abandons the in-progress capture
- file: src/always/audio.rs:199-219 (read_frame), src/always/audio.rs:182-197 (is_healthy), src/always/vad.rs:250-262
- category: error-handling
- confidence: high
- impact: When the mic is unplugged, TCC permission is revoked, or `rec` dies, `rec`'s stdout hits EOF. `read_frame` returns `Ok(read)` with `read < FRAME_BYTES`; the VAD loop `break`s (vad.rs:260) and the current utterance's audio so far is silently dropped/returned as Silence. The dead `RecChild` stays in `GLOBAL_RECORDER`; it is only replaced on the *next* `get_or_spawn` call via `try_wait`. A transient EOF (USB re-enumeration) is thus treated identically to a permanent death, and the active capture is lost rather than retried.
- problem:
  ```rust
  Ok(0) => {
      if read == 0 { tracing::warn!("rec_eof_on_read_frame"); }
      break;          // no respawn, no retry; dead child left in the global slot
  }
  ```
  `is_healthy()` is only consulted by `get_or_spawn`, never after an EOF inside the loop.
- fix: On EOF, force the dead child to be dropped/respawned and retry the frame once before giving up:
  ```rust
  // after EOF in record_with_local_vad: drop the dead recorder so next read respawns
  { let mut g = recorder_arc.lock(); if let Some(dead)=g.take(){ let _=dead.child.wait(); } }
  // then RecChild::get_or_spawn() and retry, or return a distinct Disconnected result
  ```

---

### [HIGH] Blocking `read_frame` is performed while holding the global recorder mutex — a wedged `rec` hangs every audio caller forever
- file: src/always/vad.rs:251-258, src/always/vad.rs:79-85, src/always/audio.rs:306-315
- category: concurrency
- confidence: medium
- impact: `read_frame` blocks on `rec`'s stdout until 960 bytes or EOF. All three readers do this *inside* the `GLOBAL_RECORDER` lock: `record_with_local_vad` (vad.rs:251), `poll_speech_energy` (vad.rs:80), `SoxAudioSource::read_frame` (audio.rs:307). If `rec` ever wedges (produces neither bytes nor EOF — possible after a CoreAudio glitch), the lock holder blocks indefinitely and every other audio caller (idle wake-on-voice poll, the next capture, the Sox source) deadlocks behind it permanently. Even in the non-wedged case, the idle poller and main loop serialize on this lock for a full frame each.
- problem:
  ```rust
  let read = {
      let mut recorder = recorder_arc.lock();
      if let Some(ref mut rec) = recorder.as_mut() {
          rec.read_frame(&mut frame_buf)?   // blocking syscall under the global lock
      } else { return Err(...); }
  };
  ```
- fix: Guarantee a single owning reader (one capture thread; other callers request via a channel), OR give `read_frame` a read timeout so a wedged `rec` cannot hold the lock indefinitely (then respawn on timeout). At minimum document that `poll_speech_energy` and `record_utterance` must never run on different threads concurrently.

---

### [MEDIUM] Silero v4 fed 480-sample (30ms) frames but the model's native window is 512 samples (32ms) — off-window inference
- file: src/always/vad_silero.rs:76-99, src/always/audio.rs:14-17, src/always/vad.rs:171-172
- category: correctness
- confidence: medium
- impact: Silero VAD v4 @16kHz expects 512-sample (32ms) chunks. This wrapper feeds exactly 480 and hard-rejects anything else. The codebase itself acknowledges the mismatch at vad.rs:171 ("Silero outputs per-512-sample (32ms) chunks"). Feeding 480 means either `vad_rs` internally re-buffers (smearing probabilities across frame edges) or runs on a non-native window, degrading probability calibration. The elaborate hysteresis/smoothing in vad.rs is partly compensating for this.
- problem:
  ```rust
  if samples.len() != 480 {
      anyhow::bail!("SileroVad::predict expects 480 samples (30ms @ 16kHz), got {}", samples.len());
  }
  let mut frame = [0f32; 480];
  ```
- fix: Feed Silero its native 512-sample window (maintain a separate 512-sample accumulator for VAD while keeping 30ms frames for energy), or verify and document that `vad_rs` correctly internally handles a 480 input. As written it is a silent accuracy regression.

---

### [MEDIUM] `SileroVad` rebuilt (full ONNX/ORT init) every utterance, yet doc advertises it as a cheap reusable singleton with no `reset()` for LSTM state
- file: src/always/vad_silero.rs:59-99 (esp. 64-65), src/always/vad.rs:157
- category: performance
- confidence: medium
- impact: `record_with_local_vad` calls `SileroVad::new()` per utterance (vad.rs:157), which materializes the model path and spins up a new `vad_rs::Vad` (ORT session) every single utterance — tens of ms + significant allocation on a path the user perceives as latency. The doc comment (vad_silero.rs:64-65) says "Cheap to call once; not designed for repeated construction," implying caching is intended — but the wrapper exposes NO `reset()`, so if a maintainer caches it (as the doc invites), Silero's recurrent LSTM hidden state from utterance N contaminates onset detection of N+1. The two intended usages contradict: reconstruct → per-utterance ORT cost; reuse → stale state.
- problem: `SileroVad` has only `new()` + `predict()`; no `reset()`. Doc says reuse; code reconstructs.
- fix: Construct once and cache; add `reset()` that re-initializes recurrent state per utterance (rebuild the inner `vad_rs::Vad` if it has no reset). Call `vad.reset()` at the top of `record_with_local_vad` instead of `SileroVad::new()`. Removes per-utterance ORT init AND prevents cross-utterance state bleed.

---

### [MEDIUM] No-input-device / TCC-denied case is indistinguishable from idle — surfaces only as a log line, never a UX event
- file: src/always/audio.rs:82-143, src/always/vad.rs:493-504
- category: ux
- confidence: medium
- impact: With no input device or denied mic permission, `rec` writes to stderr and exits. `spawn()` still returns `Ok(RecChild)` (the failure is async). The stderr drainer logs `rec_stderr` (good) but the VAD loop reads immediate EOF and returns `Silence`/`Timeout`. An always-on daemon that has lost mic access looks identical to a quiet one — no `mic_unavailable`/`permission_denied` event reaches the menu-bar app, so the user gets no actionable signal.
- problem:
  ```rust
  // vad.rs:500
  return if total_frames >= max_pre_voice_frames { Ok(RecordResult::Timeout) }
         else { Ok(RecordResult::Silence) };
  ```
  No branch for "recorder produced EOF with zero frames ever read" / known TCC stderr line.
- fix: Detect immediate-EOF-with-zero-frames (or parse the CoreAudio permission stderr line in the drainer and set an atomic flag) and emit a distinct `mic_unavailable` event so the UI can show an actionable error.

---

### [MEDIUM] Partial final frame on short read is silently dropped; `frame_buf` not zeroed so any future partial-frame handling feeds stale audio
- file: src/always/audio.rs:199-219, src/always/vad.rs:183, vad.rs:260-266
- category: data-loss
- confidence: high
- impact: If `rec` delivers a partial frame at shutdown/disconnect (e.g. 700 of 960 bytes then EOF), `read_frame` returns `Ok(700)`. The loop does `if read < FRAME_BYTES { break; }` and discards those bytes — the trailing fragment of the last utterance is lost (can clip the final word on a device hiccup). Worse foot-gun: `frame_buf`/`sample_buf` are allocated once outside the loop and never cleared, so correctness depends entirely on `read_frame` fully filling the buffer. If anyone relaxes the break to process partial frames, the unwritten tail holds the previous frame's bytes → ghost audio into both VAD and energy.
- problem:
  ```rust
  if read < FRAME_BYTES { break; }   // drops partial tail; relies on full-fill invariant
  ```
- fix: Zero the unread tail and zero-pad the partial frame so the last syllable survives and stale bytes can never leak:
  ```rust
  if read == 0 { break; }
  if read < FRAME_BYTES { for b in &mut frame_buf[read..] { *b = 0; } }
  // process zero-padded frame instead of discarding
  ```

---

### [MEDIUM] Periodic reuse-respawn kills the child before the replacement exists — spawn failure leaves a killed-but-retained recorder
- file: src/always/audio.rs:161-167
- category: resource-leak
- confidence: medium
- impact: On the 1000th reuse the code `kill()`s the current child then calls `Self::spawn()?`. If `spawn()` fails (`?` early-returns), `*recorder` still holds the *already-killed* child. The next caller sees a dead child via `is_healthy` and respawns, so it self-heals, but there is a window where the global slot holds a corpse and the current call errors out. Also the explicit `kill()` plus Drop's `kill()+wait()` on reassignment is redundant.
- problem:
  ```rust
  if rec.reuse_count > 1000 {
      let _ = rec.child.kill();          // killed first
      *recorder = Some(Self::spawn()?);  // if this fails, killed child stays in slot
  }
  ```
- fix: Spawn first; only swap (and let Drop kill+wait the old one) if the new spawn succeeds:
  ```rust
  if rec.reuse_count > 1000 {
      let fresh = Self::spawn()?;   // on error, keep the existing healthy child
      *recorder = Some(fresh);      // old RecChild Drop handles kill+wait
  }
  ```

---

### [MEDIUM] `poll_speech_energy` swallows EOF as a quiet frame — dead recorder hidden from the idle wake path, missed wake-on-voice
- file: src/always/vad.rs:76-95, called from src/always/event_loop.rs:210
- category: error-handling
- confidence: high
- impact: While idle-paused this is the only path that wakes listening by voice. It reads ONE frame; `if read < FRAME_BYTES { return Ok(false) }`. On EOF (dead recorder) this returns "no speech" with no log and no respawn — so a dead recorder during idle is invisible (the main loop logs `rec_eof_on_read_frame`, but this poll path does not). The user speaks, nothing wakes, and they must manually unpause. A single transient short read also suppresses a legitimate wake.
- problem:
  ```rust
  let read = { ... rec.read_frame(&mut frame_buf)? };
  if read < FRAME_BYTES { return Ok(false); }   // EOF and "quiet" collapsed together
  ```
- fix: Distinguish EOF (read==0 → log + force respawn) from a genuine quiet frame; optionally average 2-3 frames before deciding so one transient short read can't suppress a wake:
  ```rust
  if read == 0 { tracing::warn!("poll_speech_energy_eof"); /* drop+respawn recorder */ }
  if read < FRAME_BYTES { return Ok(false); }
  ```

---

### [LOW] WAV header `u32` size fields are written via unchecked `as u32` casts — silent corruption if the per-utterance cap ever grows past 4 GB
- file: src/always/audio.rs:230-250
- category: correctness
- confidence: high
- impact: `data_size = samples.len()*2` and `file_size = 44+data_size` are `usize`, written to the WAV header via `as u32`. The 300s `max_speech_frames` cap (vad.rs:126) bounds one utterance to ~9.6 MB today, so no current trigger — but the cast is unchecked. A future cap change or a pathological speculation snapshot would silently truncate the size fields and emit a corrupt WAV the STT backend mis-decodes/rejects.
- problem:
  ```rust
  let data_size = samples.len() * 2;
  let file_size = 44 + data_size;
  wav_data.extend_from_slice(&((file_size - 8) as u32).to_le_bytes());
  wav_data.extend_from_slice(&(data_size as u32).to_le_bytes());
  ```
- fix:
  ```rust
  let data_size = samples.len().checked_mul(2).context("wav data too large")?;
  let file_size = data_size.checked_add(44).context("wav too large")?;
  if file_size > u32::MAX as usize { anyhow::bail!("utterance exceeds WAV 4GB limit"); }
  ```

---

### [LOW] Frame-count timeouts assume real-time 30ms-per-frame delivery; SoX's 131072-byte buffer lets bursts/stalls distort the 30s/300s budgets
- file: src/always/vad.rs:125-126, vad.rs:479-490
- category: correctness
- confidence: low
- impact: `max_pre_voice_frames`/`max_speech_frames` assume each loop iteration == 30ms of wall time. With the large `--buffer 131072` (audio.rs:95), `rec` can burst buffered backlog faster than real time after a stall, so `total_frames` advances faster than wall-clock and the timeout fires early (cutting a live monologue); conversely a stalled `rec` makes the "30s pre-voice timeout" unbounded in wall time.
- problem:
  ```rust
  let max_pre_voice_frames = (cfg.timeout_secs as usize * 1000) / FRAME_MS as usize;
  if total_frames >= effective_max { break; }
  ```
- fix: Use `Instant::now()` wall-clock deadlines for both timeouts so device buffering can't distort the user-facing budgets.

---

### [LOW] Documented audio buffer pool is unused by the live capture path; pre-buffer does a fresh `to_vec()` heap alloc per idle frame
- file: src/always/audio.rs:31-71, src/always/vad.rs:185, vad.rs:473
- category: maintainability
- confidence: medium
- impact: `AUDIO_BUFFER_POOL`/`AudioBuffer` are fully implemented and tested as the "memory pool for audio buffers to reduce allocations," but `record_with_local_vad` allocates `speech_samples` directly (vad.rs:185) and the pre-buffer pushes `samples.to_vec()` per frame (vad.rs:473) — a fresh heap alloc every 30ms during the entire pre-voice window. The pool's stated purpose is unrealized; maintainers are misled about where capture allocations come from.
- problem: Pool exists (audio.rs:35-71) but the hot path bypasses it; `pre_buffer.push_back(samples.to_vec())` churns one alloc/frame.
- fix: Either route `speech_samples`/pre-buffer frames through the pool, or delete the unused pool. Replace the per-frame `to_vec()` with a ring of fixed `[i16; FRAME_SAMPLES]` arrays to stop pre-voice allocation churn.
