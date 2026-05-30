# Audit: daemon-core scope

Scope: src/always/daemon.rs, src/always/pause.rs, src/always/focus_state.rs
Lens: lifecycle, shared state, signal handling, lock/atomic correctness, panics in spawned tasks, graceful termination, double-start. Special attention to the uncommitted PidGuard race-fix in daemon.rs.

Evidence basis: all three assigned files were read in full (byte-identical across repeated reads). The uncommitted daemon.rs diff (PidGuard race-fix) was read identically multiple times. Relevant call sites in src/always/event_loop.rs (`run()` at line 32) and src/always/uds_server.rs (orphan watchdog at line 190, focus handler at line 544) were read directly. Confirmed `install_signal_handlers` (event_loop.rs:43) and `reconcile_duplicate_processes` (event_loop.rs:142 and :492) ARE wired up — NOT dead code. Confirmed `focus_state` is referenced in exactly two spots repo-wide — `mod.rs:14` (module decl) and `focus_state.rs:56` (inside its own body) — so `save()`/`load()`/`restore_on_startup()` have zero external callers.

---

### [HIGH] PID file is truncated before the flock is acquired, and a leaked empty PID file remains on the post-lock bail paths
- file: src/always/daemon.rs:333
- category: concurrency
- confidence: high
- impact: (1) While a second daemon races startup, the on-disk PID is zeroed before that process blocks on the lock; during that window `read_pid()`/`is_running()` report "not running" even though the canonical daemon is alive. (2) When `install()` bails after the `open` — at the post-lock socket recheck (line 350) or if `write!` fails (line 374) — the truncated, never-written PID file is left on disk as a zero-byte file. `read_pid()` then parses `""` → `None`, so `is_running()` stays false until something rewrites it; the next `always start` will believe no daemon exists and may spawn a duplicate (the exact double-mic/double-transcript failure this code is trying to prevent).
- problem: The file is opened with `truncate(true)` *before* the lock is acquired and long before the PID is written (line 374). The comment at line 346 claims "The pid file we just truncated gets cleaned up when `file` drops on Err." That is false — dropping a `std::fs::File` only closes the fd; it does not `remove_file`.
```rust
let mut file = OpenOptions::new()
    .write(true)
    .create(true)
    .truncate(true)        // truncates NOW, before flock + before all the rechecks
    .open(&path)
    .context("failed to open always PID file")?;
acquire_exclusive_lock(&file)
    .context("Always-on daemon already running (pid lock held)")?;
// ... post-lock socket recheck below can bail!(), leaving a 0-byte pid file
// ... write!(file, "{}", std::process::id())  is only at line 374
```
- fix: Open with `.truncate(false)`; acquire the flock; then immediately before the write do `use std::io::{Seek, SeekFrom}; file.set_len(0)?; file.seek(SeekFrom::Start(0))?; write!(file, "{}", std::process::id())?;`. On every error return after `open` but before the successful write, explicitly `let _ = std::fs::remove_file(&path);`. Truncating only after the lock + after all rechecks pass removes both the "looks dead" visibility window and the leaked empty-file.

---

### [HIGH] `focus_state::save()` and `restore_on_startup()` are dead — the "restore per-app pause across restarts" feature never runs
- file: src/always/focus_state.rs:22
- category: correctness
- confidence: high
- impact: The module's stated purpose ("Persist the last focused app bundle so daemon restarts can restore per-app allowlist state without waiting for a focus-change event") does not work. `save()` is never called, so the persisted JSON is always absent/stale; `restore_on_startup()` is never called at boot, and even if it were it would early-return because `load()` is `None`. After a daemon restart, `CURRENT_APP` is `None` until the Swift app pushes a focus event, and with the per-app fallback being paused-by-default (`compute_effective` → `effective_paused_for_current_app` for an unknown app), the user perceives "voice typing stopped working right after restart" until they refocus the target app.
- problem: `save()` (line 22) and `restore_on_startup()` (line 45) are `pub` with zero call sites. Repo-wide grep shows `focus_state` appears only at `mod.rs:14` (the `pub mod` declaration) and `focus_state.rs:56` (inside its own body). The focus-change handler that calls `pause::set_current_app_and_recompute` (uds_server.rs:544) does NOT also persist via `focus_state::save`, and `event_loop::run` never calls `restore_on_startup()`.
```rust
pub fn restore_on_startup() {
    let Some(bundle) = load() else { return; };   // always returns here: nothing ever called save()
    ...
}
```
- fix: In the focus-change handler (uds_server.rs around line 544, where `set_current_app_and_recompute` runs), also call `focus_state::save(bundle_id.as_deref())`. In `event_loop::run`, call `focus_state::restore_on_startup()` once during startup (after the broadcaster/UDS server are up — see next finding) and before audio gating begins. If the feature is genuinely abandoned, delete the module so it doesn't read as live.

