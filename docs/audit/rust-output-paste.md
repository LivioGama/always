# Audit: output-paste scope

Files: `src/always/paste.rs`, `src/always/keyboard.rs`, `src/always/clipboard_watcher.rs`
(callers traced in `event_loop.rs`, `correction.rs`, `pause.rs`, `auto_enter_countdown.rs`)

---

### [CRITICAL] Production paste path destroys the user's clipboard with no save/restore
- file: src/always/paste.rs:179-183 (and call site src/always/event_loop.rs:495)
- category: data-loss
- confidence: high
- impact: Every single dictation silently overwrites whatever the user had copied (a URL, a password, a code snippet, a large selection). On an always-on daemon that pastes dozens of times an hour, the system clipboard is permanently clobbered and the prior contents are unrecoverable.
- problem: The main paste path copies the transcript to the pasteboard and never restores the previous contents. `copy_to_clipboard` unconditionally replaces the pasteboard:
  ```rust
  pub fn copy_to_clipboard(text: String) -> Result<()> {
      let mut pbcopy = Command::new("pbcopy")...
      ...write_all(text.as_bytes())...
  }
  pub fn paste(text: &str, auto_enter: bool) -> Result<()> {
      copy_to_clipboard(text.to_string())?;
      paste_text(auto_enter)?;
      Ok(())
  }
  ```
  And the production event-loop call (`event_loop.rs:495`/`:517`) does the same: `paste::copy_to_clipboard(paste_clipboard)?;` then `paste::paste_text(false)`. No snapshot of the prior pasteboard, no restore after the Cmd+V is consumed. Notably, `correction.rs` (the Cmd+C capture path) *does* snapshot and restore (`read_clipboard_via_pbpaste` -> `restore_clipboard`), which proves the maintainers know the pattern but never applied it to the primary paste path.
- fix: Snapshot the pasteboard (incl. `changeCount`) before copying, paste, then restore after the synthetic Cmd+V has been consumed. Minimal version:
  ```rust
  pub fn paste(text: &str, auto_enter: bool) -> Result<()> {
      let prev = read_clipboard().ok();           // pbpaste snapshot
      copy_to_clipboard(text.to_string())?;
      paste_text(auto_enter)?;
      if let Some(prev) = prev {
          // delay so the target app has consumed Cmd+V first
          std::thread::sleep(Duration::from_millis(150));
          let _ = copy_to_clipboard(prev);        // best-effort restore
      }
      Ok(())
  }
  ```
  Ideally make restore opt-out via config, and guard the restore on `changeCount` not having advanced (see next finding). NSPasteboard preserves only string flavors via pbpaste, so document that images/RTF on the prior clipboard are still lost — but plain-text round-trip is the high-value fix.

---

### [HIGH] Clipboard restore (where it exists) races with the user copying during paste
- file: src/always/correction.rs:430-461 (pattern that the proposed paste-path fix must avoid; correction path itself is affected)
- category: concurrency
- confidence: high
- impact: If the user (or the passive watcher, or any app) copies something during the ~tens-of-ms window between snapshot and restore, the daemon clobbers the *new* clipboard with the *old* snapshot — silently destroying a fresh copy the user just made.
- problem: The restore in `read_selection_via_cmd_c` writes back the snapshot unconditionally (correction.rs:454-458):
  ```rust
  if let Some(prev) = prev
      && let Err(e) = paste::copy_to_clipboard(prev)
  {
      tracing::warn!(error = %e, "failed to restore previous clipboard contents");
  }
  ```
  There is no check that the pasteboard `changeCount` is still the value we expect. The snapshot/restore window here is large (up to a 250ms poll loop at correction.rs:431-449), so any concurrent copy in that window is overwritten. The same hazard will apply to the CRITICAL fix above if implemented naively.
- fix: Read `NSPasteboard.general.changeCount` at snapshot time; before restoring, re-read it and only restore if it equals `our_copy_change_count` (i.e. nothing copied since our own write). If it advanced, the user/app put something new on the clipboard — leave it. This requires reading changeCount via the `objc`/`cocoa` bridge rather than pbcopy/pbpaste, but it is the only correct way to make save/restore safe on a live system.

---

### [HIGH] Paste fires into whatever app has focus *now*, with no re-validation after the network round-trip
- file: src/always/event_loop.rs:476-520
- category: correctness
- confidence: high
- impact: The transcript can be pasted into the wrong application (or wrong field — e.g. a password field, a chat that auto-sends, a terminal that executes it). On a cloud-STT path the latency between "user finished speaking" and "paste happens" is hundreds of ms to seconds; the user may have Cmd-Tabbed away. Pasting a transcript into a terminal and then auto-Entering it is a destructive-command risk.
- problem: There is **no focus re-validation at all** before the paste — `event_loop.rs` contains zero references to focus/frontmost-app at the paste site. The only mid-flight guard is the pause check at lines 476-489:
  ```rust
  if pause::is_paused() { ... return Ok(()); }   // catches pause toggles only
  ...
  paste::copy_to_clipboard(paste_clipboard)?;     // line 495
  ...
  if let Err(err) = paste::paste_text(false) { ... }  // line 517
  ```
  `paste_text` synthesizes Cmd+V at `CGEventTapLocation::HID`, which always lands in the *currently* focused app — not the app that held focus when transcription started. If the user Cmd-Tabs to a different editable app (a terminal, a chat box, a password field) during the cloud-STT round-trip, `is_paused()` returns false and the transcript is pasted into that wrong app, then optionally auto-Entered. There is no snapshot of the frontmost app at recording-start to compare against, and no allowlist suppressing auto-enter in sensitive contexts.
