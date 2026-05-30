# Rust Audit — watchers-misc scope

Scope files: `src/always/mic_monitor.rs`, `src/always/idle_watcher.rs`, `src/always/auto_enter_countdown.rs`, `src/always/notification.rs`, `src/always/telemetry.rs`, `src/always/hud.rs`, `src/always/log.rs`

Caller tracing (verified): `idle_watcher::spawn` (event_loop.rs:130), `MicrophoneMonitor` (event_loop.rs:136/146/149), `auto_enter_countdown::schedule` (event_loop.rs:538), `telemetry::init_logging` (main.rs:188). `notification::*` and `hud::*` are `pub mod`s (mod.rs:12-13) with **zero callers anywhere in the repo** — currently dead/unwired code (relevant to severity below). `tracing-appender` is pinned at `0.2.5` (Cargo.lock). Note: `telemetry.rs` holds the tracing/logging infrastructure; `log.rs` holds the `Event` enum + `Logger`.

---

### [HIGH] mic auto-pause uses MASTER pause and a fragile `lsof | grep` filter — false positives/negatives master-pause the daemon
- file: src/always/mic_monitor.rs:120-139 (decision consumed at src/always/event_loop.rs:146-167)
- category: correctness
- confidence: high
- impact: When `is_microphone_in_use()` returns a false positive, the main loop calls `pause::set_paused(true)` — the **MASTER** pause (event_loop.rs:160), not the soft idle pause — silently stopping all dictation until the mic appears free again. A false negative leaves Always dictating during a Zoom/Teams call (the exact case auto-pause exists to prevent). The detection is a single `lsof -c /coreaudio/i | grep -iv -e always -e '\brec\b' -e sox -e coreaudiod | head -5` and is imprecise on multiple axes.
- problem:
  ```rust
  Command::new("sh").args([
      "-c",
      "lsof -c /coreaudio/i 2>/dev/null | grep -iv -e always -e '\\brec\\b' -e sox -e coreaudiod | head -5",
  ])
  ```
  - `grep -iv` substring-excludes on the WHOLE lsof line (PID/USER/path columns), so any unrelated process whose path contains "always"/"sox" is wrongly excluded (false negative), and legitimate third-party apps whose bundle path contains "always" are also dropped.
  - `head -5` truncates BEFORE the producer would naturally end; a real mic user sorting after the first 5 (often-coreaudiod) lines can be cut off.
  - The selector targets `/coreaudio/i` then the filter removes `coreaudiod`, i.e. it strips the very process the selector most reliably returns — leaving only incidental matches.
- fix: Match on the COMMAND column only (parse columns; exclude our own PID via `std::process::id()` and exact helper names), filter BEFORE bounding the count, and validate the selector actually captures real device holders on the target macOS. Strongly prefer using the soft/idle pause (or a dedicated mic-pause flag) rather than MASTER pause so a misfire is less destructive, and consider CoreAudio/`AVCaptureDevice` APIs over shelling out.

---

### [MEDIUM] `run_mic_lsof` swallows spawn errors as "mic free" with no log — silent failure during calls
- file: src/always/mic_monitor.rs:132-138
- category: error-handling
- confidence: high
- impact: Any failure to spawn/`output()` `sh`/`lsof` (sandboxing, PATH, resource exhaustion) is mapped to `Ok(String::new())` = "no other app using the mic", so the daemon keeps dictating into a call and the failure is invisible (the event_loop `Err` branch that would `warn!` is never reached because the error is converted to Ok).
- problem:
  ```rust
  match output {
      Ok(result) => { ... }
      Err(_) => Ok(String::new()),
  }
  ```
- fix: Log the spawn error (`tracing::warn!(?e, "mic_lsof_spawn_failed")`) and consider returning `Err` so the caller's existing `Err(e) => warn!` path fires, or treat repeated failure as "unknown" rather than "free" given the safety posture around calls.

