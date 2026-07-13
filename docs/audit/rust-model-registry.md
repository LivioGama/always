# Audit: model-registry scope

Files reviewed (read in full with the Read tool):
- `src/managers/model_registry.rs` (1262 lines)
- `src/always/keyring.rs` (133 lines)

Caller tracing done via grep (results captured before shell became intermittent):
- `verify_sha256(...)` / `compute_sha256(...)` have exactly ONE call site each, both
  inside `ModelRegistry::download()` (`model_registry.rs:484` / inside `verify_sha256`).
  No verification exists for files that were already on disk.
- `model_path()` is consumed by `src/stt_dispatch.rs:193` (the local-STT loader), which
  gates only on `info.is_downloaded` (`src/stt_dispatch.rs:157`) — a flag that
  `refresh_disk_status` sets purely from file existence, never from a hash.
- `registry.download(...)` is invoked from `src/always/uds_server.rs:641`.

The uncommitted Handy-cache-reuse change (confirmed via `git diff`) adds
`handy_models_dir()` + `path_matches_kind()` and makes both `model_path` and
`refresh_disk_status` treat a file in `~/Library/Application Support/com.pais.handy/models/`
as a usable, downloaded model.

---

### [CRITICAL] Handy-cached / pre-existing on-disk model files are loaded with NO SHA256 verification
- file: src/managers/model_registry.rs:222-241 (model_path), 255-284 (refresh_disk_status)
- category: security
- confidence: high
- impact: A model file already present in always's own models dir, or in Handy's cache dir (`~/Library/Application Support/com.pais.handy/models/<filename>`), is marked `is_downloaded = true` and handed to the STT engine loader (`stt_dispatch.rs:193`) with zero integrity check. SHA256 is only ever computed on bytes this process downloads itself (sole call site: `verify_sha256` at line 483-491 inside `download()`). Any file that another process, a corrupted Handy download, or a local attacker can place at that path with the right filename is loaded verbatim. The Handy cache is a directory always does NOT own and never created, so its contents are entirely outside always's trust boundary — yet the new reuse path loads them as trusted. This is the exact concern flagged in the task scope, and the answer is: NO, a Handy-cached file is not SHA-verified before use.
- problem: `refresh_disk_status` decides "downloaded" from existence + kind only, never hashing:
  ```rust
  let own_has_it = path_matches_kind(&model_path, model.is_directory);
  let handy_has_it = handy
      .as_ref()
      .map(|h| path_matches_kind(&h.join(&model.filename), model.is_directory))
      .unwrap_or(false);
  model.is_downloaded = own_has_it || handy_has_it;
  ```
  and `model_path` returns the Handy candidate the moment it exists:
  ```rust
  if let Some(handy) = handy_models_dir() {
      let candidate = handy.join(&m.filename);
      if path_matches_kind(&candidate, m.is_directory) {
          return Some(candidate);
      }
  }
  ```
  The loader trusts that flag:
  ```rust
  // src/stt_dispatch.rs:157
  if !info.is_downloaded { /* refuse */ }
  // src/stt_dispatch.rs:193
  .model_path(&info.id)   // -> loaded into the engine, no hash re-check
  ```
  The catalog's pinned SHA256 (and the comment at line 769-771 claiming "the same SHA256 keeps the download safe") only protects the self-download path; it provides ZERO protection for the reuse path the diff just added.
- fix: Verify the catalog SHA256 before treating any pre-existing file as usable. Single-file `.bin` models can be re-hashed against the catalog hash; directory (tar.gz) models cannot (the catalog hash is over the *archive*, not the extracted dir), so for those drop a `.verified` marker when always itself extracts them and only trust dirs that carry it — never blindly trust Handy's extracted dir. Sketch for `refresh_disk_status`:
  ```rust
  let verified = if model.is_directory {
      own_has_it && self.models_dir.join(format!("{}.verified", model.filename)).exists()
  } else {
      let p = if own_has_it { Some(model_path.clone()) }
              else if handy_has_it { handy.as_ref().map(|h| h.join(&model.filename)) }
              else { None };
      match (p, model.sha256.as_deref()) {
          (Some(p), Some(exp)) => compute_sha256(&p).map(|a| a == exp).unwrap_or(false),
          (Some(_), None)      => true,  // user custom .bin, no catalog hash
          _                    => false,
      }
  };
  model.is_downloaded = verified;
  ```
  Cache the hash by (size, mtime) so a 1.6 GB file isn't re-hashed on every refresh. At absolute minimum, hash the Handy-cached file once on first use and refuse to load on mismatch.

