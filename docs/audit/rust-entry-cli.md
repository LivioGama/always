# Rust Audit — entry-cli scope

Scope: `src/main.rs`, `src/lib.rs`, `src/cli/menu_bar.rs`, `src/cli/logs.rs`, `src/cli/mod.rs`

Verification performed:
- All five files read in full.
- `cargo check --bin always` / `cargo build --bin always` run → **exit 0, "Build finished", zero warnings**. The project compiles cleanly. (`anyhow::Context` in logs.rs IS used — 3 `.context()` calls — so it is not an unused import.)
- Cross-checked the dispatch targets that owe their contract to the CLI surface: `daemon::start` (confirmed double-start guard at daemon.rs:163-164 / 203-204 → prints to stdout, `return Ok(())`, exit 0; plus a UDS "refuse-to-start" short-circuit at daemon.rs:312 and a pid-lock at daemon.rs:340; liveness via `is_running()` → PID file + `process_is_running`), `build.rs` (sets `ALWAYS_BUILD_SHA`), `always/mod.rs` (`#[cfg(target_os="macos")] pub mod daemon`).

Architecture note: `Cli`/`Commands` are defined **inline in `src/main.rs`** (not imported from the lib). `src/lib.rs` does NOT expose a `cli` module; `mod cli;` in `main.rs` resolves to `src/cli/mod.rs` which re-exports `LogsCommand`/`handle_logs` and the `menu_bar` module. No double-module / dead-module problems exist (an earlier suspicion based on a stale read was disproven by the successful build).

---