---

### [MEDIUM] Log "7-day retention" is documented but NOT implemented — daily files accumulate forever
- file: src/always/telemetry.rs:4 (doc claim) and src/always/telemetry.rs:31-33 (impl)
- category: resource-leak
- confidence: high
- impact: The module doc states "Daily log rotation with 7-day retention" (telemetry.rs:4), but `RollingFileAppender::new(Rotation::DAILY, log_dir, "always")` only rotates — it deletes nothing. For an always-on daemon, `~/Library/Logs/Always/always.YYYY-MM-DD` grows one file per day without bound. The documented retention feature does not exist.
- problem:
  ```rust
  // Set up rolling file appender with daily rotation
  let file_appender = RollingFileAppender::new(Rotation::DAILY, log_dir, "always");
  ```
- fix: `tracing-appender 0.2.5` (the pinned version) supports retention via the builder. Use it:
  ```rust
  let file_appender = RollingFileAppender::builder()
      .rotation(Rotation::DAILY)
      .filename_prefix("always")
      .max_log_files(7)
      .build(&log_dir)?;
  ```
  This makes the code match the documented "7-day retention" and bounds disk usage.

---

### [MEDIUM] auto_enter "supersede" clears the cancel it just set — superseded countdown can still fire Return (double-Enter)
- file: src/always/auto_enter_countdown.rs:34-41
- category: concurrency
- confidence: medium
- impact: `schedule()` requests cancel of the in-flight countdown, then immediately calls `countdown_take_cancel()` which consumes that same global cancel flag. The old task may therefore never observe the cancel and proceed to press Return, while the new task also presses Return — a spurious double-Enter. Because Return is a committing keystroke, a double-Enter can submit a form/chat message twice (data hazard).
- problem:
  ```rust
  if pause::countdown_active() {
      pause::countdown_request_cancel();   // ask old task to stop
  }
  pause::countdown_set_active(true);
  pause::countdown_set_remaining_ms(delay_ms);
  // Clear any stale cancel from a previous run we just aborted.
  let _ = pause::countdown_take_cancel();  // <-- also eats the cancel set 5 lines up
  ```
  Cancellation is routed through a single process-global boolean with no per-task identity, so "which task does this cancel belong to" is ambiguous under back-to-back schedules.
- fix: Give each countdown a monotonically increasing generation id; the ticking task checks `current_generation == my_generation` before pressing Return and aborts otherwise. Or retain the previous spawned task's `AbortHandle` and `.abort()` it synchronously before spawning the new one, instead of using a shared boolean.

---

### [MEDIUM] idle_watcher / auto_enter tokio tasks have no shutdown handle (task leak on any re-spawn)
- file: src/always/idle_watcher.rs:33-58 and src/always/auto_enter_countdown.rs:47-81
- category: resource-leak
- confidence: medium
- impact: `idle_watcher::spawn` launches an unbounded `loop { sleep(5s); ... }` and returns nothing — no `JoinHandle`, no cancellation. It can only be stopped by process exit; if `spawn` is ever called twice (config re-apply / future restart-in-process), a second watcher accumulates and both drive idle-pause logic. The auto_enter timer task likewise has no retained handle (compounds the supersede race above).
- problem:
  ```rust
  rt.spawn(async move {
      loop {
          tokio::time::sleep(CHECK_INTERVAL).await;
          ...
      }
  });
  ```
  No `CancellationToken`/`AbortHandle` retained or honored. (Today event_loop.rs:128-130 spawns it exactly once for the daemon lifetime, so this is latent rather than active — hence MEDIUM.)
- fix: Return the `JoinHandle` (or accept a `CancellationToken`) and `tokio::select!` on cancellation in the loop; have callers abort the previous instance before spawning a replacement.

---