---

### [HIGH] tar.gz extraction is vulnerable to path traversal (zip-slip / tar-slip)
- file: src/managers/model_registry.rs:513-516
- category: security
- confidence: high
- impact: `tar::Archive::unpack` writes entry paths as-is and does NOT sanitize `..` components or absolute paths. A malicious/corrupted tar.gz (or a MITM if TLS is ever weakened, or a future self-hosted bucket with a wrong/missing catalog hash) can write outside the extraction dir into the user's home — overwriting `~/.zshrc`, a LaunchAgent plist, etc. = persistence / RCE on a daemon that runs continuously. The pinned catalog SHA mitigates this for current catalog entries (archive bytes must match Handy's known-good archive), but that is the only line of defense and it does not cover (a) any model whose catalog SHA is absent/wrong, or (b) a future own-bucket scenario. An archive extractor that writes into a user's home directory must validate entry paths regardless.
- problem:
  ```rust
  let tar_gz = File::open(&partial_path)?;
  let tar = GzDecoder::new(tar_gz);
  let mut archive = Archive::new(tar);
  if let Err(e) = archive.unpack(&temp_extract_dir) {
  ```
  No per-entry path check, no `set_preserve_permissions(false)`.
- fix: Iterate entries and reject unsafe paths (or use `entry.unpack_in`, which skips escaping entries):
  ```rust
  let mut archive = Archive::new(tar);
  for entry in archive.entries()? {
      let mut entry = entry?;
      let path = entry.path()?;
      if path.is_absolute()
          || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
          let _ = fs::remove_dir_all(&temp_extract_dir);
          let _ = fs::remove_file(&partial_path);
          self.extracting_models.lock().remove(model_id);
          return Err(anyhow::anyhow!("archive contains unsafe path: {}", path.display()));
      }
      // unpack_in returns Ok(false) and skips anything that would escape:
      if !entry.unpack_in(&temp_extract_dir)? {
          return Err(anyhow::anyhow!("archive entry escapes extraction dir"));
      }
  }
  ```

---

### [MEDIUM] `delete_secret` writes an EMPTY password instead of deleting the keyring entry — a "deleted" key becomes `Some("")` and masks the no-key state
- file: src/always/keyring.rs:85-94 (delete_secret), 44-53 (get_deepgram_api_key), 23-30 (get_groq_api_key)
- category: security
- confidence: high
- impact: "Deleting" a Groq/Deepgram key does not remove the macOS Keychain item; it overwrites it with `""`. The credential persists in the keychain (still visible in Keychain Access; "forget my key" lies). Worse, the getters never treat `Some("")` as "no key": `get_deepgram_api_key` returns `Ok(Some(""))`, and `get_groq_api_key` returns `Ok(Some(""))` whenever the env var is unset. Downstream STT code then believes a key is configured and sends an empty Authorization to Groq/Deepgram, producing 401 loops instead of a clean "no key configured" path.
- problem:
  ```rust
  fn delete_secret(account: &str) -> Result<()> {
      let entry = Entry::new(SERVICE_NAME, account)?;
      // Try to delete by setting to empty string (keyring 2.3 doesn't have delete_credential)
      match entry.set_password("") {
  ```
  ```rust
  match get_secret(GROQ_API_KEY_ACCOUNT) {
      Ok(Some(key)) => Ok(Some(key)),   // empty string passes straight through
  ```
- fix: Use the real delete API (keyring 2.x: `Entry::delete_password()`; keyring 3.x: `delete_credential()` — check the version pinned in Cargo.toml) and map empty to None in the getters:
  ```rust
  match entry.delete_password() {
      Ok(()) => tracing::info!(account, "Secret deleted from keyring"),
      Err(KeyringError::NoEntry) => tracing::debug!(account, "Secret not found in keyring"),
      Err(e) => return Err(e).context("Failed to delete secret from keyring"),
  }
  ```
  ```rust
  Ok(Some(k)) if !k.is_empty() => Ok(Some(k)),
  Ok(_) => Ok(None),
  ```

---