### [HIGH] `tail_pretty` orphans the spawned `tail -F` child on BrokenPipe/early-return and never kills it
- file: src/cli/logs.rs:125
- category: resource-leak
- confidence: high
- impact: `always logs --pretty | head` (or any consumer that closes its read end) makes the `println!` calls panic with `BrokenPipe` (Rust's default), unwinding out of the read loop **before** `child.wait()` at line 161 runs. The spawned `tail -F` is left orphaned, still tailing the file, every time. Repeated piped invocations leak `tail` processes that hold the log file open — bad for an always-on tool the user runs often.
- problem:
  ```rust
  let mut child = Command::new("tail")
      .arg("-F").arg("-n").arg("100").arg(path)
      .stdout(Stdio::piped()).spawn()?;
  ...
  for line in reader.lines() {
      ...
      if let Some(rendered) = render_event(&json) {
          println!("{rendered}");   // BrokenPipe -> panic -> unwinds past child.wait()
      }
  }
  child.wait().ok();   // never reached on the panic path -> tail orphaned
  ```
  Same broken-pipe panic risk for the raw JSON fallback `println!("{line}")` (logs.rs:147) and the "Not JSON — print raw" branch.
- fix: Guard the child so it is killed on any scope exit, and treat BrokenPipe as a clean exit instead of panicking:
  ```rust
  struct Kill(std::process::Child);
  impl Drop for Kill { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }
  let child = Kill(child);
  ...
  use std::io::Write;
  let mut out = std::io::stdout().lock();
  if writeln!(out, "{rendered}").is_err() { break; } // BrokenPipe -> stop cleanly
  ```

---

### [MEDIUM] `tail_pretty` discards the `tail` exit status — `--pretty` silently succeeds when tail fails
- file: src/cli/logs.rs:161
- category: error-handling
- confidence: high
- impact: `child.wait().ok();` throws away the result, so if `tail` could not open the file (permissions, file removed mid-rotation) `always logs --pretty` still returns `Ok(())` and prints nothing. A maintainer debugging "logs show nothing" (the exact failure mode CLAUDE.md warns about) gets zero signal that `tail` itself failed.
- problem:
  ```rust
  child.wait().ok();
  Ok(())
  ```
- fix:
  ```rust
  let status = child.wait().context("waiting on tail")?;
  if !status.success() { anyhow::bail!("tail exited with {status}"); }
  ```

---

### [MEDIUM] Default `always logs` (non-pretty) ignores `tail`'s exit status → silent empty output looks like "no logs"
- file: src/cli/logs.rs:69
- category: error-handling
- confidence: medium
- impact: The default (non-pretty) path execs `tail -F <file>` and `child.wait()?` only returns `Err` if *waiting* fails — not if `tail` exits non-zero. If `tail` can't open the file, the command returns success having printed nothing. Combined with the `--since`/`--level` flags being **silently ignored** on this path (they are declared in `LogsCommand` but only `--pretty` consults `--level`; `--since` is never used anywhere), `always logs --since 1h --level error` quietly behaves like a plain follow with no filtering — the user believes they filtered and trust the (unfiltered) output.
- problem:
  ```rust
  let mut tail_cmd = Command::new("tail");
  tail_cmd.arg("-F").arg(&log_file);
  let mut child = tail_cmd.spawn()?;
  child.wait()?;          // tail's non-zero exit is not surfaced
  Ok(())
  ```
  `--since` (logs.rs:21) is parsed but referenced nowhere; `--level` (logs.rs:25) is honored only inside `tail_pretty`.
- fix: Check `child.wait()?.success()` and bail on failure; and either implement `--since`/`--level` for the default path or reject them with a clear error when not combined with `--pretty` (`anyhow::bail!("--since/--level require --pretty")`) so the flags are never silently dropped.

---

### [MEDIUM] `--date` value is unvalidated and interpolated straight into a filesystem path (traversal + confusing errors)
- file: src/cli/logs.rs:75
- category: security
- confidence: medium
- impact: `always logs --date '../../../../etc/passwd'` builds `log_dir.join("always.../../../../etc/passwd")`; `PathBuf::join` with `..` components escapes the log directory, so the CLI will `tail`/read an arbitrary file the user can name (mild local path traversal). More commonly a typo'd date yields a misleading "log file not found" instead of "invalid date".
- problem:
  ```rust
  if let Some(d) = date {
      let path = log_dir.join(format!("always.{d}"));   // d unvalidated
      if !path.exists() { anyhow::bail!("log file not found: {} ...", path.display()); }
      return Ok(path);
  }
  ```
- fix: Validate the format before use:
  ```rust
  chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
      .map_err(|_| anyhow::anyhow!("invalid --date '{d}', expected YYYY-MM-DD"))?;
  ```
  and reject any `d` containing `/` or `..`.

---

### [LOW] CLI handlers call `std::process::exit(1)`, bypassing logging-guard flush (can drop final log lines)
- file: src/main.rs:432, 535, 545, 564
- category: data-loss
- confidence: medium
- impact: `main()` holds `_logging_guard` (main.rs:188), the `tracing-appender` non-blocking writer guard whose `Drop` flushes buffered log lines. Several handlers (`config delete-key` unknown key, `corrections approve/reject` unknown id, `corrections capture` no-recent-paste) call `std::process::exit(1)` mid-function. `process::exit` runs no destructors, so the guard never flushes — any log line written just before the exit can be lost. For a daemon whose logs are the primary diagnostic channel, that is a (small) data-loss/observability gap on error paths.
- problem:
  ```rust
  eprintln!("unknown correction id: {id}");
  std::process::exit(1);   // _logging_guard not dropped -> buffered logs may not flush
  ```
- fix: Return errors instead of exiting, so the guard drops and flushes:
  ```rust
  anyhow::bail!("unknown correction id: {id}");
  ```
  and let `main`'s `Result` return map it to exit 1 (clap/anyhow already does). If a specific exit code is mandatory, drop the guard explicitly before exiting.

---

### [LOW] `always start` returns exit 0 whether it started the daemon or found one already running — callers can't tell
- file: src/main.rs:204 (dispatch) / daemon.rs:23-26 (guard)
- category: ux
- confidence: medium
- impact: Double-start prevention works correctly (`is_running()` checks PID file + `kill(pid,0)` liveness, so a stale PID file from a crash does NOT block a restart — good). But the already-running branch prints `⚠️ Always is already running` to **stdout** and `return Ok(())` (exit 0). A wrapper/launchd `KeepAlive` that runs `always start` to ensure liveness cannot distinguish "I started it" from "already up" from the exit code — both 0 — so it can't detect a wedged instance.
- problem (dispatch): `Some(Commands::Start { .. }) => always::always::daemon::start(...)` forwards directly; the guard returns `Ok(())`.
- fix: Return a distinct exit code (or a typed result) from `start` for the no-op case, or document the idempotent contract. (Implementation lives in daemon.rs but the exit-code contract is owned by the CLI surface.)

---

### [LOW] `send_uds_command` only bounds the write, not the connect — comment overstates the timeout guarantee
- file: src/main.rs:660
- category: concurrency
- confidence: low
- impact: `toggle-pause`/`toggle-auto-enter` open the daemon UDS socket. The doc comment says "Times out after 2 s so a stuck daemon doesn't hang the CLI," but only `set_write_timeout(2s)` is applied; `UnixStream::connect` has no timeout. For a normal existing socket this is instant, but a leftover socket file whose peer never `accept`s can make the connect/handshake path block longer than the advertised 2s. Low likelihood (the daemon either has a live accept loop or the socket file is absent → connect fails fast), but the comment promises more than the code delivers.
- problem:
  ```rust
  let mut stream = UnixStream::connect(&sock_path)?;          // no connect timeout
  stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
  writeln!(stream, "{json}")...;
  ```
- fix: Acceptable for the common case; on a write timeout surface a clear "daemon not responding" message rather than a generic IO error, and consider a liveness pre-check before connecting.

---

## Files reviewed and clean
- **src/lib.rs** — correct `pub mod` declarations + `#![warn(clippy::print_stdout, clippy::print_stderr)]`. The CLI deliberately `#![allow]`s those lints in `cli/logs.rs` because the logs subcommand's stdout output *is* the product. No defects.
- **src/cli/mod.rs** — trivial, correct: `pub mod logs; pub mod menu_bar; pub use logs::{LogsCommand, handle_logs};`. No defects.
- **src/cli/menu_bar.rs** — `reset()` checks `script.is_file()` before exec, propagates a non-zero script exit via `anyhow::bail!`, and uses `CARGO_MANIFEST_DIR` (only valid in a repo checkout — stated explicitly in the error message). No defects.
- **src/main.rs** dispatch/argument layer — clap parse (exit 2 on bad args via `Cli::parse()`); logging is initialized with `?` so a daemon that cannot log fails loud rather than silently (correct for an always-on process); keychain key migration is non-fatal-with-`tracing::warn!` (correct for headless/Linux); UUIDs passed to `corrections approve/reject` are validated *before* touching the queue (no silent "not found" on a typo). Only the LOW items above apply.

## Env / secret handling (clean)
No `GROQ_API_KEY`/env-var secret reads occur in the entry/CLI scope. API keys flow through the OS Keychain (`keyring::set/get_groq_api_key`), and `always config show` masks them as `*** (in keychain)` / `*** (in database)` (main.rs:266-275, 333-341). No secret is ever echoed. Version string uses build-time `env!("CARGO_PKG_VERSION")` + `env!("ALWAYS_BUILD_SHA")` (set by `build.rs`) — compile-time only, no runtime env risk.