- fix: Snapshot the frontmost app/window bundle id at recording-start; immediately before `paste_text`, re-read the current frontmost app and compare. If it changed, skip the synthetic paste (leave text on clipboard + notify the user) rather than pasting into the wrong window. Additionally suppress `auto_enter` when the focused element is a secure text field (AX `AXSecureTextField`) or the frontmost app is a terminal, unless the user explicitly opted in per-app.

---

### [HIGH] `copy_to_clipboard` leaks a pbcopy child process / clipboard write on early error
- file: src/always/paste.rs:56-70
- category: resource-leak
- confidence: medium
- impact: If `write_all` to pbcopy's stdin fails (e.g. broken pipe because pbcopy died), the function returns early via `?` and never calls `pbcopy.wait()`. The child becomes a zombie. On an always-on daemon doing thousands of pastes, repeated failures accumulate zombie processes / file descriptors.
- problem:
  ```rust
  pbcopy.stdin.as_mut().context("Failed to open pbcopy stdin")?
      .write_all(text.as_bytes())
      .context("Failed to write transcript to clipboard")?;   // early return, no wait()
  let status = pbcopy.wait().context("Failed waiting for pbcopy")?;
  ```
  The `?` on `as_mut()` and on `write_all` both return before `wait()`, leaving the spawned child unreaped.
- fix: Ensure the child is always waited on, e.g. take stdin in a scope so it drops (closing the pipe) and `wait()` in all paths:
  ```rust
  let mut child = Command::new("pbcopy").stdin(Stdio::piped()).spawn()?;
  let write_res = (|| {
      child.stdin.as_mut().context("stdin")?.write_all(text.as_bytes()).context("write")
  })();
  // drop stdin then always reap
  let _ = child.wait();
  write_res?;
  ```

---

### [MEDIUM] Transcript is lost if `paste_text` fails — no durable fallback sink
- file: src/always/event_loop.rs:495-520 with src/always/pause.rs:229 set_last_pasted
- category: data-loss
- confidence: medium
- impact: If the synthetic Cmd+V fails (`paste_text` returns `Err` — e.g. `CGEventSource::new` fails because Accessibility/Input-Monitoring permission was revoked, a common macOS occurrence after OS updates), the transcript is gone. The code bails with the error and the user's spoken sentence is unrecoverable; the only place the text lived was the clipboard, which the next dictation overwrites.
- problem:
  ```rust
  paste::copy_to_clipboard(paste_clipboard)?;   // text now only on clipboard
  ...
  if let Err(err) = paste::paste_text(false) {
      pause::end_paste();
      return Err(err);                          // transcript dropped, no notify, no history
  }
  ```
  `set_last_pasted` (the only in-memory retention) is reached only on the success path (line 557), so a failed paste leaves no recoverable copy. There is no persistent transcript history sink in this path — `LAST_PASTED`/`LAST_FILTERED` are single-slot in-memory snapshots.
- fix: On `paste_text` failure, surface a notification ("paste failed — transcript on clipboard, check Accessibility permission") and push the text into a durable last-N history independent of the clipboard, instead of silently returning `Err`. Leaving the text on the clipboard is fine as a stopgap, but the user must be told it wasn't pasted.

---

### [MEDIUM] Fixed sleeps for paste/auto-enter timing are racy across apps
- file: src/always/paste.rs:155 (50ms before Return), :106 (40ms before repaste in undo)
- category: correctness
- confidence: medium
- impact: On a busy/slow machine or a slow Electron app, 50ms may be too short: the synthetic Return can arrive before Cmd+V has been processed, producing the exact "Cmd held → Cmd+Return binding" bug the comment describes, or a Return that lands in the wrong field. On a fast machine it adds needless latency. The 40ms undo->repaste in `undo_last_paste`/`replace_via_undo` has the same fragility.
- problem:
  ```rust
  std::thread::sleep(std::time::Duration::from_millis(50)); // before Return
  ...
  std::thread::sleep(std::time::Duration::from_millis(40)); // after Cmd+Z, before repaste
  ```
  These are magic constants tuned for one environment; there is no acknowledgement/confirmation that the prior keystroke was consumed.