### [MEDIUM] Blocking `std::thread::sleep` on the async entry path of `schedule()` (delay_ms == 0)
- file: src/always/auto_enter_countdown.rs:22-32
- category: concurrency
- confidence: medium
- impact: For `delay_ms == 0`, `schedule()` runs `std::thread::sleep(50ms)` + `press_return_now()` synchronously on the caller's thread. `schedule` is invoked from `event_loop` (event_loop.rs:538) and takes a tokio `Handle`; if the caller is a runtime worker, this blocks that worker for ~50ms+, stalling co-scheduled tasks. The non-zero path correctly uses `tokio::time::sleep` on a spawned task.
- problem:
  ```rust
  if delay_ms == 0 {
      std::thread::sleep(Duration::from_millis(50));
      let _ = press_return_now();
      pause::dictation_buffer_clear();
      return;
  }
  ```
- fix: Spawn the zero-delay path too (`rt.spawn` with `tokio::time::sleep`), or run `press_return_now` via `spawn_blocking`. Avoid `std::thread::sleep` on async threads.

---

### [LOW] auto_enter zero-delay path swallows `press_return` failure (no log)
- file: src/always/auto_enter_countdown.rs:27
- category: error-handling
- confidence: high
- impact: A failed Return synthesis on the 0-delay path is silently dropped (`let _ =`), unlike the timed path which logs `auto_enter_countdown_press_return_failed`. Makes "auto-enter intermittently not pressing Enter" undiagnosable.
- problem:
  ```rust
  let _ = press_return_now();
  ```
  vs timed path:
  ```rust
  if let Err(error) = press_return_now() {
      tracing::error!(?error, "auto_enter_countdown_press_return_failed");
  }
  ```
- fix: Log the error in the zero-delay branch identically.

---

### [LOW] Command/AppleScript injection latent in `notify()` (currently dead code, but unsafe if wired)
- file: src/always/notification.rs:11-23
- category: security
- confidence: high
- impact: `notify` only escapes double-quotes, not backslashes, before interpolating `title`/`message` into an `osascript` string literal. A backslash in the input breaks the AppleScript string (notification fails) or, with crafted input, allows AppleScript injection executed by `osascript`. `notification::*` has **no callers today** (dead code), so this is not currently reachable — but `show_pause_status_change`/`show_auto_enter_status_change` exist as public API and could be wired to app-name/transcription-derived data, making the input untrusted.
- problem:
  ```rust
  format!(
      r#"display notification "{}" with title "{}" sound name "Ping""#,
      message.replace("\"", "\\\""),
      title.replace("\"", "\\\"")
  )
  ```
  Backslashes are not escaped; AppleScript string literals require escaping both `\` and `"`.
- fix: Either remove the dead module, or before wiring it pass values via argv instead of interpolation:
  ```rust
  Command::new("osascript").args([
      "-e",
      "on run argv\n  display notification (item 1 of argv) with title (item 2 of argv)\nend run",
      message, title,
  ])
  ```
  so no metacharacter in data can alter the program. At minimum, escape backslashes before quotes.

---

### [LOW] `notify()` blocks on `osascript` `output()` with no timeout (latent; dead code today)
- file: src/always/notification.rs:6-35
- category: performance
- confidence: medium
- impact: `notify` calls `command.output()`, which blocks the calling thread until `osascript` (a Cocoa-loading subprocess) exits, with no timeout — a stuck osascript (no GUI session, WindowServer stall) would pin the caller thread indefinitely. Not currently reachable (no callers), but a trap if this module is ever wired into the event loop or a UDS handler.
- problem:
  ```rust
  match command.output() { Ok(_) => Ok(()), Err(_) => { ...; Ok(()) } }
  ```
- fix: Before wiring, switch to fire-and-forget `command.spawn()` (drop the handle) or a bounded `wait_timeout` on a dedicated thread so the daemon hot path never blocks on a GUI subprocess.

---

