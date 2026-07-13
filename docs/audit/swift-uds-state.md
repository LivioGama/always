# Swift UDS-State Audit — UDSClient.swift, StateMonitor.swift

Scope: `Always/Sources/Always/Services/UDSClient.swift`, `Always/Sources/Always/Services/StateMonitor.swift`

Both files were read in full. Cross-file caller tracing was attempted but the Bash/grep tooling returned no output in this session; findings below are based only on code actually read. The threading model relevant to most findings is fully contained in the two files (the private `com.always.udsclient` queue for the read source + watchdog, vs. `DispatchQueue.main` for reconnect scheduling and external `connect()`/`connectToDaemon()` entry).

---

### [CRITICAL] `socketFD` mutated and read across two queues with no synchronization (race → use of closed/invalid fd)
- file: Always/Sources/Always/Services/UDSClient.swift:395, 516, 532-535, 643-687, 656, 818
- category: concurrency
- confidence: high
- impact: A reconnect on the main queue can close `socketFD` and assign a new fd while the read source's handler (private `queue`) is mid-`recv()` on the old value, or vice versa. Result: `recv`/`send` on a closed fd, reading from a fd that was reassigned to a *different* connection, or `disconnect()` closing an fd that `connect()` just reopened. This is exactly the "overlay silently breaks after daemon restart" class of bug.
- problem: `socketFD` is plain `var socketFD: Int32 = -1` (line 395) with no lock or queue confinement. It is written in `connect()` (line 516, runs on main via `scheduleReconnect`'s `asyncAfter` and via external `connectToDaemon`), written/closed in `disconnect()` (lines 532-535, called from BOTH main and the private `queue`), and read in the read-source handler `recv(self.socketFD, …)` (line 656, private `queue`) and in `writeLine` `send(socketFD, …)` (line 818, main). The watchdog handler (private `queue`) calls `self.disconnect()` (line 623) which closes the fd while the read handler on the same/other thread may also be running. There is no `queue.sync`/lock guarding any of this.
- fix: Confine ALL socket-fd lifecycle (`connect`, `disconnect`, `startReceiving`, `writeLine`, fd reads) to the single private `queue`. Make `connect()`/`disconnect()`/`sendCommand*` hop onto `queue` (`queue.async { ... }`) before touching `socketFD`, `receiveSource`, or `receiveBuffer`. Only publish `@Published` state changes back to main. Example:
```swift
func connect() {
    queue.async { [weak self] in self?._connectOnQueue() }
}
func disconnect() {
    queue.async { [weak self] in self?._disconnectOnQueue() }
}
```
and route `writeLine` / `recv` through the same queue so the fd is never touched concurrently.

---

### [CRITICAL] `disconnect()` closes the fd but does not cancel the read DispatchSource it created on that fd (fd reused under a live source)
- file: Always/Sources/Always/Services/UDSClient.swift:531-543, 656-687
- category: resource-leak
- confidence: high
- impact: On the recv-error / EOF path the handler calls `self.disconnect()` (lines 664-665, 672-673) from inside the read source's own event handler. `disconnect()` does `close(socketFD)` (line 533) *before* `receiveSource?.cancel()` (line 536). A `DispatchSourceRead` requires the fd to remain open until the cancel handler runs; closing the fd while the source is still attached is a documented GCD error and can cause the source to fire on a closed/reused fd or crash. Because `connect()` may immediately reopen a new fd with the same numeric value, the still-live old source can deliver events for the new connection.
- problem: `disconnect()`:
```swift
if socketFD >= 0 {
    close(socketFD)        // closes fd FIRST
    socketFD = -1
}
receiveSource?.cancel()    // cancels source SECOND — fd already closed
receiveSource = nil
```
GCD's contract is: cancel the source, and close the fd only in the source's cancel handler. Here the order is inverted, and the fd is shared with `send()`/`recv()` raw calls.
- fix: Cancel the source first and close the fd inside the source's cancel handler. Track the fd the source owns so the cancel handler closes exactly that fd:
```swift
let fd = socketFD
let source = DispatchSource.makeReadSource(fileDescriptor: fd, queue: queue)
source.setCancelHandler { close(fd) }   // close ONLY here
...
// disconnect():
receiveSource?.cancel()   // triggers cancel handler -> close(fd)
receiveSource = nil
socketFD = -1             // do NOT close() here
```

---

### [HIGH] `lastEventTime` written and read across queues without synchronization (watchdog can spuriously fire or miss)
- file: Always/Sources/Always/Services/UDSClient.swift:401, 518, 619, 735
- category: concurrency
- confidence: high
- impact: `lastEventTime` is a `Date` (non-atomic struct, 2 words) written from the read handler in `handleEvent` (`lastEventTime = Date()`, line 735, private `queue`) and from `connect()` (line 518, main), and read in the watchdog handler (line 619, private `queue`). A torn read of a partially-updated `Date` can yield a garbage interval, causing the watchdog to falsely declare the connection stalled and force an unnecessary disconnect/reconnect storm — or to never trip when it should.
- problem:
```swift
self.lastEventTime = Date()        // connect(): main queue (line 518)
let elapsed = Date().timeIntervalSince(self.lastEventTime)  // watchdog: queue (line 619)
lastEventTime = Date()             // handleEvent: queue (line 735)
```
- fix: Confine `lastEventTime` to the private `queue` (set it from `connect`'s on-queue path once fd lifecycle is queue-confined per the CRITICAL fix), or guard it with an `os_unfair_lock` / make it atomic.

---

### [HIGH] `reconnectAttempts` never reset after exhausting attempts → backoff caps at 30s and respawn is requested on *every* subsequent failure
- file: Always/Sources/Always/Services/UDSClient.swift:398, 564-607, 595-599
- category: correctness
- confidence: high
- impact: `reconnectAttempts` is only reset to 0 inside `connect()` on a *successful* connect (line 517). If the daemon stays down, attempts climb unbounded; once `>= maxReconnectAttemptsBeforeRespawn` (3), `onDaemonNeedsRespawn?()` is invoked on *every single* reconnect attempt thereafter (line 595-599), not once. Combined with the StateMonitor respawn debounce being only `respawnInFlight`, this fires a respawn request every backoff cycle (up to every 30s) for as long as the daemon is unreachable — repeated `restartDaemon()` churn.
- problem:
```swift
if self.reconnectAttempts >= self.maxReconnectAttemptsBeforeRespawn,
   !self.isBootstrapping {
    self.log("Reconnect attempts exhausted — requesting daemon respawn")
    self.onDaemonNeedsRespawn?()   // fires on attempt 3,4,5,6,... forever
}
```
There is no "respawn already requested" latch and no reset of `reconnectAttempts` after requesting respawn.
- fix: Latch the respawn request (e.g. `if reconnectAttempts == maxReconnectAttemptsBeforeRespawn` exactly, or a `respawnRequested` bool reset only on successful connect), and/or reset `reconnectAttempts` after triggering respawn so backoff restarts cleanly.

---

### [HIGH] `processMessage` double-splits on newline but `drainCompletedLines` already split — and a JSON body containing a literal `\n` inside a string would be mis-parsed
- file: Always/Sources/Always/Services/UDSClient.swift:692-731, 702-704
- category: correctness
- confidence: medium
- impact: `drainCompletedLines` passes a chunk that may contain several `\n`-terminated lines to `processMessage`, which re-splits on `"\n"` (line 704). This works only because the daemon emits compact single-line JSON. If any event payload ever contains an embedded newline (e.g. a transcript or correction string with `\n`, which `serde_json` does NOT escape as `\n`... actually it does — but a non-compact serializer or a multi-line field would not), the framing breaks and the event is silently dropped (decode failure → counts toward forced reconnect). The framing relies entirely on "JSON never contains a raw newline," which is an implicit, undocumented contract with the Rust side.
- problem: Line-delimited framing assumes the daemon never emits a `\n` except as a record terminator. `drainCompletedLines` uses `lastIndex(of: 0x0A)` and `processMessage` re-splits with `components(separatedBy: "\n")`. There is no length-prefix framing despite the protocol carrying a numeric version handshake.
- fix: Either (a) document and assert the daemon always uses compact JSON (serde_json::to_writer with no pretty printing, which escapes interior `\n` as `\\n` — verify this on the Rust side), or (b) switch to length-prefixed framing. At minimum, drop the redundant re-split in `processMessage` and iterate `\n`-delimited subranges directly from `receiveBuffer` to avoid the double pass.

---

### [MEDIUM] `lowMicrophoneVolume` handler reads `event.data?["energy"] as? Double` but `data` is `[String:String]?` — energy is never delivered to the overlay
- file: Always/Sources/Always/Services/StateMonitor.swift:395-399; Always/Sources/Always/Services/UDSClient.swift:141-143, 209, 272-273
- category: correctness
- confidence: high
- impact: For `LowMicrophoneVolume`, the decoder populates the typed `event.lowMicrophoneVolume: LowMicrophoneVolumeData` (energy: Double) — NOT the loose `data` dict (the `default:` branch that fills `data` is never hit for this case, see decoder lines 272-273). But the handler reads `event.data?["energy"] as? Double`. Since `data` is `nil` for this event and is typed `[String:String]`, `event.data?["energy"]` is always `nil`, so the `if let` fails and the low-mic-volume warning overlay NEVER shows.
- problem:
```swift
case .lowMicrophoneVolume:
    if let energy = event.data?["energy"] as? Double {   // data is nil here; also [String:String]
        StatusOverlayController.shared.flash(state: .lowMicrophoneVolume(energy: energy), duration: 3.0)
    }
```
The cast `as? Double` on a `String?` value is also nonsensical (would never succeed even if populated as a string). The correct field is `event.lowMicrophoneVolume?.energy`.
- fix:
```swift
case .lowMicrophoneVolume:
    if let energy = event.lowMicrophoneVolume?.energy {
        StatusOverlayController.shared.flash(state: .lowMicrophoneVolume(energy: energy), duration: 3.0)
    }
```

---

### [MEDIUM] `connect()`'s `guard !isConnected` reads a main-thread `@Published` from a background queue — and can wrongly short-circuit reconnects
- file: Always/Sources/Always/Services/UDSClient.swift:388, 470, 520-525
- category: concurrency
- confidence: medium
- impact: `isConnected` is `@Published` and is mutated only via `DispatchQueue.main.async` (lines 520-525, 540-542). `connect()` begins with `guard !isConnected else { return }` (line 470). Because the flag flips on main asynchronously, there's a window where `disconnect()` has been called (fd closed) but `isConnected` is still `true` on the next `connect()` check, causing the reconnect to silently no-op and leave the client permanently disconnected until the watchdog fires again. Conversely, reading a `@Published` (main-actor-ish) property off the private queue is a data race on Combine's internal storage.
- problem:
```swift
func connect() {
    guard !isConnected else { return }   // isConnected only updated async on main
```
The connect/disconnect gating uses a UI-bound flag as a connection-state machine, which it is not safe for.
- fix: Track an internal, queue-confined connection state (e.g. `socketFD >= 0` on the private queue) for gating, and keep `isConnected` purely for UI. Do the guard on the private queue against `socketFD`.

---

### [MEDIUM] Decoded events delivered via `NotificationCenter` lose typing and ordering guarantees; every subscriber re-decodes via downcast
- file: Always/Sources/Always/Services/UDSClient.swift:761-763; Always/Sources/Always/Services/StateMonitor.swift:319-327
- category: maintainability
- confidence: medium
- impact: Events are posted as `NotificationCenter.default.post(name: .daemonEvent, object: event)` on main (line 761-763). Each event causes a hop from the private queue → main → Combine publisher → sink. Under a burst (e.g. `ModelsList` + rapid `ModelDownloadProgress` ticks) the main run loop coalesces nothing but does serialize through NotificationCenter, and any other code listening on `.daemonEvent` gets the object too. There is no backpressure; high-frequency `autoEnterCountdownTick` / `modelDownloadProgress` floods main. More importantly, ordering between the watchdog's `isDegraded` update and event delivery is not guaranteed.
- problem: Using `NotificationCenter` as the event bus for a high-rate typed stream is fragile and untyped; `compactMap { $0.object as? DaemonEvent }` silently drops anything mis-posted.
- fix: Expose a typed `PassthroughSubject<DaemonEvent, Never>` on `UDSClient` and subscribe to it in `StateMonitor`, removing the `NotificationCenter` round-trip. Keeps the type and confines the publisher.

---

### [MEDIUM] Hello version-mismatch disconnects but does NOT stop reconnecting → tight respawn/reconnect loop against an incompatible daemon
- file: Always/Sources/Always/Services/UDSClient.swift:742-756
- category: error-handling
- confidence: medium
- impact: On a protocol-version mismatch the client logs, sets `connectionError`, and calls `disconnect()` (line 751) — but does NOT call `scheduleReconnect()` here, so it relies on the watchdog... except the watchdog only runs while connected, and `disconnect()` stops it. However, the recv EOF that follows the server closing (or the next watchdog cycle before stop) can still trigger `scheduleReconnect`, which reconnects, gets another mismatched Hello, and disconnects again — an endless reconnect loop against a daemon it can never talk to, and (per the respawn finding) eventually spams `restartDaemon()`. A version-incompatible daemon will be repeatedly killed/restarted with no user-visible resolution.
- problem: There is no "give up / surface to user and stop" terminal state for a protocol mismatch; the only signal is `connectionError` which may be overwritten by the next connect's `connectionError = nil` (line 523).
- fix: On version mismatch set a sticky `isHostQuitting`-like terminal flag (e.g. `incompatibleDaemon = true`) that `scheduleReconnect`/`connect` honor, stop reconnecting, and surface a persistent UI error prompting the user to update the app. Do not request respawn for a version mismatch.

---

### [MEDIUM] `setBootstrapping(false)` flips `isDegraded = true` immediately if not yet connected, racing the bootstrap connect
- file: Always/Sources/Always/Services/UDSClient.swift:553-562; Always/Sources/Always/Services/StateMonitor.swift:126-130
- category: ux
- confidence: medium
- impact: `endBootstrap()` → `setBootstrapping(false)` sets `isDegraded = true` whenever `!isConnected` (line 558-560). But `isConnected` only flips on a later main hop after the socket connects. If `endBootstrap` runs (e.g. from the connect sink at StateMonitor.swift:80) in the same main cycle, ordering is fine; but if `endBootstrap` is called from elsewhere right before the connect's async main update lands, the UI flashes "Reconnecting…/degraded" for one cycle even though connection is healthy.
- problem: Bootstrap-end uses the async UI flag `isConnected` as a synchronous source of truth.
- fix: Gate `isDegraded` on the queue-confined `socketFD >= 0` (per the concurrency fix) rather than the async `isConnected`, or only raise degraded after a real failed connect post-bootstrap.

---

### [LOW] `memcpy` into `sun_path` does not NUL-terminate explicitly (relies on zero-init); benign today but brittle
- file: Always/Sources/Always/Services/UDSClient.swift:485-499
- category: correctness
- confidence: low
- impact: `addr` is `sockaddr_un()` (zero-initialized), and the length guard is `pathBytes.count < sizeof(sun_path)` (strictly less), so the trailing byte stays 0 → NUL-terminated. Correct, but the termination is implicit. If the guard were ever loosened to `<=`, the path would be un-terminated and `connect` would read past the buffer.
- problem:
```swift
guard pathBytes.count < MemoryLayout.size(ofValue: addr.sun_path) else { ... }
_ = withUnsafeMutablePointer(to: &addr.sun_path) { pathPtr in
    memcpy(pathPtr, socketPath, pathBytes.count)
}
```
- fix: Keep the strict `<`, and add a comment that the trailing NUL relies on zero-init; or explicitly write the terminator. Also prefer passing the actual address length (`sizeof(sun_family) + path.count + 1`) instead of the full `sizeof(sockaddr_un)` to `connect`.

---

### [LOW] Partial writes in `writeLine` are not handled — a short `send()` silently truncates a command
- file: Always/Sources/Always/Services/UDSClient.swift:814-827
- category: error-handling
- confidence: low
- impact: `send()` may return fewer bytes than requested under buffer pressure. `writeLine` only checks `< 0`; a partial write (`0 < bytesWritten < count`) logs "Sent command" as success while sending a truncated, undecodable command line to the daemon (which then fails to parse it). Commands are small so this is rare, but it is unhandled.
- problem:
```swift
let bytesWritten = data.withUnsafeBytes { buffer in
    send(socketFD, buffer.baseAddress!, buffer.count, 0)
}
if bytesWritten < 0 { ...error... } else { ...success... }  // no partial-write loop
```
- fix: Loop until all bytes are written (or error), advancing the pointer by `bytesWritten` each iteration.

---

### [LOW] `receiveBuffer` can grow unbounded if a peer streams bytes with no newline
- file: Always/Sources/Always/Services/UDSClient.swift:640, 677-700
- category: resource-leak
- confidence: low
- impact: `drainCompletedLines` only trims up to the last `\n`. If the daemon (or a corrupted/stale socket) ever sends a large volume of data with no newline, `receiveBuffer.append` (line 677) grows without bound and is never drained, consuming memory until the connection eventually closes. No max-buffer cap or "line too long" guard exists.
- problem: No upper bound on `receiveBuffer` size; a missing terminator wedges the buffer.
- fix: Add a max-line guard (e.g. if `receiveBuffer.count` exceeds, say, 4 MB with no `\n`, log, `disconnect()`, and `scheduleReconnect()` — the stream is corrupt).

---

### [LOW] `respawnDaemonIfNeeded` debounce (`respawnInFlight`) is read/written without main confinement
- file: Always/Sources/Always/Services/StateMonitor.swift:40, 101-114
- category: concurrency
- confidence: low
- impact: `respawnInFlight` (line 40) is checked/set in `respawnDaemonIfNeeded` and cleared in a `Task` `defer` (line 106). `respawnDaemonIfNeeded` is invoked from `onDaemonNeedsRespawn` which is called inside `scheduleReconnect`'s main-async closure, so it's effectively main; but the `defer { self?.respawnInFlight = false }` runs on the `Task`'s executor (not guaranteed main), so the flag is read on main and written off-main — a benign-but-real race that could allow two overlapping respawns.
- problem:
```swift
guard !respawnInFlight, !isBootstrapping else { return }   // main
respawnInFlight = true                                       // main
Task { [weak self] in
    defer { self?.respawnInFlight = false }                 // Task executor, maybe not main
```
- fix: Mark `StateMonitor` `@MainActor` (it's UI-facing anyway) so the flag and `@Published` props are actor-isolated, and `await MainActor.run { respawnInFlight = false }` in the defer.
