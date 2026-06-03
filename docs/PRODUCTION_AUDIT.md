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

> Status legend: ✅ = fixed & verified in the subagent fan-out (pass 2 — `cargo build`/`cargo test`/`clippy` green, `--features local-stt` + `swift build` green); 🔲 = open / recommended next.

### CRITICAL

- 🔲 **Handy-cached / pre-existing models loaded with NO SHA256 verification** — `src/managers/model_registry.rs` (`model_path`, `refresh_disk_status`). `verify_sha256` runs *only* on bytes this process downloads itself; a file already on disk (incl. Handy's cache, a dir outside our trust boundary) is marked `is_downloaded` and loaded verbatim. *Deferred deliberately:* the correct fix is non-trivial (single-file `.bin` can be re-hashed against the catalog SHA, but directory/tar.gz models cannot — they need a `.verified` marker dropped only when **we** extract them), and this is in the file currently being iterated on (Handy-cache reuse WIP). Recommend: re-hash `.bin` models before first use; require a `.verified` marker for directory models; never blindly trust Handy's extracted dir.
- 🔲 **Production paste destroys the clipboard with no save/restore** — `src/always/paste.rs` + `event_loop.rs`. Every dictation clobbers the user's clipboard permanently. *Deferred deliberately:* a correct fix needs `NSPasteboard.changeCount` (via the objc bridge) to avoid the restore racing a concurrent user copy — the naive pbpaste-only restore is itself a bug (it would overwrite a fresh copy). `correction.rs` already has the snapshot/restore pattern but without the changeCount guard. Recommend: implement save/restore guarded by `changeCount`, behind a config opt-out.
### HIGH

- 🔲 `src/always/event_loop.rs:623` — **`apply_grammar_blocking` runs an untimed `rt.block_on(pp.process(..))` on the main loop thread**; a hung LLM endpoint freezes the daemon (no new utterance recorded). Wrap in `tokio::time::timeout` and fall back to the acoustic text (the error arm already does this). *(Owner: maintainer — file has in-flight WIP.)*
- 🔲 `src/stt.rs` + `src/always/vad.rs` — **circuit breaker / `should_fall_back` never actually falls back to local**: the documented graceful-degradation during a Groq outage is not wired (utterances are simply lost for the cooldown). Either implement the local fallback or remove the promise from the docs.
- 🔲 `src/always/daemon.rs:333` — **PID file truncated before the flock** (visibility window where a live daemon looks dead) and **a 0-byte PID file is leaked on the post-lock bail paths** (the "cleaned up on drop" comment is false — `File` drop doesn't `remove_file`). *(Owner: maintainer — PidGuard has in-flight WIP.)*
- 🔲 `src/always/focus_state.rs` — **`save()` / `restore_on_startup()` are dead code** (zero call sites); the "restore per-app pause across restarts" feature never runs, so dictation appears dead right after a daemon restart until refocus. Wire it (persist on focus change; restore at startup) or delete the module.
- 🔲 `src/always/audio.rs` / `vad.rs` — **mic disconnect/EOF abandons the in-progress capture and leaves the dead recorder in the global slot** (no respawn/retry on transient USB re-enumeration), and **`read_frame` blocks under the global recorder mutex** (a wedged `rec` can deadlock every audio caller). Higher-risk; needs careful design.
- 🔲 `src/always/uds_server.rs` / `event.rs` — **no size bound on daemon→client events** (`TranscriptFinal`, `ModelsList`, …); a huge transcript becomes one unbounded line every client must buffer.

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
