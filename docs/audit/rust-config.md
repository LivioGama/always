# Config Audit — Always voice-to-text daemon

Scope: `src/always/config.rs`, `src/config.rs`, `src/always/per_app.rs`, `src/always/localization.rs`
(supporting context read: `src/db.rs`, `src/main.rs`, `src/always/uds_server.rs`)

Note: `db::open()` already enables `PRAGMA journal_mode=WAL` (db.rs:53), and `db::set_preference` already calls `per_app::invalidate_cache()` for the `per_app_settings_json` key (db.rs:431-433). Findings below account for that.

---

### [HIGH] Cross-process per-app cache is never invalidated — the running daemon serves a stale allowlist after a CLI `config set`/`config reset`
- file: src/always/per_app.rs:56-83, 63-65
- category: correctness
- confidence: high
- impact: After the user edits per-app settings through the CLI (`always config set per_app_settings_json ...`) or `always config reset`, the already-running daemon keeps applying the OLD allowlist until it is restarted — e.g. an app the user just "resumed" stays paused, or a removed override keeps voice typing active. The bug is silent and survives indefinitely.
- problem: `invalidate_cache()` only clears the calling process's static `CACHE`. The CLI and the daemon are separate processes:
```rust
static CACHE: LazyLock<RwLock<Option<AppOverrides>>> = LazyLock::new(|| RwLock::new(None));
pub fn invalidate_cache() { *CACHE.write() = None; }
```
  The module doc claims invalidation is wired "from the `set_preference` write path"; that is true, but `set_preference` runs in whichever process called it. When `main.rs:417 db::set_preference(&conn, &key, &value)` runs in the CLI process, it invalidates the CLI's cache only. The daemon's `CACHE` is untouched, so `load()` keeps returning the cached snapshot. (The UDS write path in `uds_server.rs:599` works because it runs inside the daemon; the CLI path does not.)
- fix: Make cross-process edits reach the daemon. Cheapest: store a monotonically increasing revision integer alongside the blob and have `load()` compare it (re-read when changed). Or have the CLI send the daemon a UDS "reload prefs" command. Or, since the blob is tiny, drop the in-memory cache and re-read per focus change (the cache only saved a few SQLite reads per focus session). At minimum, document that CLI per-app edits require a daemon restart.

---

### [HIGH] Per-app override DB fields read from prefs are unvalidated — corrupt/out-of-range stored values silently break VAD or paste with no clamp
- file: src/always/config.rs:373-394
- category: correctness
- confidence: high
- impact: `set_preference` validates values on the *CLI/UDS write path*, but `AlwaysConfig::from_cli` reads several prefs back with NO validation. If the DB row is corrupted (partial write, manual edit, schema/version skew, or a value written before validation existed), an out-of-range `silero_threshold`/`energy_threshold`/`cooldown_ms` is accepted verbatim and the daemon silently misbehaves: a `silero_threshold` outside `[0,1]` makes the VAD never (or always) trigger → no transcription or constant transcription; a huge `cooldown_ms` effectively disables dictation. None surfaces an error.
- problem: Only `silence_secs` is clamped; the rest are taken raw and `silero_threshold` is cast `as f32` which can carry `NaN`/`Inf` (e.g. a stored `"nan"`), making all VAD comparisons silently false:
```rust
silence_secs: prefs.stt_silence.unwrap_or(silence_secs).clamp(1.0, 15.0),
...
energy_threshold: prefs.stt_energy_threshold.unwrap_or(0.012),     // unclamped
cooldown_ms: prefs.stt_cooldown_ms.unwrap_or(800),                 // unclamped
silero_threshold: prefs.silero_threshold.unwrap_or(0.5) as f32,    // unclamped; can be NaN/Inf/>1
auto_enter_delay_ms: prefs.auto_enter_delay_ms.unwrap_or(...),     // unclamped
idle_pause_secs: prefs.idle_pause_secs.unwrap_or(...),             // unclamped
```
- fix: Clamp/validate each field at load time, mirroring the bounds `set_preference` already enforces (db.rs:307-388): `silero_threshold` → reject non-finite then `.clamp(0.1, 0.9)`; `energy_threshold` → `.clamp(0.0, 1.0)`; `cooldown_ms` → `.min(5000)`; `auto_enter_delay_ms` → `.min(60_000)`; `idle_pause_secs` → `.min(86_400)`. Defense-in-depth: the read path must not trust the DB just because the write path validated.

---