---

### [MEDIUM] A corrupt/empty PID file is never cleaned up — `remove_stale_pid` can't reach it because `read_pid()` collapses "empty" into "absent"
- file: src/always/daemon.rs:413
- category: error-handling
- confidence: high
- impact: A zero-byte or garbage PID file (reachable via the HIGH finding above, and also on a crash mid-write) makes `read_pid()` return `None`. `remove_stale_pid()` only unlinks when it can parse a PID that is not running, so it never removes a corrupt file; the stale empty file persists indefinitely.
- problem:
```rust
pub fn read_pid() -> Option<u32> {
    std::fs::read_to_string(pid_path()).ok()
        .and_then(|s| s.trim().parse().ok())   // empty/garbage → None, same as "file absent"
}
fn remove_stale_pid() {
    if read_pid().is_some_and(|pid| !process_is_running(pid)) {  // never true for a corrupt file
        let _ = std::fs::remove_file(pid_path());
    }
}
```
- fix: In `remove_stale_pid`, treat a present-but-unparseable file as stale: if `pid_path()` exists and parsing fails, unlink it. The flock is the real source of truth; the PID file is advisory, so removing a corrupt one is always safe.

---

### [MEDIUM] If wired up, `restore_on_startup` broadcasts Paused/Resumed that can be lost if it runs before UDS clients subscribe
- file: src/always/focus_state.rs:49
- category: correctness
- confidence: medium
- impact: `restore_on_startup` mutates global pause state and then broadcasts `paused_quietly()/resumed_quietly()`. In `event_loop::run`, the UDS server is spawned async (line 62) and clients connect later. If a future fix calls `restore_on_startup()` at the wrong point, the restored Paused/Resumed event is broadcast to zero subscribers and the Swift overlay starts out of sync with the daemon's actual effective-pause state.
- problem:
```rust
let (effective, changed) = crate::always::pause::set_current_app_and_recompute(Some(bundle));
if changed {
    if effective { ...paused_quietly(); } else { ...resumed_quietly(); }
}
```
- fix: When wiring it, call `restore_on_startup()` after the broadcaster is initialized and ideally after the first client connects, OR have the UDS server emit current effective-pause state on each new client connection (handle_client initial-state) so a late subscriber always re-syncs regardless of restore timing.

---

