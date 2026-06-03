# IPC / UDS Audit — `ipc-uds` scope

Files audited (all read in full):
- `src/always/uds_server.rs`
- `src/always/json_listener.rs`
- `src/always/event.rs`
- (cross-ref) `src/always/daemon.rs` socket helpers, `Always/Sources/Always/UDSClient.swift` / `DaemonProtocol.swift` (Swift side; reads were intermittently failing in this session — Swift-contract findings flagged as medium/low confidence where I could not re-open the file).

---

### [HIGH] Slow/stuck UDS client blocks broadcast writer and silently stalls the per-client event stream
- file: src/always/uds_server.rs:410
- category: concurrency
- confidence: high
- impact: A single client (the Swift app) that stops reading (hung UI thread, suspended app, full kernel socket buffer) blocks the per-connection writer task on `write_all`/`flush`. Because every connection has its own `broadcast::Receiver`, that receiver stops draining and overflows the bounded channel (capacity 100); on overflow `recv()` returns `Lagged` and the `while let Ok(...)` loop **terminates the loop entirely**, permanently killing the event stream to that client without disconnecting it.
- problem: The broadcast loop bails on the first non-`Ok` result:
  ```rust
  while let Ok(event) = rx.recv().await {
      ...
      if let Err(e) = writer.write_all(json_line.as_bytes()).await { ... break; }
      if let Err(e) = writer.flush().await { ... break; }
  }
  ```
  `broadcast::Receiver::recv()` returns `Err(RecvError::Lagged(n))` when a slow consumer falls behind, and `Err(RecvError::Closed)` only when all senders drop. `while let Ok(...)` treats `Lagged` identically to `Closed` and exits the loop. A slow client that briefly falls >100 events behind (easy during a model download progress storm) loses its event stream forever, even though the connection is still open — the overlay/menu-bar then silently stops updating until the user restarts the app. Also, `write_all`/`flush` have no timeout, so a wedged client holds the writer task indefinitely.
- fix: Distinguish `Lagged` (recover, keep going) from `Closed` (exit) and wrap the write in a timeout so a dead client is dropped instead of hanging:
  ```rust
  loop {
      let event = match rx.recv().await {
          Ok(ev) => ev,
          Err(broadcast::error::RecvError::Lagged(n)) => {
              tracing::warn!(skipped = n, "uds_client_lagged");
              continue; // keep serving; optionally push a resync snapshot
          }
          Err(broadcast::error::RecvError::Closed) => break,
      };
      let json_line = match event.to_json_line() { Ok(l) => l, Err(_) => continue };
      match tokio::time::timeout(Duration::from_secs(5),
              async { writer.write_all(json_line.as_bytes()).await?; writer.flush().await })
              .await {
          Ok(Ok(())) => {}
          _ => { tracing::warn!("uds_slow_client_dropped"); break; }
      }
  }
  ```

---