### [MEDIUM] Per-app blob write is a read-modify-write of a single column with no transaction — concurrent daemon + CLI writes lost-update each other
- file: src/always/per_app.rs:176-189
- category: data-loss
- confidence: medium
- impact: The per-app allowlist lives in ONE `per_app_settings_json` column rewritten in full. The daemon (via UDS `SetAppPaused`) and the CLI (`always config set per_app_settings_json ...`) can both do load→mutate→serialize→store concurrently. With no transaction wrapping the read-modify-write, two interleaved writers are last-writer-wins: one app's resume/pause toggle is silently discarded.
- problem:
```rust
let mut overrides = load();                 // read (possibly from stale cache)
let entry = overrides.entry(...).or_default();
entry.paused = paused;                       // mutate
...
let json = serde_json::to_string(&overrides)?;
let conn = crate::db::open()?;
crate::db::set_preference(&conn, "per_app_settings_json", &json)?;   // store whole blob
invalidate_cache();
```
  `load()` may even return a cached snapshot that predates a concurrent writer's change, widening the lost-update window beyond the bare SQLite write.
- fix: Wrap read-modify-write in a single SQLite transaction (`BEGIN IMMEDIATE`), re-reading the blob fresh from the DB inside the transaction (not from the in-memory cache) before merging the single key, then committing. With WAL already on, `BEGIN IMMEDIATE` plus a `busy_timeout` serializes the two writers instead of clobbering.

---

### [MEDIUM] No `busy_timeout` configured — concurrent daemon/CLI writes can fail `SQLITE_BUSY` and the toggle is silently dropped
- file: src/db.rs:47-56 (open); consumed at src/always/per_app.rs:186-189
- category: concurrency
- confidence: medium
- impact: WAL allows one writer + many readers, but a second concurrent *writer* still blocks. With no `busy_timeout`, a write under contention returns `SQLITE_BUSY` immediately. `set_app_paused_override` propagates the error, but its own doc says callers are "best-effort", and `uds_server.rs:599` only logs — so the user's per-app toggle silently does nothing under contention.
- problem: `db::open()` sets WAL but never a busy timeout:
```rust
let conn = Connection::open(&path).context("Failed to open database")?;
conn.execute_batch("PRAGMA journal_mode=WAL;")?;
migrate(&conn)?;
```
- fix: Add `conn.busy_timeout(std::time::Duration::from_secs(5))?;` in `db::open()` so short pref writes wait for the lock rather than failing. (Out of assigned scope file-wise but directly governs the config write path; flagging for the maintainer.)

---

### [MEDIUM] Invalid per-app JSON is silently swallowed and replaced with an empty allowlist
- file: src/always/per_app.rs:97
- category: data-loss
- confidence: high
- impact: If `per_app_settings_json` is ever malformed (partial write, manual edit, version skew), the user's entire allowlist is silently discarded — every app reverts to paused with zero log/notification. The next `set_app_paused_override` then serializes the now-empty map back, making the loss permanent.
- problem:
```rust
serde_json::from_str(&raw).unwrap_or_default()
```
  `unwrap_or_default()` turns any parse error into an empty map with no logging. (The write path validates JSON shape via `serde_json::Value`, but a value written by an older schema or a partial/truncated blob will still fail to deserialize into `AppOverrides` here.)
- fix: Log `tracing::error!(len = raw.len(), %err, "per_app_settings_json parse failed")` on error, and avoid immediately overwriting — e.g. stash the corrupt blob under a backup key before the next write so it is recoverable. At minimum make the silent allowlist loss observable in logs.

---

### [MEDIUM] `lang` passed to `from_cli` is never validated against `SUPPORTED_LANGS`
- file: src/always/config.rs:292-360; allowlist at src/config.rs:5-7
- category: error-handling
- confidence: medium
- impact: `set_preference("lang", ...)` validates against `SUPPORTED_LANGS` (db.rs:299-306), but `AlwaysConfig::from_cli(lang, ...)` stores the CLI `--lang` value verbatim with no check. `--lang gibberish` flows straight to the STT backend, producing a confusing downstream error or wrong-language transcription instead of a clear startup error. Inconsistent with the write path that does validate.
- problem: `from_cli` does `let config = Self { lang, ... }` with no membership test against:
```rust
pub const SUPPORTED_LANGS: &[&str] = &["en","fr","es","de","it","pt","zh","ja","ko","ru","ar","nl"];
```
- fix: At the top of `from_cli`, normalize and validate: `let lang = lang.to_lowercase(); if !crate::config::SUPPORTED_LANGS.contains(&lang.as_str()) { anyhow::bail!("unsupported language '{lang}'; supported: {:?}", crate::config::SUPPORTED_LANGS); }`. Fail fast with an actionable message.

---

