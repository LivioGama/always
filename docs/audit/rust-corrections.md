# Rust Corrections Audit — Always

Scope: `src/always/correction.rs`, `src/always/correction_queue.rs`, `src/glossary.rs`, `src/db.rs`.
All four files read in full. Caller traces confirmed via grep (callers noted per finding).

---

### [CRITICAL] SQLite opened with no busy timeout — concurrent access from daemon threads + GUI/CLI processes fails writes
- file: src/db.rs:47-56 (`open`)
- category: concurrency
- confidence: high
- impact: `db::open()` is called independently from many places — the daemon UDS server thread (`uds_server.rs:694`), the keyboard handler thread (`keyboard.rs:153`), the event loop (`event_loop.rs:121`), per-app cache reload (`per_app.rs:86,186`), keyring sync (`keyring.rs:103`), config readers (`config.rs:539,592`), AND the separate `always` CLI process (`main.rs:252,417,436`). Each gets its own `Connection` to the same `~/.config/always/always.db`. WAL permits concurrent readers + one writer, but with no `busy_timeout` a second writer hitting a held write lock fails *immediately* with `SQLITE_BUSY` ("database is locked") instead of waiting. In an always-on daemon this is an intermittent, hard-to-reproduce failure: dropped preference writes, `set_preference` aborting with `?`, and CLI commands failing while the daemon writes.
- problem: No busy timeout / busy handler anywhere (grep for `busy_timeout`/`busy_handler` across `src/` returns nothing):
  ```rust
  let conn = Connection::open(&path).context("Failed to open database")?;
  conn.execute_batch("PRAGMA journal_mode=WAL;")?;
  migrate(&conn)?;
  Ok(conn)
  ```
- fix: Configure a busy timeout right after open so contending writers back off and retry instead of erroring:
  ```rust
  let conn = Connection::open(&path).context("Failed to open database")?;
  conn.busy_timeout(std::time::Duration::from_secs(5))?;
  conn.execute_batch("PRAGMA journal_mode=WAL;")?;
  ```

### [HIGH] Glossary writes are non-atomic and unsynchronized — concurrent captures lose data; a crash mid-write destroys glossary.json
- file: src/always/correction.rs:340-341 (`apply_pairs_to_glossary`) and src/always/correction.rs:402-403 (`add_or_bump_term`)
- category: data-loss
- confidence: high
- impact: Both functions do read-modify-write on `~/.always/glossary.json` with a bare `std::fs::write`, no file lock, no temp-file+rename. Confirmed concurrent callers: the keyboard hotkey thread (`keyboard.rs:422` → `capture_via_hotkey` → `apply_pairs_to_glossary`) and the daemon UDS server thread (`uds_server.rs:761,843,857`). Two near-simultaneous captures both read the old file and both overwrite → one update silently lost. Worse, `std::fs::write` truncates-then-writes: a crash/power-loss mid-write leaves a truncated/empty/half-written file. `glossary.rs::load_entries` (line 206-207) then fails to parse it, `entries()` (line 45-48) catches the error and falls back to `Vec::new()` — silently disabling ALL vocabulary corrections. The user's curated dictionary is destroyed with only a `warn!` in the log.
- problem:
  ```rust
  let json = serde_json::to_string_pretty(&entries)?;
  std::fs::write(&path, json).context("write glossary.json")?;
  ```