### [LOW] `notify()` returns `Result` whose `Err` arm is unreachable — misleads callers
- file: src/always/notification.rs:27-34
- category: maintainability
- confidence: high
- impact: The `Err(_)` branch warns then returns `Ok(())`, so `notify` effectively never returns `Err`; `?`-propagation in `show_pause_status_change`/`show_auto_enter_status_change` is dead, and a genuinely failed notification (no GUI session) is indistinguishable from success to any caller.
- problem:
  ```rust
  Err(_) => {
      tracing::warn!(title, message, "notification_fallback");
      Ok(())
  }
  ```
- fix: Make the function infallible (`-> ()`), or actually surface the failure. The current `Result` signature implies a recoverable failure path that never occurs.

---

### [LOW] `MicrophoneMonitor` throttle state is `&mut`-coupled (single-owner only)
- file: src/always/mic_monitor.rs:27-41
- category: concurrency
- confidence: low
- impact: The 3s throttle lives in `&mut self` (`last_check`, `last_usage_state`), so the monitor must be owned by one thread. Fine today (single owner in event_loop), but if shared behind a lock later, the throttle serializes a blocking `lsof` subprocess under that lock. Easy future foot-gun.
- problem: throttle state coupled to `&mut self`, no internal synchronization.
- fix: Document single-owner requirement, or move the throttle to atomics so the monitor is `&self`/`Sync`-safe.

---

### [LOW] `hud.rs` and `notification.rs` are dead modules (no callers)
- file: src/always/hud.rs:1-16, src/always/notification.rs:1-63 (declared at src/always/mod.rs:12-13)
- category: maintainability
- confidence: high
- impact: Both are `pub mod`s with no callers anywhere in the repo (`hud::show_enhanced_status` only calls its own `show_status_bar_indicator`; `notification::*` is entirely unreferenced). Dead code carries the latent injection/blocking risks above and rots silently (it is not exercised, so the unsafe `notify` path won't be caught by normal testing).
- problem: unreferenced public modules retained in the tree.
- fix: Remove them, or wire them and fix the safety issues first. If intentionally kept for future use, mark with `#[allow(dead_code)]` and a TODO so the intent is explicit.

---

## Privacy review (telemetry / transcript content) — PASS with one minor note
- `should_log_transcripts()` (telemetry.rs:136-142) correctly defaults transcript-text logging OFF in release (`cfg!(debug_assertions)` false), opt-in via `ALWAYS_LOG_TRANSCRIPTS=1`, honors `=0`/`=false` to force off. Good.
- `log.rs` gates every transcript-bearing field (`text`, `raw_text`, `processed_text`) behind `should_log_transcripts()`, otherwise emitting only `chars`/`energy` counts (log.rs:79-151). No transcript content leaves the device — these modules do local file/oslog logging only, no network telemetry. Good.
- Minor note (no separate finding): `Event::MicrophoneAutoPaused { apps }` (log.rs:152-153) and event_loop.rs:155 log detected app names unconditionally. These are app identities (metadata), not transcripts; acceptable for most threat models but recorded in the always-on info log regardless of the transcript flag.

---

## Items checked and found clean
- `hud.rs`: pure `tracing::info!`, no I/O, threads, or state (only flagged for being dead code above).
- `idle_watcher.rs`: disabled path (`idle_pause_secs == 0`) early-returns; 5s sleep is not a busy-poll; guards correctly against re-pausing when master/idle already paused.
- `auto_enter_countdown` tick loop uses `saturating_sub` (no underflow) and 100ms sleep (not a busy loop).
- `mic_monitor` is internally throttled to 3s (last_check) so the per-iteration `is_microphone_in_use()` call in the main loop does not spawn `lsof` on every tick.
- `telemetry::get_log_directory()` has correct per-platform fallbacks and never panics; `create_dir_all` error is propagated via `?` at startup (acceptable fail-fast).
- `init_oslog` ignoring its init result is justified by the in-code comment (tracing owns the log global); acceptable.