### [LOW] `localization` is hard-wired to English regardless of selected `lang`
- file: src/always/config.rs:398, 438
- category: correctness
- confidence: high
- impact: Even when the user selects `fr`/`de`/etc., post-processing casing/terminator heuristics always use `Localization::ENGLISH`. The field and `localization.rs` are built to be locale-aware, but `from_cli`/`default` never map `lang` → `Localization`, so the documented "non-English users get correct merge-time casing without code changes" promise is not delivered for any actual selection.
- problem:
```rust
// English is the only built-in locale today...
localization: Localization::ENGLISH,
```
  with no mapping from `lang`.
- fix: Add `Localization::for_lang(&str) -> Localization` (falling back to ENGLISH) in `localization.rs` and set `localization: Localization::for_lang(&lang)`. Even with only English heuristics today, this removes the silent mismatch and makes adding a locale a one-line change.

---

### [LOW] `config_dir()` / `db_path()` fall back to `"."` (CWD) when the OS config dir can't be resolved — DB created in an unpredictable directory
- file: src/config.rs:9-23
- category: correctness
- confidence: medium
- impact: If `dirs::config_dir()` returns `None`, the DB path becomes `./always/always.db`, relative to the daemon's cwd. The daemon is launched from `/Applications` (per project docs), so it could attempt to write into a read-only location, or create a stray `always.db` wherever cwd happens to be — yielding "lost"/divergent settings per launch directory. `db::open` creates parents via `create_dir_all`, so this silently materializes the wrong path rather than erroring.
- problem:
```rust
dirs::config_dir()
    .unwrap_or_else(|| PathBuf::from("."))
    .join("always")
```
- fix: Fall back to an absolute writable path (`dirs::home_dir()? .join(".config/always")`) or hard-error rather than silently using a cwd-relative path. At minimum log a warning when the fallback fires.

---

### [LOW] `ALWAYS_CONFIG_DIR` / `ALWAYS_DB_PATH` env paths are not expanded (`~` / `$VAR` taken literally)
- file: src/config.rs:9-23
- category: ux
- confidence: medium
- impact: A user setting `ALWAYS_CONFIG_DIR=~/cfg` or `ALWAYS_DB_PATH=$HOME/db/always.db` gets a literal directory named `~` (or `$HOME`) created, because `PathBuf::from(p)` does no shell expansion. Settings appear to vanish.
- problem:
```rust
if let Ok(p) = std::env::var("ALWAYS_CONFIG_DIR") {
    return PathBuf::from(p);   // no ~ / env expansion
}
```
- fix: Expand `~`/env vars (e.g. via `shellexpand`, or manual `home_dir()` substitution) before constructing the `PathBuf`, or document explicitly that only absolute paths are accepted.

---

### [LOW] Env-var STT/transcriber config silently swallows malformed values — misconfiguration is undiagnosable from logs
- file: src/always/config.rs:308-311, 561-573; vocab loaders 443-491
- category: error-handling
- confidence: medium
- impact: `ALWAYS_VAD_MODE`, `ALWAYS_TRANSCRIBER`, and the numeric/JSON vocab env vars use `.parse().ok()` / `from_str(...).ok()` and silently fall back to defaults on a typo. A user who sets `ALWAYS_TRANSCRIBER=grok` (typo for `groq`) silently gets the default backend. Inconsistent: `resolve_transcriber_backend`'s DB branch logs `ignoring_invalid_transcriber_backend`, but the env branch above it does not.
- problem:
```rust
if let Ok(env_val) = std::env::var("ALWAYS_TRANSCRIBER")
    && let Ok(parsed) = env_val.parse() { return parsed; }   // set-but-unparseable → silent default
...
let vad_mode = std::env::var("ALWAYS_VAD_MODE").ok().and_then(|s| s.parse().ok()).unwrap_or_default();
```
- fix: When an env var is *set but unparseable*, emit a `tracing::warn!` (matching the DB branch) instead of silently using the default, so misconfiguration is visible in logs.

---

### [LOW] `detect_project_root()` walks the filesystem to `/` inside `Default::default()` — surprising side effect, follows symlinks, swallows permission errors
- file: src/always/config.rs:427, 515-532
- category: performance
- confidence: low
- impact: Minor. `AlwaysConfig::default()` calls `detect_project_root()`, which does up to N `path.join(".git").exists()` syscalls walking to root. A `Default` impl doing filesystem walks is a surprising side effect; `.exists()` also follows symlinks and treats a permission-denied parent as "no `.git`", so a real repo behind an unreadable dir is silently missed.
- problem:
```rust
let mut path = current.clone();
while path.pop() {
    if path.join(".git").exists() { return Some(path); }
}
```
- fix: Acceptable for correctness; if it shows in profiling, cache the result. Consider resolving `project_root` lazily rather than in `Default::default()` so `default()` has no I/O side effects.