### [MEDIUM] `EFFECTIVE_PAUSED` is a cache that can silently go stale when per-app overrides change without a recompute
- file: src/always/pause.rs:139
- category: correctness
- confidence: medium
- impact: `is_paused()` returns the cached `EFFECTIVE_PAUSED`, refreshed only when someone calls `recompute_effective()`. Its per-app component (`effective_paused_for_current_app`) can change out-of-band when the allowlist/per-app cache is mutated (e.g. the user toggles an app's allow state via Settings). If that mutation path doesn't call `recompute_effective()`, the audio pipeline keeps listening in an app the user just disabled (or stays paused in one they just enabled) until an unrelated event (focus change, manual toggle, idle tick) forces a recompute. For an always-on dictation daemon, listening in a just-disabled app is a privacy-relevant surprise.
- problem: The contract (lines 133-141) warns callers "must call `recompute_effective()` first," but nothing structurally enforces it on the per-app override write path:
```rust
pub fn is_paused() -> bool {
    EFFECTIVE_PAUSED.load(Ordering::Relaxed)   // cached, not live
}
```
- fix: `compute_effective()` is cheap (two relaxed atomic loads plus the per-app lookup). Either make `is_paused()` compute live and drop the `EFFECTIVE_PAUSED` cache, or guarantee every per-app override mutation calls `recompute_effective()` and broadcasts on `changed`.

---

### [LOW] `install_signal_handlers` silently skips SIGINT if SIGTERM registration fails — one failure disables all graceful shutdown
- file: src/always/daemon.rs:441
- category: error-handling
- confidence: medium
- impact: If `signal(SignalKind::terminate())` errors, the function logs a warning and `return`s, so the SIGINT handler is never installed either and no graceful-shutdown handler exists at all. The daemon then dies on the default disposition without running cleanup, leaving stale pid/socket files. Both registrations are coupled into a single all-or-nothing path.
- problem:
```rust
let mut term = match signal(SignalKind::terminate()) {
    Ok(s) => s,
    Err(e) => { tracing::warn!(error = %e, "failed_to_install_sigterm_handler"); return; }  // also skips SIGINT
};
let mut int = match signal(SignalKind::interrupt()) { ... };
```
- fix: Install whichever handlers succeed independently and `select!` only over the ones that registered, rather than bailing out of both on the first error. Minor, since `signal()` rarely fails, but the coupling means one failure disables all graceful shutdown.

---

### [LOW] Cross-thread pause/idle/auto-enter flags use `Ordering::Relaxed` while being treated as a coherent snapshot
- file: src/always/pause.rs:41
- category: concurrency
- confidence: low
- impact: `recompute_effective` reads MASTER/IDLE and `swap`s EFFECTIVE all with `Relaxed`; `is_paused()` loads with `Relaxed` from the audio thread. For independent booleans the worst case is a one-iteration-late pause/resume (benign on x86/arm64). The latent risk is only if future code makes one flag's value need to be visible *before* EFFECTIVE flips — Relaxed gives no such guarantee.
- problem: All loads/stores are `Ordering::Relaxed` (e.g. lines 41, 46, 49, 140, 156, 163, 327, 331).
- fix: Low priority. If you want the snapshot guarantee, use `Release` on the EFFECTIVE swap + `Acquire` on the `is_paused()` load, or document that the flags are deliberately independent.

---

### [LOW] Lifecycle helpers shell out to `kill`/`ps` per call, forking heavily on the hot startup polling path
- file: src/always/daemon.rs:473
- category: performance
- confidence: high
- impact: `process_is_running` (`kill -0`), `list_daemon_pids` (`ps -eo`), and the kill paths each spawn a child process. `wait_until_no_daemon_processes` polls `list_daemon_pids()` (a full `ps` fork) every 50ms for up to 5s = up to ~100 `ps` forks per startup, plus per-pid `kill` forks. Wasteful for an always-on background daemon, and the `ps`-argv parse is fragile for commands containing whitespace.
- problem:
```rust
fn process_is_running(pid: u32) -> bool {
    std::process::Command::new("kill").arg("-0").arg(pid.to_string()).output()
        .is_ok_and(|o| o.status.success())
}
```
- fix: Use `libc::kill(pid as i32, 0)` and treat `errno == ESRCH` as "not running". `libc` is already a dependency (`flock` at lines 390/406), so no new crate is needed. Removes nearly all fork overhead in the startup loops.

---

## Notes (not defects)
- The PidGuard race-fix intent is sound and a genuine improvement: acquire the flock first, refuse-to-start on a live UDS socket, post-lock socket recheck, then sweep zombies (which by definition cannot hold the lock). This correctly closes the old "kill orphans → take lock" race that could murder a daemon still loading its model. The only correctness defect the rewrite introduces is the truncate-before-lock / leaked-empty-PID issue (HIGH above).
- `PidGuard::Drop` (lines 386-402) correctly unlocks (`flock LOCK_UN`) and removes pid + socket; cleanup is correct when Drop runs.
- `install_signal_handlers` IS called from `event_loop::run` (line 43), and `reconcile_duplicate_processes` IS called both periodically (every 30s, event_loop.rs:142) and pre-paste (event_loop.rs:492). These are correctly wired — not dead code. Note that `std::process::exit(0)` in the signal handler (line 469) does NOT itself run `PidGuard::Drop`; the handler therefore duplicates the pid/socket unlink inline (lines 465-468), and the UDS orphan-watchdog does the same (uds_server.rs:205-213). That duplication is intentional and correct given `exit()` skips Drop.
- `is_daemon_run_command` correctly rejects diagnostic/`status` commands (unit test at daemon.rs:495), avoiding self-kill of a `ps | rg` pipeline. Good.
- `focus_state` JSON read/write is best-effort and silently swallows errors; acceptable for a non-critical cache (the real defect is that it's never invoked — see HIGH above).
