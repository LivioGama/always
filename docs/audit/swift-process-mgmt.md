# Swift Process Management Audit — `process-mgmt`

Scope:
- `Always/Sources/Always/Services/CLIService.swift` (178 lines, read verbatim)
- `Always/Sources/Always/Services/SingleInstanceGuard.swift` (130 lines, read verbatim)

Caller context verified by reading `Always/Sources/Always/Always.swift` (AppDelegate lifecycle + `killStaleDaemon`/`listDaemonProcessIDs`/`isHealthyDaemon`/`isDaemonSocketLive`) and grepping callers of `startDaemon`/`restartDaemon`. `restartDaemon()` is invoked from `StateMonitor.swift:108` (crash-recovery path); `startDaemon()` is invoked from `AppDelegate.applicationDidFinishLaunching` (line 174, after `killStaleDaemon`) and `AppDelegate.reconcileDuplicateDaemons` (line 286, after `killStaleDaemon`).

Design note (NOT a bug): `applicationWillTerminate` (Always.swift:243-249) intentionally leaves the daemon running so the next GUI launch attaches over the existing UDS socket. Orphan daemons are reaped on next launch via the `ps` sweep in `killStaleDaemon`. Findings below are scoped to genuine defects, with caller mitigations accounted for.

---

### [HIGH] `runCLI` reads pipe AFTER `waitUntilExit()` — deadlock on any verbose subcommand
- file: Always/Sources/Always/Services/CLIService.swift:153
- category: concurrency
- confidence: high
- impact: Any CLI subcommand whose combined stdout+stderr exceeds the OS pipe buffer (~16-64 KB) blocks the child forever on `write()`; the parent is parked in `waitUntilExit()` that never returns, hanging the calling async Task indefinitely. Affects `status`, `config show`, `get-state`, `stop`, `toggle-pause`, `toggle-auto-enter` — i.e. every Settings/menu action that touches the daemon.
- problem: Classic Process pipe-deadlock ordering — wait first, then drain:
  ```swift
  try process.run()
  process.waitUntilExit()

  let data = pipe.fileHandleForReading.readDataToEndOfFile()
  ```
  Both `standardOutput` and `standardError` feed the **same** `Pipe` (lines 150-151), doubling the fill rate. If the child emits more than the pipe buffer before exiting, it blocks on write, never exits, and `waitUntilExit()` blocks the parent. Because `runCLI` is `async` but `waitUntilExit()` is a synchronous blocking call, this also pins a thread of the Swift concurrency cooperative pool while it hangs.
- fix: Drain the pipe before/while waiting:
  ```swift
  try process.run()
  let data = pipe.fileHandleForReading.readDataToEndOfFile()  // blocks until child closes the write end (EOF on exit)
  process.waitUntilExit()
  ```
  Additionally, move the blocking call off the cooperative pool — wrap in `withCheckedThrowingContinuation` + `process.terminationHandler`, or run inside `Task.detached`, so a slow/hung daemon subcommand cannot starve Swift concurrency.

---

### [MEDIUM] flock-on-open failure silently *allows* a second instance to launch (fail-open mutex)
- file: Always/Sources/Always/Services/SingleInstanceGuard.swift:95
- category: correctness
- confidence: high
- impact: If `open()` of the lock file fails (permissions on `~/Library/Application Support/always`, disk full, sandbox/container quirk, immutable flag), the guard returns `true` ("you may run") for *every* launch. Since the failure is environmental and identical across instances, single-instance protection collapses entirely — multiple menu-bar icons and, via the launch path, multiple daemons.
- problem:
  ```swift
  lockFD = open(lockPath, O_CREAT | O_RDWR, 0o600)
  guard lockFD >= 0 else {
      Self.logger.error("Could not open GUI lock file — allowing launch")
      return true
  }
  ```
  "Fail open" is the wrong default for a mutual-exclusion primitive.
- fix: Fall back to the process sweep as source of truth instead of unconditionally allowing:
  ```swift
  guard lockFD >= 0 else {
      Self.logger.error("Could not open GUI lock file — falling back to process sweep")
      return Self.listGUIProcessIDs(except: ProcessInfo.processInfo.processIdentifier).isEmpty
  }
  ```
  This still lets the first instance run while blocking later ones when flock is unavailable.

---

### [MEDIUM] `startDaemon` does nothing for 15s when a dead/zombie daemon PID exists with no live socket
- file: Always/Sources/Always/Services/CLIService.swift:38
- category: error-handling
- confidence: medium
- impact: A wedged daemon that holds a PID but never binds the UDS socket (crashed mid-startup, stuck on mic permission, hung) makes `startDaemon` take the no-op `else` branch and then busy-wait the full 15s before throwing. The launch path (AppDelegate:170) kills first so it is usually safe there, but `reconcileDuplicateDaemons` and any direct `startDaemon` caller hitting this branch will stall 15s and fail with no self-recovery (no kill+respawn). Voice-to-text stays dead until manual restart.
- problem:
  ```swift
  if AppDelegate.listDaemonProcessIDs().isEmpty {
      try spawnDetachedRunProcess()
  } else {
      Self.logger.warning("Daemon process without live socket — waiting for UDS bind")
  }
  let deadline = Date().addingTimeInterval(15)
  while Date() < deadline { ... }
  throw CLIError.commandFailed("Always-on daemon failed to start")
  ```
  The `else` branch only logs, then falls into a 15s wait that a stuck process will never satisfy. `startDaemon` cannot escalate to kill-and-respawn on its own.
- fix: After a short grace period with a PID present but no live socket, treat it as stale and recover in-place: call `AppDelegate.killStaleDaemon()` then `spawnDetachedRunProcess()` rather than waiting the full window for a process that will never bind.

---