- fix: Atomic replace via temp-file + rename (crash-safe on same fs), and serialize the read-modify-write across both functions with a process-wide `Mutex` (mirroring `CorrectionQueue`'s lock) so concurrent captures can't clobber each other:
  ```rust
  let tmp = path.with_extension("json.tmp");
  std::fs::write(&tmp, json).context("write glossary.json.tmp")?;
  std::fs::rename(&tmp, &path).context("atomic replace glossary.json")?;
  ```

### [HIGH] Pending-corrections queue persistence is non-atomic — crash mid-write silently discards the whole review queue
- file: src/always/correction_queue.rs:203-205 (`persist_locked`)
- category: data-loss
- confidence: high
- impact: `persist_locked` runs on every `add`/`take`/`clear`/prune (called from `clipboard_watcher.rs:96`, `uds_server.rs:743-777`, `main.rs:493-537`) and does a bare `std::fs::write` that truncates `pending_corrections.json` first. A crash/OOM-kill/power-loss between truncate and full write leaves a partial/empty file. On next start `load_from_disk` (line 214) fails to parse, logs a warning, returns `None` → `unwrap_or_default()` (line 101) → the entire pending review queue is silently dropped. All unreviewed corrections lost.
- problem:
  ```rust
  if let Err(e) = std::fs::write(&inner.persistence_path, json) {
      tracing::error!(error = %e, path = %inner.persistence_path.display(), "correction_queue_persist_failed");
  }
  ```
- fix: temp-file + atomic rename, same pattern as the glossary fix:
  ```rust
  let tmp = inner.persistence_path.with_extension("json.tmp");
  if std::fs::write(&tmp, &json).is_ok() {
      let _ = std::fs::rename(&tmp, &inner.persistence_path);
  }
  ```

### [MEDIUM] Queue capacity cap is not enforced on load — an oversized on-disk file stays unbounded in memory
- file: src/always/correction_queue.rs:209-225 (`load_from_disk`) + 117-144 (`add`)
- category: resource-leak
- confidence: medium
- impact: `MAX_PENDING` (100) is enforced only inside `add`, and only after the duplicate-check early-return (lines 119-126). `load_from_disk` inserts every entry from disk with no cap (lines 220-223). If `pending_corrections.json` was hand-edited or grew via a prior bug to thousands of entries, they all load into memory and get persisted back unbounded; the cap is then re-applied only one entry at a time as new *distinct* pairs arrive, and never on duplicate adds or `take`. The queue can sit far over capacity indefinitely.
- problem:
  ```rust
  let mut map = HashMap::with_capacity(entries.len());
  for e in entries {
      map.insert(e.id, e);   // no MAX_PENDING bound
  }
  ```
- fix: After loading, if `map.len() > MAX_PENDING`, drop the oldest by `queued_at_unix_ms` down to the cap and persist the trimmed result (e.g. in `with_path` after `load_from_disk`, alongside the existing `prune_stale_in_place` call).

### [MEDIUM] `now_unix_ms` clamps clock errors to 0 — new entries get a 1970 timestamp and are immediately pruned as stale
- file: src/always/correction_queue.rs:64-69 (`now_unix_ms`), used at line 58 (`PendingCorrection::new`) and 168 (prune)
- category: data-loss
- confidence: medium
- impact: If `SystemTime::now()` is before `UNIX_EPOCH` (clock skew, NTP step-back, VM time travel), `duration_since` errors and `now_unix_ms` returns `0`. On the add path that stamps `queued_at_unix_ms = 0` (epoch 1970), so the entry is instantly eligible for staleness pruning (`>= cutoff` fails, since cutoff is `now - 7d`) and is silently dropped on the next load before the user ever sees it. Asymmetric and data-losing specifically on insert.
- problem:
  ```rust
  .map(|d| d.as_millis() as u64)
  .unwrap_or(0)
  ```
  A `0` timestamp is indistinguishable from "queued at the dawn of time" and treated as stale by `prune_stale_in_place`.
- fix: Don't fabricate `0` on the add path; guard prune so `queued_at_unix_ms == 0` is never auto-pruned, or carry a monotonic fallback. At minimum: `inner.pending.retain(|_, p| p.queued_at_unix_ms == 0 || p.queued_at_unix_ms >= cutoff);`

### [MEDIUM] `set_preference` interpolates the column name into SQL via `format!` — allow-list is the only thing preventing injection
- file: src/db.rs:412 (`set_preference`)
- category: security
- confidence: medium
- impact: `let sql = format!("UPDATE preferences SET {key} = ?1 WHERE id = 1");` interpolates `key` directly into SQL. It is safe today ONLY because of the `valid_keys` allow-list check at line 290. This is a defense-in-depth gap: any future call path reaching the UPDATE without that guard (refactor, new entry point such as `uds_server.rs:696` which forwards a wire value) becomes a SQL-injection vector. The *value* is parameterized (`?1`); the *column name* is not.
- problem:
  ```rust
  if !valid_keys.contains(&key) { anyhow::bail!(...); }      // line 290 — the only guard
  ...
  let sql = format!("UPDATE preferences SET {key} = ?1 WHERE id = 1");  // line 412
  ```
- fix: Interpolate only a literal pulled from the static array, so no caller string can ever reach the SQL:
  ```rust
  let col = valid_keys.iter().find(|&&k| k == key).copied()
      .expect("key validated against valid_keys above");
  let sql = format!("UPDATE preferences SET {col} = ?1 WHERE id = 1");
  ```

### [MEDIUM] `i64`→`u32` preference reads silently wrap negative / oversized values into nonsense config
- file: src/db.rs:243-244 (`get_preferences`: `auto_enter_delay_ms`, `idle_pause_secs`)
- category: correctness
- confidence: medium
- impact: `row.get::<_, Option<i64>>(19)?.map(|v| v as u32)` casts straight to `u32`. A negative or out-of-range value (DB corruption, manual edit, or any future writer that bypasses validation) becomes a huge positive `u32` — e.g. `-1` → 4_294_967_295 ms ≈ 49.7 days of auto-enter delay; `idle_pause_secs` similarly wraps. `set_preference` validates on write, but `get_preferences` trusts the stored value blindly, so out-of-band writes silently produce broken timing config.
- problem:
  ```rust
  auto_enter_delay_ms: row.get::<_, Option<i64>>(19)?.map(|v| v as u32),
  idle_pause_secs: row.get::<_, Option<i64>>(20)?.map(|v| v as u32),
  ```
- fix: Clamp on read to the same bounds the write path enforces (60_000 / 86_400):
  ```rust
  auto_enter_delay_ms: row.get::<_, Option<i64>>(19)?.map(|v| v.clamp(0, 60_000) as u32),
  idle_pause_secs:     row.get::<_, Option<i64>>(20)?.map(|v| v.clamp(0, 86_400) as u32),
  ```

### [LOW] `looks_like_correction` uses byte length while Levenshtein counts chars — multibyte words misjudged
- file: src/always/correction.rs:71-75 (vs. char-based `strsim::levenshtein` at line 80)
- category: correctness
- confidence: medium
- impact: `wrong.len()`/`right.len()` are byte lengths; for accented/non-ASCII tokens (`café` = 5 bytes / 4 chars; Cyrillic/CJK far more) the `MIN_WORD_LEN`, `MIN_LEN_RATIO`, and `max_len/MIN_DIST_DIVISOR` math skew while `strsim::levenshtein` (line 80) counts chars. The two metrics disagree, so the similarity gate accepts/rejects non-English corrections inconsistently — a real short accented correction can be wrongly rejected, or two unrelated multibyte words wrongly accepted (polluting the glossary). Not a crash (slicing is fine here), but a correctness gap for non-English users.
- problem:
  ```rust
  if wrong.len() < MIN_WORD_LEN || right.len() < MIN_WORD_LEN { return false; }
  let max_len = wrong.len().max(right.len());
  let min_len = wrong.len().min(right.len());
  ```
- fix: Use `chars().count()` so the length metrics agree with the char-based distance:
  ```rust
  let wl = wrong.chars().count();
  let rl = right.chars().count();
  if wl < MIN_WORD_LEN || rl < MIN_WORD_LEN { return false; }
  let max_len = wl.max(rl);
  let min_len = wl.min(rl);
  ```

### [LOW] `prune_stale_in_place` uses non-saturating subtraction — latent panic if the function is ever refactored
- file: src/always/correction_queue.rs:169-171
- category: crash-panic
- confidence: low
- impact: `let pruned = before - inner.pending.len();` is safe today only because `retain` can only shrink the map. It is a latent subtract-overflow (panic in debug, wrap in release) the moment the body is edited to also insert — and a debug-build panic here would kill the always-on daemon.
- problem:
  ```rust
  let before = inner.pending.len();
  inner.pending.retain(|_, p| p.queued_at_unix_ms >= cutoff);
  let pruned = before - inner.pending.len();
  ```
- fix: `let pruned = before.saturating_sub(inner.pending.len());`

### [LOW] `apply_pairs_to_glossary` silently discards a malformed `mistranscriptions` field
- file: src/always/correction.rs:312-322 (`apply_pairs_to_glossary`)
- category: data-loss
- confidence: low
- impact: If an existing entry's `mistranscriptions` is present but not an array (hand-edited to a string, or schema drift), the code overwrites it with an empty array, discarding whatever was there, with no log.
- problem:
  ```rust
  None => {
      entry["mistranscriptions"] = serde_json::Value::Array(Vec::new());  // silently drops old value
      entry.get_mut("mistranscriptions").and_then(|v| v.as_array_mut()).expect("just inserted")
  }
  ```
- fix: Log at warn level before overwriting a non-array `mistranscriptions`, and consider preserving a scalar value by wrapping it into the new array.

### [LOW] `migrate` infers "column missing" from any `prepare` failure — a non-"no such column" error triggers a duplicate-column ALTER that crashes startup
- file: src/db.rs:71-211 (every `has_*` probe)
- category: error-handling
- confidence: low
- impact: Each migration step detects a missing column via `prepare("SELECT col ... LIMIT 0").is_ok()`. Any prepare failure (not just "no such column" — e.g. transient lock, disk error) is read as "column absent", triggering `ALTER TABLE ADD COLUMN`. If the column actually exists, the ALTER fails with "duplicate column name" and `?` aborts `open()`, taking down the daemon at startup. Low probability, but the failure mode is a startup crash rather than a clean retry. Also ~15 prepare round-trips run on every `open()`.
- problem:
  ```rust
  let has_groq_api_key = conn.prepare("SELECT groq_api_key FROM preferences LIMIT 0").is_ok();
  if !has_groq_api_key { conn.execute_batch("ALTER TABLE ... ADD COLUMN groq_api_key TEXT;")?; }
  ```
- fix: Read the actual column set once via `PRAGMA table_info(preferences)` and add only genuinely-absent columns, or drive migrations off `PRAGMA user_version`. Cheaper and robust against non-schema prepare errors.

---

## Files reviewed clean (no defects found beyond the above)
- `src/glossary.rs` — load/sort/prompt-build logic is sound: parse errors are caught and fall back to empty (glossary.rs:45-48), prompt builders handle empty input, dedup is case-insensitive. (Note: it is the *reader* victim of the non-atomic glossary writes flagged in correction.rs; the fix belongs on the write side.)