- fix: Make the delay configurable and bump the default, and/or post the Return with `CGEventSourceSetLocalEventsSuppressionInterval` plus an explicit modifier-clear keystroke. At minimum, document why these specific values and expose them in config so users on slow apps can raise them.

---

### [MEDIUM] `force_paste` (⌃⌥V) clobbers clipboard and never restores; appends a trailing space unconditionally
- file: src/always/keyboard.rs:390-404
- category: data-loss
- confidence: high
- impact: The force-paste hotkey copies the last filtered transcript (with a hard-coded trailing space) and pastes it, again with no clipboard save/restore — same clipboard-destruction problem as the main path, plus a spurious trailing space the user did not ask for (problematic in code/identifiers/URLs).
- problem:
  ```rust
  if let Err(error) = paste::copy_to_clipboard(format!("{} ", text))
      .and_then(|_| paste::paste_text(pause::is_auto_enter_enabled()))
  ```
  `format!("{} ", text)` always appends a space; clipboard is overwritten with no restore.
- fix: Reuse the shared save/restore paste path (per CRITICAL fix). Only append the trailing space when `auto_enter`/word-mode semantics call for it, matching the main path's `paste_clipboard` logic instead of unconditionally.

---

### [MEDIUM] `chars` count in force-paste log uses byte length, not character count
- file: src/always/keyboard.rs:391
- category: correctness
- confidence: high
- impact: Misleading diagnostics for unicode/multibyte transcripts (emoji, accented text, CJK). Not a paste bug per se, but the variable is literally named `chars` and reports bytes — will mislead anyone debugging a paste-length issue with non-ASCII text.
- problem:
  ```rust
  let chars = text.len();           // byte length, not char count
  tracing::info!(chars, "force_paste_filtered");
  ```
- fix: `let chars = text.chars().count();`

---

### [MEDIUM] Passive clipboard watcher `read_clipboard` errors are swallowed in a tight 250ms poll loop
- file: src/always/clipboard_watcher.rs:55-67
- category: error-handling
- confidence: medium
- impact: If `correction::read_clipboard()` starts failing persistently (pbpaste broken, sandbox/permission change), the loop spins forever every 250ms doing nothing and logging nothing — a silent no-op that's hard to diagnose, and a steady background wakeup/CPU cost on an always-on daemon.
- problem:
  ```rust
  let current = match correction::read_clipboard() {
      Ok(s) => s,
      Err(_) => continue,     // silent, no backoff, no log
  };
  ```
  The error is discarded with no rate-limited warning and no backoff; the `interval` keeps firing at 250ms regardless.
- fix: Log the error at warn with a rate limiter (e.g. once per N failures), and back off the poll interval after repeated consecutive failures so a broken pbpaste doesn't keep the daemon busy-looping.

---

### [LOW] Clipboard polling at 250ms is a permanent background wakeup even when nothing is dictated
- file: src/always/clipboard_watcher.rs:35,58-59
- category: performance
- confidence: medium
- impact: When `passive_correction_capture` is enabled, the daemon shells out to `pbpaste` (a process spawn) 4×/second for the entire lifetime of the always-on daemon, even when the user hasn't dictated in hours. That's ~345k process spawns/day and prevents the CPU from idling — meaningful battery cost on a laptop.
- problem: `const POLL_INTERVAL: Duration = Duration::from_millis(250);` drives an unconditional `interval.tick()` loop that reads the clipboard every tick regardless of whether a paste recently occurred. The `take_last_pasted_within(PASTE_WINDOW)` check happens *after* the read, so the expensive pbpaste spawn runs even when there's been no paste in the last 60s.
- fix: Gate the clipboard read on "a paste happened within PASTE_WINDOW" *first* (cheap in-memory check) and skip `read_clipboard()` entirely outside that window; or use NSPasteboard `changeCount` (a cheap integer read) to detect changes instead of spawning pbpaste each tick.

---

### [LOW] `replace_via_undo` issues Cmd+Z into whatever app is focused, with no focus/age guard at the call boundary
- file: src/always/paste.rs:110-116 (callers: event_loop.rs grammar-patch path, correction.rs)
- category: correctness
- confidence: medium
- impact: If focus changed between the original paste and the grammar patch, `replace_via_undo` sends Cmd+Z to the *new* app (undoing something unrelated there), then pastes the corrected text into it. `GRAMMAR_PATCH_MAX_AGE` (4s) bounds time but not focus.
- problem:
  ```rust
  pub fn replace_via_undo(text: &str) -> Result<()> {
      undo_last_paste()?;          // Cmd+Z to current focus, no focus check
      copy_to_clipboard(text.to_string())?;
      paste_text(false)
  }
  ```
  `undo_last_paste` has the comment "we only get here while the user is parked in the same field" but nothing enforces that.
- fix: Re-validate frontmost app/window equals the snapshot taken at original paste time before issuing the undo; abort the patch if focus moved. Also applies the clipboard save/restore concern (it overwrites clipboard with the corrected text and never restores).