### [MEDIUM] `acquireOrHandOff` kills duplicate GUIs but not the daemons they may have already spawned
- file: Always/Sources/Always/Services/SingleInstanceGuard.swift:37
- category: resource-leak
- confidence: medium
- impact: Two copies launched near-simultaneously: the lock loser may have already run `applicationDidFinishLaunching` → `startDaemon()` → `spawnDetachedRunProcess()` before the winner SIGTERMs it. The spawned daemon is detached and started with `ALWAYS_SKIP_ORPHAN_KILL=1` (CLIService:86), so killing the loser GUI does not kill the daemon it spawned — leaving a duplicate `always-daemon` racing for the socket (which the code elsewhere notes causes double-paste).
- problem: In `acquireOrHandOff`:
  ```swift
  if tryAcquireLock() {
      terminateOtherGUIProcesses(except: ProcessInfo.processInfo.processIdentifier)
      ...
      return true
  }
  ```
  `terminateOtherGUIProcesses` only sweeps GUI PIDs (`isAlwaysGUIProcess` explicitly excludes `always-daemon`, line 84). The winner relies on the later `reconcileDuplicateDaemons` to clean up, but that only fires if `>1` daemon is already visible at that exact moment — a duplicate spawned a beat later by a dying loser can slip through.
- fix: After winning the lock, have the surviving GUI also reconcile daemons (call `AppDelegate.killStaleDaemon()` once before its own `startDaemon`, which the launch path already does at AppDelegate:170 — so the real gap is ensuring the *guard* runs before any daemon spawn and that a single authoritative reconcile happens after the GUI sweep). At minimum, document/guarantee guard acquisition strictly precedes daemon bootstrap.

---

### [LOW] `listGUIProcessIDs` swallows `ps` launch failure via `try?`, indistinguishable from "no instances"
- file: Always/Sources/Always/Services/SingleInstanceGuard.swift:63
- category: error-handling
- confidence: medium
- impact: If `/bin/ps` fails to spawn, `try?` discards the error and the function returns `[]`. Callers then believe there are no other GUI instances: `terminateOtherGUIProcesses` does nothing, `activateExistingInstance` hand-off no-ops, and the flock-failure fallback I propose above would wrongly allow a launch.
- problem:
  ```swift
  try? proc.run()
  let data = pipe.fileHandleForReading.readDataToEndOfFile()
  proc.waitUntilExit()
  ```
  Read-before-wait ordering here is correct (good), but `try?` makes a `ps` spawn failure look identical to an empty process list. Matching is also brittle: `isAlwaysGUIProcess` uses `command.hasSuffix("/Always")` (line 87), which can match unrelated binaries named `Always`.
- fix: Log/propagate the `ps` launch failure instead of `try?`-swallowing, and tighten matching to the full bundle MacOS path rather than a bare `/Always` suffix.

---

### [LOW] `restartDaemon` uses a fixed 300ms sleep between kill and start — racy on slow teardown
- file: Always/Sources/Always/Services/CLIService.swift:97
- category: correctness
- confidence: low
- impact: Called from `StateMonitor.swift:108` for crash recovery. If the killed daemon takes longer than 300ms to exit and release the UDS socket, the subsequent `startDaemon` can see the old PID still present (hitting the no-op `else` branch at CLIService:41) and then stall 15s, making crash recovery unreliable on a busy/slow machine.
- problem:
  ```swift
  AppDelegate.killStaleDaemon()
  try await Task.sleep(nanoseconds: 300_000_000)
  return try await startDaemon()
  ```
  A blind fixed delay assumes teardown completes in 300ms with no confirmation the old process is gone or the socket file removed.
- fix: Poll (bounded) until `AppDelegate.listDaemonProcessIDs()` is empty and the socket no longer accepts a connection before calling `startDaemon`, instead of a fixed sleep.

---

### [LOW] 15s startup wait polls socket every 10ms and ignores Task cancellation
- file: Always/Sources/Always/Services/CLIService.swift:44
- category: performance
- confidence: medium
- impact: Up to ~1,500 `isDaemonSocketLive()` probes (each a socket()/connect()/close syscall sequence, per Always.swift:298) over 15s. The loop never checks `Task.isCancelled`, so if the user quits during launch the bootstrap Task keeps probing for the full window.
- problem:
  ```swift
  while Date() < deadline {
      if AppDelegate.isDaemonSocketLive() { return "Always-on daemon ready." }
      try await Task.sleep(nanoseconds: 10_000_000) // 10ms — too tight for a multi-second startup
  }
  ```
- fix: Back the poll off (e.g. 50-100ms) and honor cancellation:
  ```swift
  while Date() < deadline {
      try Task.checkCancellation()
      if AppDelegate.isDaemonSocketLive() { return "Always-on daemon ready." }
      try await Task.sleep(nanoseconds: 50_000_000)
  }
  ```

---

### [LOW] Dev fallback binary path is `target/release/always`, but local dev profile is debug and bundled name is `always-daemon`
- file: Always/Sources/Always/Services/CLIService.swift:28
- category: maintainability
- confidence: medium
- impact: When the bundled `always-daemon` is absent (dev runs), the fallback resolves only to `target/release/always`. The project's CLAUDE.md states local dev uses the **debug** profile (`target/debug/always`); so this fallback may launch a stale release binary or hit `binaryNotFound` when only a debug build exists, confusing local debugging.
- problem:
  ```swift
  cliPath = projectPath.appendingPathComponent("target/release/always").path
  ```
  Hardcodes the release profile while the documented dev profile is debug.
- fix: Probe both profiles newest-first (mirror `build.sh`'s "newest of release/debug" logic): check `target/debug/always` and `target/release/always`, pick whichever exists / is most recent, and log the selection.