### [HIGH] Initial-state burst is written without a write timeout and before the reader task starts — a client that never reads wedges the handler at connect
- file: src/always/uds_server.rs:349
- category: concurrency
- confidence: medium
- impact: If a client connects but its socket receive buffer is full / it never reads (e.g. crashed mid-handshake, or a non-protocol probe like `socket_is_live`'s connect-and-drop), `writer.write_all(initial_payload)` / `flush()` can block the spawned `handle_client` task indefinitely. These tasks accumulate (one per accept) and each holds a `ClientGuard` keeping `CONNECTED_CLIENTS > 0`, which defeats the orphan watchdog (it will never see 0 clients) so the daemon never self-exits.
- problem:
  ```rust
  if let Err(e) = writer.write_all(initial_payload.as_bytes()).await { ... return Ok(()); }
  if let Err(e) = writer.flush().await { ... return Ok(()); }
  ```
  No timeout; a stuck write here pins the task and the client count. Note `socket_is_live()` in `daemon.rs` does exactly `UnixStream::connect(path)` and immediately drops — a benign connect that increments `CONNECTED_CLIENTS`, triggers the full Hello+state burst, then RSTs. Normally the write fails fast, but a probe that connects and lingers (or many rapid probes) inflates the client count and can race the orphan watchdog.
- fix: Wrap initial-state and all subsequent writes in `tokio::time::timeout(...)`. Additionally consider only counting a client as "connected" after a successful Hello write, so connect-probes don't reset the orphan timer.

---

### [HIGH] No message-size bound on the daemon→client direction; one giant event can balloon memory and stall every client
- file: src/always/uds_server.rs:411 / src/always/event.rs:224
- category: resource-leak
- confidence: medium
- impact: `MAX_COMMAND_LINE_LENGTH` (1KB) is enforced only on the inbound command path. Outbound events have no size cap. `DaemonEvent::ModelsList { models }`, `TranscriptFinal { text }`, `CorrectionDialogRequested { last_transcript }` and `ModelDownloadFailed { error }` carry arbitrary-length strings/vectors. A long dictation (`TranscriptFinal`) or a large catalog is serialized to a single JSON line broadcast to all clients; each receiver allocates and writes the whole blob. The Swift client also accumulates into an unbounded `receiveBuffer` (`UDSClient.swift:640,677`) until it sees a `\n`, so a huge event grows memory on both ends. Inbound, the daemon's `BufReader::read_line` has no cap either, so a client (or a corrupt/never-terminated frame) can make the daemon buffer an unbounded line in memory.
- problem: `to_json_line` simply does `serde_json::to_string(self)? + "\n"` with no length guard, and the broadcast loop writes it verbatim. There is no upper bound anywhere on event payload size.
- fix: Cap obviously-large fields before broadcasting (e.g. truncate `TranscriptFinal`/`CorrectionDialogRequested` text to a sane max for the overlay, send full text via a side channel if needed), and/or enforce a max serialized line length in `to_json_line` and skip+log oversized events rather than letting them through.

---

### [MEDIUM] Rate limiter is per-connection and useless against reconnect floods; also `Mutex` is pointless (single reader task)
- file: src/always/uds_server.rs:309, 385-392
- category: security
- confidence: high
- impact: The documented DoS protection ("Max 10 commands per second per client") is trivially bypassed: a client can open N connections (each gets its own fresh `RateLimiter`) and issue 10·N commands/sec. There is no global cap and no per-connection accept throttling, so the rate limit gives a false sense of protection. Separately, `rate_limiter` is a `tokio::sync::Mutex` but it is only ever locked from the single reader task spawned per connection — the lock is dead weight and adds an `.await` point on every command.
- problem:
  ```rust
  let rate_limiter = Mutex::new(RateLimiter::new()); // per connection
  ...
  let mut limiter = rate_limiter.lock().await;
  if !limiter.check() { ... continue; }
  ```
- fix: Make the limiter a process-global `AtomicU32` token bucket (or a `parking_lot::Mutex` static) so it bounds aggregate command rate across all connections, and/or cap `CONNECTED_CLIENTS` (reject `accept` when above a small N since only the Mac app should ever connect). Drop the per-connection `tokio::sync::Mutex` — a plain local `RateLimiter` (or `RefCell`) suffices since it is single-task-owned.

---

### [MEDIUM] Reader task is detached and never joined/aborted when the writer loop exits — half-open connection leak
- file: src/always/uds_server.rs:365, 410
- category: resource-leak
- confidence: high
- impact: `handle_client` spawns a detached reader task (`tokio::spawn(async move { ... reader ... })`) and then runs the writer loop. When the writer loop exits (client write error, `Lagged`, or `Closed`), `handle_client` returns and `_guard` drops decrementing the client count — but the spawned **reader task keeps running**, holding the read half of the split stream and a clone of `ctx`. It only ends on EOF/read-error. If the peer half-closes write but keeps the read side open (or vice versa), the reader lingers, still executing commands (mutating pause state, swapping transcribers) for a connection the daemon already considers "gone." Over many connect/disconnect cycles these orphaned reader tasks accumulate.
- problem: There is no `JoinHandle` retained and no `abort()` on the reader when the writer side ends; the two halves have independent lifetimes.
- fix: Keep the `JoinHandle` from the spawned reader and `reader_handle.abort()` after the writer loop breaks (or use `tokio::select!` to run reader+writer in the same task so either side ending tears down both). Decrement the client count only after both halves are done.

---

### [MEDIUM] Stale-socket cleanup has a connect-then-bind TOCTOU and treats any connect failure as "stale"
- file: src/always/uds_server.rs:160-170; src/always/daemon.rs:162
- category: correctness
- confidence: medium
- impact: Startup races: between `socket_is_live()` returning false (refused connect → "stale") and `remove_file` + `bind`, a second daemon could bind, after which this daemon deletes the now-live socket file and binds its own — both processes end up "running," fighting over the mic and producing duplicate transcripts (the exact failure the comment at line 158 says it is trying to prevent). Also `socket_is_live` decides "live vs stale" purely on `UnixStream::connect(path).is_ok()` (confirmed: `Ok(_) => true, Err(_) => false`, plain blocking connect, no timeout): a transient `ECONNREFUSED` due to a full listener backlog returns false → a *live* daemon's socket can get `remove_file`'d out from under it. A blocking `connect` with no timeout can also hang startup if the peer accepts but never proceeds.
- problem:
  ```rust
  if socket_path.exists() {
      if crate::always::daemon::socket_is_live(&socket_path) { bail!(...) }
      std::fs::remove_file(&socket_path)?;   // <-- gap before bind
  }
  let listener = UnixListener::bind(&socket_path)...;
  ```
  And `daemon::socket_is_live` (line 162) is a plain `UnixStream::connect(sock)` with no `connect_timeout`.
- fix: Use the pid file as the authority (already exists: `pid_path()`); only treat the socket as removable when the recorded pid is dead. Hold an exclusive lock (flock on the pid file) across the check+remove+bind window to close the TOCTOU. Use `UnixStream::connect_timeout` (or set a read timeout) in `socket_is_live` so a busy-but-live daemon isn't misclassified as stale and startup can't hang.

---

### [MEDIUM] `validate_command` rejects legitimate framing and double-parses; brace-depth heuristic is a poor proxy
- file: src/always/uds_server.rs:74-96, 379, 394
- category: correctness
- confidence: medium
- impact: `validate_command` requires the trimmed line to literally `starts_with('{')` and `ends_with('}')` and counts `{` chars to bound nesting. This (a) rejects any command whose JSON value legitimately contains `{`/`}` inside a string such that the brace count exceeds 10 (e.g. a `LogCorrection { intended }` or future command carrying user text with many braces), silently dropping the command; (b) the JSON is then parsed *again* by `DaemonCommand::from_json_line`, so the hand-rolled structural check duplicates serde's work while being less correct. Silent `continue` on validation failure means the Swift side gets no error/ack and the command just vanishes.
- problem:
  ```rust
  let brace_depth = trimmed.chars().filter(|&c| c == '{').count();
  if brace_depth > 10 { bail!("excessive nesting depth"); }
  ```
  Counting all `{` (including those inside string literals) is not nesting depth and can false-positive on benign payloads. On failure the reader does `continue` with no reply.
- fix: Drop the structural pre-check; rely on `serde_json::from_str` (already bounded by `MAX_COMMAND_LINE_LENGTH`, and serde has built-in recursion limits). If a real depth bound is wanted, use `serde_json`'s recursion limit / a `Deserializer` with `disable_recursion_limit` left off, not a char count. Consider sending a structured error event back so dropped commands are observable.

---

### [MEDIUM] Orphan watchdog calls `std::process::exit(0)` while other tasks may be mid-write — abrupt teardown can corrupt state
- file: src/always/uds_server.rs:190-215
- category: data-loss
- confidence: medium
- impact: The watchdog calls `std::process::exit(0)` directly. This does not run destructors (acknowledged in the comment for pid/socket), but it also kills any in-flight work with no synchronization: a concurrent `switch_active_backend` writing the prefs DB (`crate::db::set_preference`), a model download writing a partial file, or a logger mid-write can be torn down mid-operation. SQLite writes interrupted by `process::exit` can leave the prefs DB in a `-journal`/`-wal` state; partial model files are left without the registry knowing. The exit is decided solely on "no UDS clients for 120s," which also fires during legitimate long offline periods where the app is simply backgrounded.
- problem:
  ```rust
  log.write(crate::always::log::Event::Stop);
  let _ = std::fs::remove_file(crate::always::daemon::pid_path());
  if let Some(sock) = ... { let _ = std::fs::remove_file(sock); }
  std::process::exit(0);
  ```
- fix: Signal a graceful-shutdown channel instead of `process::exit` from inside the watchdog: let the main `start_server` accept loop break, await outstanding DB/download tasks (or at least a short drain), then exit. At minimum, guard DB writes so they are atomic/committed and won't be observed half-applied after an abrupt exit.

---

### [MEDIUM] Version handshake is one-directional: daemon never validates the client's version, and the Swift client only checks Hello *after* the daemon has already streamed full state + accepted commands
- file: src/always/uds_server.rs:319-359, 394-396; Always/Sources/Always/Services/UDSClient.swift:742-756
- category: correctness
- confidence: high
- impact: The Swift client DOES enforce the version (it disconnects on mismatch — `UDSClient.swift:744`), so this is not "unenforced." But the negotiation is one-sided and racy: (a) the daemon **never reads** any client version, so an *older* daemon paired with a *newer* app accepts commands it half-understands — a newer `DaemonCommand` variant deserializes to a `from_json_line` error and is silently dropped (`uds_server.rs:396` logs and `continue`s), so a Settings action just "does nothing" with no feedback to the user; (b) the daemon sends Hello in the same best-effort loop as the rest of initial state (`if let Ok(json_line) = event.to_json_line()`), and the broadcast loop begins immediately — so by the time the client reads Hello, parses it, and disconnects on mismatch, the daemon may already have broadcast events and the client's reader may already have dispatched some of them. There is no daemon-side gate that withholds non-Hello traffic until the client acks a compatible version.
- problem: Hello is assembled and sent alongside the rest of initial state with no enforcement that it succeeds or that the client agrees before more frames flow; unknown commands from a newer client vanish silently:
  ```rust
  for event in initial_events {              // Hello is just one of these
      if let Ok(json_line) = event.to_json_line() { initial_payload.push_str(&json_line); }
  }
  ...
  Err(e) => tracing::error!(error = %e, "uds_parse_command_failed: ..."), // unknown cmd dropped
  ```
- fix: Have the daemon read the client's claimed version as the first inbound line and refuse incompatible versions with an explicit `version_mismatch` event before processing any command. Send Hello as a standalone, must-succeed write. For unknown/failed commands, emit a structured error event back so the client can surface "this build can't do that" instead of silent no-op.

---

### [LOW] `send_event` / broadcaster ignore send failures; no observability when there are zero subscribers
- file: src/always/event.rs:351-353, 896-898 (uds_server)
- category: error-handling
- confidence: high
- impact: `EventBroadcaster::send` does `let _ = self.tx.send(event);`. `broadcast::Sender::send` returns `Err(SendError)` only when there are no receivers — which is the normal "no client connected" case — so swallowing it is acceptable, but it means events emitted while no GUI is connected are silently lost (e.g. `CorrectionLogged` fired from the hotkey path with the app closed). For corrections in particular this is real state lost to the user with no trace.
- problem: `let _ = self.tx.send(event);` — no count of dropped/undelivered events, no buffering for late subscribers.
- fix: For user-meaningful, low-frequency events (corrections, active-backend change) consider persisting/queueing so a client that connects shortly after still learns about them, or at minimum `trace!` the no-receiver case so it is diagnosable. (Low because most events are transient UI state.)

---

### [LOW] `json_listener.rs` propagates fatal errors out of an infinite loop, killing the JSON-mode process on a single transient transcribe failure
- file: src/always/json_listener.rs:19-21
- category: error-handling
- confidence: medium
- impact: In `--json` mode the loop does `record_utterance(...).context(...)?` — any error (a transient network blip to Groq, a temporary audio device error) propagates out of `listen_json`, terminating the long-running JSON listener process entirely. For an "always-on" integration consumer this means one hiccup ends the stream and no further JSON lines are emitted.
- problem:
  ```rust
  loop {
      match vad::record_utterance(cfg, &mut log).context("failed to record/transcribe utterance")? {
          ...
      }
  }
  ```
  The `?` inside the infinite loop converts a recoverable per-utterance failure into process death.
- fix: Catch the error per iteration, emit a JSON `{"type":"error", ...}` line, log it, and continue the loop; only break on truly unrecoverable conditions (e.g. audio device permanently gone).

---

### [MEDIUM] Swift client writes commands with a single blocking `send()` and no short-write loop — large/back-pressured commands can be truncated, desyncing the daemon's line parser
- file: Always/Sources/Always/Services/UDSClient.swift:814-827 (writer); src/always/uds_server.rs:370 (daemon reader)
- category: correctness
- confidence: medium
- impact: This is the producer side of the protocol contract the daemon's `read_line` depends on. `writeLine` does one `send(socketFD, ..., count, 0)` and only checks for `< 0`; it ignores a short write (`0 <= bytesWritten < count`). On a back-pressured socket `send` can write fewer bytes than requested. A truncated command line (no trailing `\n`, or split mid-JSON) makes the daemon's `BufReader::read_line` either block waiting for the rest or, once more bytes arrive, glue two commands together — both fail `validate_command`/parse and are silently dropped (`uds_server.rs:379/396`). Commands like `ApplyRuntimePreferences` and `SetAppPaused` carry enough JSON to be at risk under load. There is no resync/NAK, so the desync is invisible.
- problem:
  ```swift
  let bytesWritten = data.withUnsafeBytes { buffer in send(socketFD, buffer.baseAddress!, buffer.count, 0) }
  if bytesWritten < 0 { ... } else { logger.debug("Sent command: \(commandType)") }  // short write ignored
  ```
- fix: Loop until all bytes are written (advancing the pointer by `bytesWritten` until the full length is sent), handling `EAGAIN`/`EINTR`. On the daemon side, treat a parse failure as recoverable (it already does) but additionally bound the inbound line accumulation so a never-terminated line can't grow unbounded in `BufReader` (see size-bound finding).

---

### [LOW] Line-delimited framing has no escaping guarantee for embedded newlines on the inbound path
- file: src/always/uds_server.rs:368-376; src/always/event.rs:332
- category: correctness
- confidence: low
- impact: The protocol is newline-delimited JSON (`read_line` splits on `\n`). `serde_json::to_string` never emits a raw newline inside a string (it escapes `\n` as `\n`), so the outbound side is safe; the Swift writer uses `JSONEncoder` which likewise escapes newlines, so the inbound side is safe under normal operation. The residual gap is the same as the truncation finding: any frame that fails to parse is dropped with a silent `continue` and no NAK, so a desync (from a short write, a stray byte, or a future buggy client) is invisible.
- problem: Framing relies on the producer never emitting unescaped `\n`; there is no length-prefix or delimiter-escaping fallback and no NAK on a frame that fails to parse.
- fix: This is acceptable for a JSON-line protocol given serde escapes newlines, but consider returning a structured parse-error event to the client (instead of silent `continue`) so desyncs are observable, and document the "one JSON object per line, newlines must be escaped" contract at the type definition.

---

## Summary
The biggest production risks in this scope are concurrency/liveness on the broadcast path: a slow or wedged Swift client (1) permanently loses its event stream the moment it lags >100 events because `while let Ok(...)` treats `Lagged` as terminal (HIGH), and (2) can pin writer/reader tasks with no write timeout, defeating the orphan watchdog and leaking half-open connections (HIGH/MEDIUM). The advertised DoS protections (per-connection rate limit, brace-depth check) are weak/bypassable, and the stale-socket cleanup has a TOCTOU plus a disabled connect timeout that can misclassify a live daemon. The orphan watchdog's `process::exit(0)` can tear down DB/download work mid-flight (MEDIUM, data-loss). Protocol versioning is advisory-only.