### [MEDIUM] `delete()` swallows all filesystem errors, so a failed delete reports success and Handy-reused models reappear
- file: src/managers/model_registry.rs:304-316
- category: error-handling
- confidence: high
- impact: If removal fails (permissions, file mmap'd/busy because the model is the currently-loaded engine, partially-removed directory), `delete()` still returns `Ok(())`. The user/UI is told the delete succeeded while the file is still present and `is_downloaded` flips back to `true` on the immediate `refresh_disk_status`. A half-removed directory (some files gone, dir remains) still passes `path_matches_kind` and gets loaded as a corrupt model. Additionally, `delete()` only touches always's own models dir — it never removes the Handy cache copy, so deleting a Handy-reused model is a no-op and `refresh_disk_status` instantly re-marks it downloaded: the model "won't delete."
- problem:
  ```rust
  if path.is_dir() {
      fs::remove_dir_all(&path).ok();   // error discarded
  } else if path.is_file() {
      fs::remove_file(&path).ok();      // error discarded
  }
  if partial.exists() {
      fs::remove_file(&partial).ok();   // error discarded
  }
  self.refresh_disk_status()?;          // re-marks Handy copy as downloaded
  ```
- fix: Propagate removal errors with context; and either refuse to "delete" a Handy-cache-only model (it isn't ours) or surface that it can't be removed:
  ```rust
  if path.is_dir() {
      fs::remove_dir_all(&path).with_context(|| format!("remove model dir {}", path.display()))?;
  } else if path.is_file() {
      fs::remove_file(&path).with_context(|| format!("remove model file {}", path.display()))?;
  }
  ```

---

### [MEDIUM] Concurrent `download()` of the same model id races: corrupt `.partial` and broken cancellation
- file: src/managers/model_registry.rs:359-373, 418-425
- category: concurrency
- confidence: medium
- impact: `download()` is `pub async` and `ModelRegistry` is `Clone` and shared (UDS server at `uds_server.rs:641`, dispatch task). Nothing prevents two concurrent `download("turbo")` calls (UI double-click, retry while in flight). Both open/append the same `<filename>.partial`, interleaving bytes into a corrupt file (caught later by SHA, wasting a multi-GB download), and the second call overwrites the first's `Arc<AtomicBool>` cancel flag (line 364-366). A later `cancel_download` then cancels only one task, and whichever `DownloadCleanup::drop` runs first removes the shared cancel-flag entry, defeating cancellation of the other.
- problem: the `is_downloading` flag is set but never used as an admission guard, and the cancel-flag insert clobbers:
  ```rust
  if let Some(m) = self.available_models.lock().get_mut(model_id) {
      m.is_downloading = true;
  }
  let cancel_flag = Arc::new(AtomicBool::new(false));
  self.cancel_flags
      .lock()
      .insert(model_id.to_string(), cancel_flag.clone());  // overwrites existing
  ```
- fix: make the check-and-set atomic and bail if already in flight:
  ```rust
  {
      let mut models = self.available_models.lock();
      let m = models.get_mut(model_id)
          .ok_or_else(|| anyhow::anyhow!("Model not found: {model_id}"))?;
      if m.is_downloading {
          return Err(anyhow::anyhow!("download already in progress for {model_id}"));
      }
      m.is_downloading = true;
  }
  ```
  (insert the cancel flag only after winning that check.)

---

### [MEDIUM] Disk-write / flush failures during download do not emit `DownloadFailed`, so a subscribed UI can hang on "downloading"
- file: src/managers/model_registry.rs:448, 459
- category: error-handling
- confidence: medium
- impact: `file.write_all(&chunk)?` (line 448) and `file.flush()?` (line 459) propagate IO errors (e.g. ENOSPC on a full disk — realistic for a 1.7 GB Cohere model) directly out of `download()` WITHOUT calling `emit_failed`, unlike the network branch (line 384) and stream branch (line 444). The UI's view of `is_downloading` is driven by broadcast events, not re-polling; the `DownloadCleanup` guard resets the in-memory flag but emits no event, so a connected GUI can sit on a stuck progress bar forever.
- problem:
  ```rust
  file.write_all(&chunk)?;   // line 448 — no emit_failed
  ...
  file.flush()?;             // line 459 — no emit_failed
  ```
- fix:
  ```rust
  if let Err(e) = file.write_all(&chunk) {
      self.emit_failed(model_id, &format!("disk write error: {e}"));
      return Err(e.into());
  }
  ...
  if let Err(e) = file.flush() {
      self.emit_failed(model_id, &format!("disk flush error: {e}"));
      return Err(e.into());
  }
  ```

---

### [MEDIUM] Resume can append to a stale prefix when the server returns 206 without honoring the requested range
- file: src/managers/model_registry.rs:377-413, 462-472
- category: data-loss
- confidence: medium
- impact: On resume, the existing `.partial` prefix is trusted and new bytes are appended. The "range unsupported" guard only checks for a `200 OK` response (line 390); a `206 PARTIAL_CONTENT` whose `Content-Range` does not start at `resume_from` (CDN object rotated, server bug) is accepted and appended, producing a wrong-content file. The size check (462-472) and SHA check (483-491) catch it eventually, but the genuine fix path (delete + restart) only triggers on those failures — there's no positive validation that the resumed range actually matched, so corrupted-resume detection relies entirely on downstream hashing.
- problem: only `status() == OK` is treated as "range ignored"; `Content-Range` is never inspected:
  ```rust
  if resume_from > 0 && response.status() == reqwest::StatusCode::OK {
      ... restart fresh ...
  }
  // no else-branch validating Content-Range for the 206 case
  ```
- fix: verify the 206 range start matches `resume_from`, else discard and restart:
  ```rust
  if resume_from > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
      let ok = response.headers().get(reqwest::header::CONTENT_RANGE)
          .and_then(|v| v.to_str().ok())
          .map(|cr| cr.contains(&format!("bytes {resume_from}-")))
          .unwrap_or(false);
      if !ok {
          drop(response);
          let _ = fs::remove_file(&partial_path);
          resume_from = 0;
          response = client.get(&url).send().await?;
      }
  }
  ```

---

### [LOW] `discover_custom_whisper_models` registers arbitrary `.bin` files as loadable with no validation (accepts 0-byte files)
- file: src/managers/model_registry.rs:692-765
- category: error-handling
- confidence: medium
- impact: Any `*.bin` in the models dir is advertised as a custom model with `is_downloaded: true`, `sha256: None`, including empty/truncated files (`size_mb` just becomes 0). Selecting one can crash or hang the always-on daemon when the whisper engine tries to parse it. Intentional Handy parity, but no minimal sanity check.
- problem:
  ```rust
  is_downloaded: true,
  ...
  sha256: None,
  ```
  with no size or ggml-magic check; 0-byte files are accepted.
- fix: skip implausibly small files and optionally check the ggml magic header before registering: `if size_mb == 0 { continue; }`, and ensure the engine load failure is handled gracefully rather than crashing the daemon.

---

### [LOW] Comment says "16 listeners" but channel capacity is 64, and conflates listener count with buffer size
- file: src/managers/model_registry.rs:183-186
- category: maintainability
- confidence: high
- impact: Cosmetic, but misleads anyone tuning `Lagged` behaviour: `broadcast::channel(N)` is the per-receiver message buffer, not a listener count, and the literal is 64 not 16.
- problem:
  ```rust
  // 16 listeners is plenty — UDS clients, the dispatch hot-swap
  // task, future UI watchers. Slow consumers receive `Lagged`
  // and re-sync via `list_models`.
  let (events_tx, _events_rx) = broadcast::channel(64);
  ```
- fix: correct the comment to "64 buffered events" and describe it as the lag/buffer window, not a listener count.

---

## Checked and found OK (for completeness)
- SHA256 IS enforced for SELF-downloaded files before extraction/rename: `verify_sha256` (line 483-491) runs before the extract/rename block (line 497+); mismatch deletes the partial (line 650) and returns `Err`.
- Hashing runs in `spawn_blocking` (line 483) so the async executor isn't stalled on multi-GB hashes; the `JoinError` is handled (line 487, `map_err`).
- Truncation guard before verify (line 462-472) rejects short downloads.
- `DownloadCleanup` RAII resets `is_downloading` and removes the cancel flag on every early return / panic; success path disarms it (line 559) so it doesn't clobber `is_downloaded = true`.
- Secrets are never logged: `set_secret` / `delete_secret` log only the `account` name; getters log only the error, never the key; the `GROQ_API_KEY` env read is not logged. No `tracing` call interpolates a secret value.
- `verify_sha256` returning `Ok(())` for `sha256: None` is acceptable for user-supplied custom `.bin` files (not network downloads) — but see the CRITICAL finding: it must NOT be the reason Handy-cached *catalog* models (which DO have a known SHA) skip verification.
