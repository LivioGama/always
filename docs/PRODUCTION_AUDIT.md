# Always — Production-Readiness Audit (2026-05-30)

A systematic bug + production-readiness audit of the **Always** voice-to-text
system: the Rust daemon (`src/`, ~16.5k LOC) and the Swift menu-bar app
(`Always/Sources/`, ~6.8k LOC).

Method: parallel specialist reviewers swept every subsystem plus cross-cutting
lenses (panic/`unwrap`, concurrency/deadlock, security). Raw per-unit findings
are preserved under [`docs/audit/`](audit/) (`rust-*.md`, `swift-*.md`).

Totals across finders (pre-dedup): **6 CRITICAL, 27 HIGH, 62 MEDIUM, 54 LOW**.

This document tracks the high-severity findings and their status. It is the
single source of truth for what was fixed and what remains.

> **Status as of 2026-06-12: all CRITICAL and HIGH findings are fixed and
> verified at HEAD.** See the per-item evidence in "Remaining — prioritized
> backlog" below. What's left: the MEDIUM/LOW pool under `docs/audit/`, the
> async-grammar clipboard restore (MEDIUM), and the `rdev` → CGEventTap
> migration (blocks crates.io publish, not the DMG release).

---

## Fixed in this pass (verified: `cargo build` + `cargo clippy` clean, `cargo test` green, also builds under `--features local-stt`)

| # | Sev | File | Fix |
|---|-----|------|-----|
| 1 | **CRITICAL** | `src/always/event_loop.rs` | **Paste-in-flight lock leak → permanent mute.** After `try_begin_paste()`, a bare `copy_to_clipboard(...)?` could return without `end_paste()`, so every future utterance dropped as `duplicate_paste_suppressed_in_flight` until restart. Both paste-failure paths now release the lock, clear the dictation merge buffer, and emit `voice_activity_ended` + `transcription_filtered` so the overlay resets. (Also closes the two HIGH findings: stale merge buffer + stuck overlay on paste failure.) |
| 2 | **CRITICAL** | `src/db.rs` | **SQLite `busy_timeout(5s)`.** Daemon threads + the separate CLI/GUI process each open their own connection; without a busy timeout a contended write failed instantly with `SQLITE_BUSY` (silently dropped preference writes). |
| 3 | HIGH | `src/always/correction_queue.rs` | **Atomic persist (temp + rename).** A crash mid-write no longer truncates `pending_corrections.json` (which silently dropped the whole review queue on next start). |
| 4 | HIGH | `src/always/correction.rs` | **Atomic glossary writes (both sites).** A crash mid-write no longer destroys `glossary.json` (which silently disables ALL vocabulary corrections). |
| 5 | HIGH | `src/always/config.rs` | **Atomic config save (temp + rename, pid-suffixed temp).** A crash mid-write no longer leaves a truncated `config.toml` that resets every user setting on next launch. |
| 6 | HIGH | `src/always/config.rs` | **Clamp per-app DB values on read** (`energy_threshold`, `cooldown_ms`, `silero_threshold` w/ non-finite rejection, `auto_enter_delay_ms`, `idle_pause_secs`) to the same bounds the write path enforces. A corrupt/out-of-range row can no longer silently break the VAD. |
| 7 | HIGH | `src/always/paste.rs` | **Always reap the `pbcopy` child.** On a broken-pipe write error the child was never `wait()`ed → zombie accumulation on a daemon that pastes thousands of times. |
| 8 | HIGH | `src/always/uds_server.rs` | **Broadcast loop survives `Lagged` + per-write timeout.** A slow client briefly exceeding the 100-event channel made `recv()` return `Lagged`, and the old `while let Ok(..)` killed that client's event stream forever (overlay/menu-bar froze until app restart). Now `Lagged` is logged and skipped; writes are bounded by a 5s timeout so a wedged client is dropped, not pinned. |
| 9 | HIGH (security) | `src/managers/model_registry.rs` | **tar.gz path-traversal (zip-slip) guard.** Extraction now rejects absolute / `..` entry paths and uses `unpack_in` (which refuses entries that escape the destination), so a malicious/corrupt model archive can't write into `~` (LaunchAgent/`.zshrc` → persistence/RCE). |

---

## Fixed in pass 2 (subagent fan-out — one agent per file, built/tested centrally)

| Sev | File | Fix |
|-----|------|-----|
| **CRITICAL** | `Always/.../UDSClient.swift` | Socket-fd lifecycle confined to the private read queue (no more cross-queue race on `socketFD`); `DispatchSource` cancelled before the fd is closed, with the fd closed in the source's cancel handler (was: `close()` then `cancel()` — GCD-illegal); `lastEventTime` queue-confined; respawn request latched to fire once per outage. |
| HIGH | `src/stt.rs` | 2xx-with-unparseable-body now retried as transient (was a permanent fail with no `record_failure`/retry); `auth_header` derives per call from the passed key (removed the process-global `OnceLock` that froze the first key). |
| HIGH | `src/always/per_app.rs` | Per-app cache now keyed on DB mtime (incl. the `-wal` sidecar) so a cross-process CLI edit is picked up on the next focus change instead of requiring a daemon restart. |
| HIGH | `src/always/mic_monitor.rs` | Mic-in-use detection parses `lsof` columns in Rust — matches the COMMAND column only, excludes own pid + exact helper names, filters before limiting (was substring-grep on the whole line + premature `head -5`). |
| HIGH | `src/always/stt_local.rs` | Hard 10-minute audio cap before decode + post-decode truncate — bounds local-STT memory on a stuck-open utterance. |
| HIGH | `src/always/uds_server.rs` | Initial Hello/state burst at connect wrapped in a 5s write timeout so a non-reading client can't wedge `handle_client` and defeat the orphan watchdog. |
| HIGH | `Always/.../CLIService.swift` | `runCLI` drains the stdout/stderr pipe to EOF before `waitUntilExit()` (was the classic wait-then-read deadlock) and runs off the Swift cooperative pool. |

---

## Remaining — prioritized backlog

> Status legend: ✅ = fixed & verified (`cargo build`/`cargo test`/`clippy` green, `--features local-stt` + `swift build` green); 🔲 = open / recommended next.
>
> **2026-06-12 sweep:** every formerly-open CRITICAL/HIGH item below was
> re-verified against HEAD and is now fixed. Status flipped with the fixing
> commit + current file:line evidence so this document stops claiming open
> blockers that no longer exist.

### CRITICAL

- ✅ **Handy-cached / pre-existing models loaded with NO SHA256 verification** — fixed in `c373db89`. `model_path` / `refresh_disk_status` (`src/managers/model_registry.rs:243`) now re-hash single-file `.bin` models against the catalog SHA (memoized by size+mtime) and trust directory models only with our own `<filename>.verified` marker; Handy's extracted directories are never returned for directory models.
- ✅ **Production paste destroys the clipboard with no save/restore** — fixed in two steps. `7b1a3593` added the guarded snapshot/restore (`event_loop.rs` snapshots before the write and `restore_clipboard_if_unchanged` re-checks the pasteboard); `7bd8adb1` added the `NSPasteboard.changeCount` token (raw objc bridge in `src/always/paste.rs`) so a foreign write of the *identical string* or of non-text flavors also cancels the restore. Remaining (MEDIUM): the async-grammar path defers its restore (`TODO(clipboard-restore)` in `event_loop.rs`) and only the string flavor round-trips.

### HIGH

- ✅ `src/always/event_loop.rs` — **`apply_grammar_blocking` untimed block** — fixed in `34bb5d1a`: wrapped in `tokio::time::timeout(GRAMMAR_BLOCKING_TIMEOUT /* 8s */)` with fallback to the acoustic text (`event_loop.rs:777`).
- ✅ `src/stt.rs` + `src/stt_dispatch.rs` — **circuit breaker now actually falls back to local** — fixed in `be25248a`: `build_transcriber` wraps the Groq primary in `FallbackTranscriber`, which lazily loads the best downloaded (integrity-verified) local model on the first open-breaker error and transcribes offline until Groq recovers. `4f6e10be` adds the one-shot `SttFallbackEngaged` UDS event + orange overlay notice so degradation is visible.
- ✅ `src/always/daemon.rs` — **PID file races** — fixed in `7b1a3593`: `PidGuard::install` takes the flock *before* writing (no truncate-on-open visibility window) and unlinks the PID file on every bail path.
- ✅ `src/always/focus_state.rs` — **dead code wired** — fixed in `adb5e7d3`: persisted on focus change (`uds_server.rs`) and restored at daemon startup (`event_loop.rs:81`).
- ✅ `src/always/audio.rs` / `vad.rs` — **dead/wedged recorder recovery** — fixed in two steps. `7b1a3593` evicts the dead `RecChild` from the global slot on EOF so the next capture cycle respawns. `b8f362a8` bounds every `read_frame` with a raw `poll(2)` (2s ceiling; healthy capture delivers a frame every 30ms) and evicts on timeout — a wedged `rec` (bytes never arrive, no EOF) can no longer hold the recorder mutex forever and deadlock all audio callers.
- ✅ `src/always/uds_server.rs` / `event.rs` — **event size bound** — fixed in `c373db89`: `MAX_EVENT_FIELD_CHARS = 8192` with `cap_field` truncation (+ elision marker) applied at serialization (`event.rs`).

### MEDIUM / LOW

62 MEDIUM and 54 LOW findings (robustness, edge cases, maintainability) are in
the per-unit files under [`docs/audit/`](audit/). Notable themes: unicode/char-
boundary handling in `text_match`/`hallucination`, regex/allocation bounds,
SwiftUI `@State`/`ObservableObject` lifetime in the Settings panels, and
observer/timer cleanup in the Swift services.

---

## Operational / release posture (not code-level)

Strong baseline already in place: CI runs `fmt` + `clippy -D warnings` + tests +
`cargo-llvm-cov` + `cargo-audit` + `cargo-deny` + `cargo-machete` + CodeQL +
dependency-review; release pipeline does universal (arm64+x86_64) builds, DMG +
SHA256 + Sparkle EdDSA appcast, and notarization with a real pre-flight check.
`deny.toml` enforces licenses/bans (rustls-only, no OpenSSL) and flags the
unmaintained `rdev`. No secrets tracked in git.

Watch items: finish the `rdev` → native `CGEventTap` migration (tracked P2.2);
ensure the Sparkle `SUPublicEDKey` placeholder is replaced before any release
(the release script already hard-fails on it).
