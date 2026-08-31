//! Local-model catalog and download manager — ported from Handy's
//! `src-tauri/src/managers/model.rs` (see `cjpais/Handy@0.8.3`).
//!
//! Differences vs the upstream port:
//!
//! * No Tauri. The progress / state events are published over a
//!   `tokio::sync::broadcast::Sender<ModelEvent>` instead of
//!   `AppHandle::emit`. The UDS server subscribes and fans events out
//!   to connected GUI clients.
//! * No `tauri-specta` derive — plain `serde` is enough; the Swift app
//!   uses the JSON shape directly.
//! * Storage path comes from `dirs::data_dir()` (which resolves to
//!   `~/Library/Application Support/always/models` on macOS) instead
//!   of Tauri's `app_data_dir`.
//! * The "auto-select on startup" and "migrate bundled models" steps
//!   are dropped: Always tracks the active backend in the prefs DB
//!   (`AlwaysConfig::transcriber_backend`) rather than in the registry
//!   itself, and ships no bundled model files in the app bundle.
//!
//! Active-model selection lives outside this module by design — the
//! registry only knows what's on disk and what's downloadable. The
//! daemon's `stt_dispatch` picks the right `Transcriber` backend
//! based on config.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::Archive;
use tokio::sync::broadcast;

/// Which transcription engine a downloaded model targets. The
/// `crate::always::stt_local::LocalTranscriber` dispatches on this
/// when loading the model into a `transcribe-rs` engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EngineType {
    Whisper,
    Parakeet,
    Moonshine,
    MoonshineStreaming,
    SenseVoice,
    GigaAM,
    Canary,
    Cohere,
    Nemotron,
}

/// One file of a multi-file directory model. See [`ModelInfo::files`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelFile {
    /// Name the file must have inside the model directory. The engine
    /// loaders look for exact names (`encoder.onnx`, `tokenizer.model`,
    /// …), so this is not cosmetic.
    pub name: String,
    pub url: String,
    /// Mandatory. A multi-file model has no single archive checksum to
    /// fall back on, so each part verifies itself.
    pub sha256: String,
    pub size_bytes: u64,
}

/// One catalog entry. Mirrors Handy's struct field-for-field so we can
/// keep the same SHA256s + URLs and let the existing Handy CDN serve
/// our downloads until we host them ourselves.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub filename: String,
    pub url: Option<String>,
    pub sha256: Option<String>,
    /// Non-empty for directory models published as loose files rather
    /// than one archive.
    ///
    /// The single-`url` path assumes one `.tar.gz` it can download and
    /// unpack. Some model hosts do not publish an archive at all —
    /// HuggingFace serves each ONNX file separately and has no
    /// repo-tarball endpoint — so pointing `url` at an invented
    /// `.tar.gz` yields a 404 and a model that can never install. When
    /// this list is non-empty it takes precedence over `url`: each file
    /// is fetched into the model directory under its exact `name` and
    /// verified individually.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ModelFile>,
    pub size_mb: u64,
    pub is_downloaded: bool,
    pub is_downloading: bool,
    pub partial_size: u64,
    pub is_directory: bool,
    pub engine_type: EngineType,
    pub accuracy_score: f32,
    pub speed_score: f32,
    pub supports_translation: bool,
    pub supports_streaming: bool,
    pub is_recommended: bool,
    pub supported_languages: Vec<String>,
    pub supports_language_selection: bool,
    pub is_custom: bool,
    /// True while the startup background pass is still SHA-verifying
    /// this model's on-disk bytes. `is_downloaded` is provisional
    /// (existence-based) until this clears; activation always re-checks
    /// the hash via `model_path`, so a corrupt file can be *listed* but
    /// never *loaded*.
    #[serde(default)]
    pub is_verifying: bool,
}

/// Streaming download progress payload. Emitted at most ~10 times per
/// second so a 1.5 GB Whisper download doesn't drown the UDS channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub model_id: String,
    pub downloaded: u64,
    pub total: u64,
    pub percentage: f64,
}

/// Events the UI / UDS server can subscribe to. Names mirror Handy's
/// Tauri event channels so any reasoning that quotes Handy's source
/// continues to apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelEvent {
    DownloadProgress(DownloadProgress),
    DownloadComplete {
        model_id: String,
    },
    DownloadCancelled {
        model_id: String,
    },
    DownloadFailed {
        model_id: String,
        error: String,
    },
    VerificationStarted {
        model_id: String,
    },
    VerificationCompleted {
        model_id: String,
    },
    ExtractionStarted {
        model_id: String,
    },
    ExtractionCompleted {
        model_id: String,
    },
    ExtractionFailed {
        model_id: String,
        error: String,
    },
    /// The active model id changed — UI should refresh badges. Carries
    /// `None` when the user switched back to the remote Groq backend.
    ActiveChanged {
        model_id: Option<String>,
    },
    /// The startup background verification pass finished re-hashing
    /// on-disk models — `is_downloaded`/`is_verifying` flags are now
    /// authoritative. The UDS bridge re-pushes the full catalog.
    DiskStatusRefreshed,
}

/// RAII cleanup for the `is_downloading` flag + cancel slot. Borrowed
/// near-verbatim from Handy; the only change is the unwrap-on-poison
/// removal because we use `parking_lot::Mutex` (cannot poison).
struct DownloadCleanup<'a> {
    available_models: &'a Mutex<HashMap<String, ModelInfo>>,
    cancel_flags: &'a Mutex<HashMap<String, Arc<AtomicBool>>>,
    model_id: String,
    disarmed: bool,
}

impl Drop for DownloadCleanup<'_> {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        if let Some(model) = self.available_models.lock().get_mut(self.model_id.as_str()) {
            model.is_downloading = false;
        }
        self.cancel_flags.lock().remove(&self.model_id);
    }
}

/// SHA256 verification verdict cache: `(path, file_len, mtime) -> matched`.
/// Aliased to keep the `ModelRegistry` field readable and satisfy
/// `clippy::type_complexity` (the CI gate runs `clippy -D warnings`).
type VerifyCache = Arc<Mutex<HashMap<(PathBuf, u64, SystemTime), bool>>>;

/// On-disk row of the persisted verify cache (`.verify-cache.json`).
///
/// mtime is stored in NANOSECONDS. It used to be whole seconds, which made
/// the cache permanently useless: the live key comes from `meta.modified()`,
/// and on APFS that carries sub-second precision, so a truncated key could
/// never equal the key it was meant to match. Every daemon start logged
/// `model_verify_cache_loaded entries=1` and then re-hashed anyway --
/// measured at 31-32s on EVERY start, 47 starts over three days, all of it
/// re-hashing a 1.08 GB Whisper checkpoint the user was not even using.
///
/// `mtime_unix_secs` is still accepted on read so an existing cache file
/// does not have to be discarded.
#[derive(Debug, Serialize, Deserialize)]
struct PersistedVerdict {
    path: PathBuf,
    len: u64,
    #[serde(default)]
    mtime_unix_secs: u64,
    #[serde(default)]
    mtime_unix_nanos: u128,
    verdict: bool,
}

/// Registry of all known local STT models. Constructed once at daemon
/// startup; cloning is cheap (only `Arc`s).
#[derive(Clone)]
pub struct ModelRegistry {
    models_dir: PathBuf,
    available_models: Arc<Mutex<HashMap<String, ModelInfo>>>,
    cancel_flags: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    extracting_models: Arc<Mutex<HashSet<String>>>,
    events_tx: broadcast::Sender<ModelEvent>,
    /// Memoized SHA256 verdicts for single-file `.bin` models, keyed by
    /// `(path, file_len, mtime)`. Re-hashing a 1.6 GB checkpoint on every
    /// `refresh_disk_status` would be unacceptable, so we cache the
    /// outcome and only re-hash when the file's size or mtime changes.
    /// `true` = hash matched the catalog SHA, `false` = mismatch/error.
    verify_cache: VerifyCache,
}

impl ModelRegistry {
    /// Build the registry from the hardcoded catalog + whatever is
    /// already on disk. Creates the models directory if missing.
    pub fn new() -> Result<Self> {
        let total_start = Instant::now();

        let models_dir = default_models_dir()?;
        fs::create_dir_all(&models_dir)
            .with_context(|| format!("create models dir at {}", models_dir.display()))?;
        let dir_create_ms = total_start.elapsed().as_millis() as u64;

        let mut catalog = HashMap::new();
        populate_catalog(&mut catalog);
        let catalog_populate_ms = total_start.elapsed().as_millis() as u64 - dir_create_ms;

        // Custom user-supplied .bin files in the models directory show
        // up as "Custom" entries in the UI. Mirrors Handy's behavior.
        let custom_discover_ms =
            if let Err(e) = discover_custom_whisper_models(&models_dir, &mut catalog) {
                tracing::warn!(error = %e, "model_registry_custom_discovery_failed");
                0
            } else {
                total_start.elapsed().as_millis() as u64 - dir_create_ms - catalog_populate_ms
            };

        // 16 listeners is plenty — UDS clients, the dispatch hot-swap
        // task, future UI watchers. Slow consumers receive `Lagged`
        // and re-sync via `list_models`.
        let (events_tx, _events_rx) = broadcast::channel(64);

        let registry = Self {
            models_dir,
            available_models: Arc::new(Mutex::new(catalog)),
            cancel_flags: Arc::new(Mutex::new(HashMap::new())),
            extracting_models: Arc::new(Mutex::new(HashSet::new())),
            events_tx,
            verify_cache: Arc::new(Mutex::new(HashMap::new())),
        };

        // Startup used to block here SHA256-hashing every multi-GB model
        // (several seconds per gigabyte, on EVERY daemon start because
        // the verdict cache was memory-only). Now: load the persisted
        // verdicts, do a hash-free provisional pass so the catalog is
        // immediately usable, and finish real verification on a
        // background thread. Activation still hashes via `model_path`,
        // so nothing unverified can ever be loaded.
        registry.load_verify_cache();
        let cache_load_ms = total_start.elapsed().as_millis() as u64
            - dir_create_ms
            - catalog_populate_ms
            - custom_discover_ms;

        registry.refresh_disk_status_provisional();
        let provisional_ms = total_start.elapsed().as_millis() as u64
            - dir_create_ms
            - catalog_populate_ms
            - custom_discover_ms
            - cache_load_ms;

        let background = registry.clone();
        std::thread::Builder::new()
            .name("model-verify".into())
            .spawn(move || {
                let started = Instant::now();
                if let Err(e) = background.refresh_disk_status() {
                    tracing::warn!(error = %e, "model_background_verify_failed");
                }
                tracing::info!(
                    verify_ms = started.elapsed().as_millis() as u64,
                    "model_background_verify_done"
                );
                let _ = background.events_tx.send(ModelEvent::DiskStatusRefreshed);
            })
            .context("spawn model-verify thread")?;

        let total_ms = total_start.elapsed().as_millis() as u64;
        tracing::info!(
            total_ms,
            dir_create_ms,
            catalog_populate_ms,
            custom_discover_ms,
            cache_load_ms,
            provisional_ms,
            "model_registry_new_complete"
        );

        Ok(registry)
    }

    /// Subscribe to download / extraction / active-change events.
    pub fn subscribe(&self) -> broadcast::Receiver<ModelEvent> {
        self.events_tx.subscribe()
    }

    /// All catalog entries with `is_downloaded` / `is_downloading` /
    /// `partial_size` fields reflecting current disk state.
    pub fn list(&self) -> Vec<ModelInfo> {
        self.available_models.lock().values().cloned().collect()
    }

    /// Single lookup. Returns `None` for unknown ids.
    pub fn get(&self, model_id: &str) -> Option<ModelInfo> {
        self.available_models.lock().get(model_id).cloned()
    }

    /// On-disk path of a (possibly not-yet-downloaded) model. Prefers
    /// always's own models directory; falls back to Handy's cache if the
    /// same filename exists there. Returning `None` from
    /// `models_dir.join(filename)` is impossible — the caller wants to
    /// know "is there a usable file" rather than "what would the path
    /// be", so we encode that distinction here.
    ///
    /// Integrity is enforced before a pre-existing file is offered as
    /// usable, mirroring `refresh_disk_status`: single-file `.bin`
    /// catalog models must re-hash to the catalog SHA256 (cached by
    /// size+mtime); directory models are trusted only when we extracted
    /// them ourselves (a `<filename>.verified` marker is present in our
    /// own dir) — Handy's extracted directories are outside our trust
    /// boundary and are never returned for directory models.
    pub fn model_path(&self, model_id: &str) -> Option<PathBuf> {
        let m = self.get(model_id)?;
        let own = self.models_dir.join(&m.filename);

        if m.is_directory {
            // Trust only our own, marker-verified extraction.
            if path_matches_kind(&own, true) && self.dir_verified(&m.filename) {
                return Some(own);
            }
        } else {
            // Prefer our own copy, then Handy's, but only if the bytes
            // verify against the catalog SHA (or there's no catalog SHA,
            // i.e. a user-supplied custom `.bin`).
            if path_matches_kind(&own, false) && self.bin_verified(&own, m.sha256.as_deref()) {
                return Some(own);
            }
            if let Some(handy) = handy_models_dir() {
                let candidate = handy.join(&m.filename);
                if path_matches_kind(&candidate, false)
                    && self.bin_verified(&candidate, m.sha256.as_deref())
                {
                    return Some(candidate);
                }
            }
        }

        // Nothing usable yet — return the canonical "would-be" path so
        // callers that just want to compute the install location keep
        // working. Callers that need "is this usable" should consult
        // `ModelInfo::is_downloaded`.
        Some(own)
    }

    /// Root directory holding all downloaded model files (always's own
    /// downloads, not Handy's cache).
    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    /// Path of the `<filename>.verified` marker we drop in our own
    /// models dir after a successful download+verify+extract of a
    /// directory model. Its presence is the *only* evidence we accept
    /// that an extracted directory is trustworthy.
    fn verified_marker(&self, filename: &str) -> PathBuf {
        self.models_dir.join(format!("{filename}.verified"))
    }

    /// True when a directory model in OUR models dir carries the
    /// `.verified` marker (i.e. always itself extracted and SHA-verified
    /// the source archive). Never consulted for Handy's cache — those
    /// directories are outside our trust boundary.
    fn dir_verified(&self, filename: &str) -> bool {
        self.verified_marker(filename).is_file()
    }

    /// Sidecar file persisting SHA verdicts across daemon restarts.
    /// Without it the memory-only cache forced a full re-hash of every
    /// downloaded model on every startup. Keys include size+mtime, so a
    /// replaced/corrupted file never inherits a stale verdict.
    fn verify_cache_path(&self) -> PathBuf {
        self.models_dir.join(".verify-cache.json")
    }

    /// Best-effort load of persisted verdicts. A corrupt or missing
    /// cache file is silently discarded — worst case we re-hash.
    fn load_verify_cache(&self) {
        let Ok(raw) = fs::read_to_string(self.verify_cache_path()) else {
            return;
        };
        let Ok(entries) = serde_json::from_str::<Vec<PersistedVerdict>>(&raw) else {
            tracing::warn!("model_verify_cache_corrupt_discarding");
            return;
        };
        let mut cache = self.verify_cache.lock();
        for e in entries {
            // Prefer the nanosecond field; fall back to the legacy seconds
            // field so an old cache file still loads (its entries simply
            // will not match, exactly as before, and get rewritten).
            let mtime = if e.mtime_unix_nanos > 0 {
                SystemTime::UNIX_EPOCH + Duration::from_nanos(e.mtime_unix_nanos as u64)
            } else {
                SystemTime::UNIX_EPOCH + Duration::from_secs(e.mtime_unix_secs)
            };
            cache.insert((e.path, e.len, mtime), e.verdict);
        }
        tracing::info!(entries = cache.len(), "model_verify_cache_loaded");
    }

    /// Best-effort atomic persist (tmp + rename). Called after every
    /// fresh hash so a crash never costs more than one verdict.
    fn save_verify_cache(&self) {
        let entries: Vec<PersistedVerdict> = self
            .verify_cache
            .lock()
            .iter()
            .map(|((path, len, mtime), &verdict)| PersistedVerdict {
                path: path.clone(),
                len: *len,
                mtime_unix_secs: 0,
                mtime_unix_nanos: mtime
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
                verdict,
            })
            .collect();
        let Ok(json) = serde_json::to_string(&entries) else {
            return;
        };
        let target = self.verify_cache_path();
        let tmp = target.with_extension("json.tmp");
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::rename(&tmp, &target);
        }
    }

    /// Cache-only verdict lookup — never hashes. `None` means "unknown,
    /// a real hash is required".
    fn bin_cached_verdict(&self, path: &Path, expected: Option<&str>) -> Option<bool> {
        if expected.is_none() {
            // Custom model without a catalog hash: existence is trust.
            return Some(path.is_file());
        }
        let meta = path.metadata().ok()?;
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let key = (path.to_path_buf(), meta.len(), mtime);
        let hit = self.verify_cache.lock().get(&key).copied();
        if hit.is_some() {
            tracing::debug!(path = %path.display(), "model_verify_cache_hit");
        }
        hit
    }

    /// True when a single-file `.bin` at `path` is safe to treat as
    /// downloaded. When `expected` is `Some`, the on-disk bytes must
    /// hash to the catalog SHA256; the verdict is memoized by
    /// `(path, len, mtime)` so a multi-GB checkpoint isn't re-hashed on
    /// every refresh. When `expected` is `None` (user-supplied custom
    /// `.bin`), we keep the legacy existence-only trust.
    fn bin_verified(&self, path: &Path, expected: Option<&str>) -> bool {
        let Some(expected) = expected else {
            // No catalog hash to check against (custom model). Preserve
            // the historical behavior of trusting its presence.
            return true;
        };
        // Key the cache on the identity the filesystem can cheaply
        // report. If we can't stat the file, treat it as unusable.
        let Ok(meta) = path.metadata() else {
            return false;
        };
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let key = (path.to_path_buf(), meta.len(), mtime);

        if let Some(&verdict) = self.verify_cache.lock().get(&key) {
            return verdict;
        }

        let verdict = match compute_sha256(path) {
            Ok(actual) => actual == expected,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "model_bin_hash_failed");
                false
            }
        };
        if !verdict {
            tracing::warn!(
                path = %path.display(),
                "model_bin_sha256_mismatch_refusing_to_trust"
            );
        }
        self.verify_cache.lock().insert(key, verdict);
        // Persist immediately: a fresh hash of a multi-GB file is the
        // expensive thing this cache exists to avoid repeating.
        self.save_verify_cache();
        verdict
    }

    /// Re-scan the models directory and update `is_downloaded` flags +
    /// partial sizes. Cheap — called once at startup and again after
    /// every download / delete. Also consults Handy's cache directory:
    /// if `~/Library/Application Support/com.pais.handy/models/<filename>`
    /// exists with the right kind, the model is treated as downloaded
    /// even when we haven't fetched it ourselves.
    pub fn refresh_disk_status(&self) -> Result<()> {
        self.refresh_disk_status_inner(true)
    }

    /// Hash-free variant used at startup: existence + cached verdicts
    /// only, so the catalog is listable in milliseconds. Models whose
    /// bytes still need a real hash get `is_verifying = true` and a
    /// provisional existence-based `is_downloaded`; the background
    /// `refresh_disk_status` pass settles them and broadcasts
    /// [`ModelEvent::DiskStatusRefreshed`].
    fn refresh_disk_status_provisional(&self) {
        let _ = self.refresh_disk_status_inner(false);
    }

    fn refresh_disk_status_inner(&self, hash_missing: bool) -> Result<()> {
        let handy = handy_models_dir();
        let mut models = self.available_models.lock();
        for model in models.values_mut() {
            let model_path = self.models_dir.join(&model.filename);
            let partial_path = self.models_dir.join(format!("{}.partial", model.filename));
            let extracting_path = self
                .models_dir
                .join(format!("{}.extracting", model.filename));

            // Sweep stale extraction temp dirs left over from a crash
            // mid-extract — but only if we're not actively extracting
            // this model right now.
            let is_currently_extracting = self.extracting_models.lock().contains(&model.id);
            if extracting_path.exists() && !is_currently_extracting {
                tracing::warn!(model = %model.id, "cleaning_up_interrupted_extraction");
                let _ = fs::remove_dir_all(&extracting_path);
            }

            // Integrity gate: a file only counts as "downloaded" once we
            // can vouch for it. We never blindly trust existence alone —
            // see the per-kind rules below.
            let mut needs_hash = false;
            let downloaded = if model.is_directory {
                // tar.gz models extract to a directory whose contents the
                // catalog SHA (over the *archive*) can't re-verify. Trust
                // only a directory WE extracted+verified, marked by a
                // sibling `<filename>.verified` file in our own dir. Handy's
                // extracted dirs live outside our trust boundary and are
                // deliberately ignored here (the user re-downloads once).
                // Marker check is cheap — identical in both passes.
                path_matches_kind(&model_path, true) && self.dir_verified(&model.filename)
            } else {
                // Single-file `.bin`: verify against the catalog SHA
                // (cached by size+mtime). Own dir wins, then Handy's. A
                // custom `.bin` (sha256 == None) keeps the legacy
                // existence-only behavior. In the provisional pass an
                // unknown verdict reports existence and flags the model
                // as still-verifying instead of hashing inline.
                let mut bin_status = |path: &Path| -> bool {
                    if !path_matches_kind(path, false) {
                        return false;
                    }
                    match self.bin_cached_verdict(path, model.sha256.as_deref()) {
                        Some(verdict) => verdict,
                        None if hash_missing => self.bin_verified(path, model.sha256.as_deref()),
                        None => {
                            needs_hash = true;
                            true // provisional: file exists, hash pending
                        }
                    }
                };
                let own_ok = bin_status(&model_path);
                let handy_ok = !own_ok
                    && handy
                        .as_ref()
                        .map(|h| bin_status(&h.join(&model.filename)))
                        .unwrap_or(false);
                own_ok || handy_ok
            };
            model.is_downloaded = downloaded;
            model.is_verifying = needs_hash && !hash_missing;
            model.is_downloading = false;
            model.partial_size = partial_path.metadata().map(|m| m.len()).unwrap_or(0);
        }
        Ok(())
    }

    /// Request cancellation of an in-flight download. Idempotent — a
    /// cancel after the download has already finished is a no-op.
    pub fn cancel_download(&self, model_id: &str) {
        if let Some(flag) = self.cancel_flags.lock().get(model_id) {
            flag.store(true, Ordering::Relaxed);
            tracing::info!(model = %model_id, "model_download_cancel_requested");
        }
    }

    /// Remove a downloaded model (file or directory) from disk and
    /// update flags. Errors if the model id is unknown.
    pub fn delete(&self, model_id: &str) -> Result<()> {
        let info = self
            .get(model_id)
            .ok_or_else(|| anyhow::anyhow!("unknown model id: {model_id}"))?;
        let path = self.models_dir.join(&info.filename);
        let partial = self.models_dir.join(format!("{}.partial", info.filename));

        if path.is_dir() {
            fs::remove_dir_all(&path).ok();
        } else if path.is_file() {
            fs::remove_file(&path).ok();
        }
        if partial.exists() {
            fs::remove_file(&partial).ok();
        }
        // Drop the directory-model trust marker so a re-download is
        // forced to re-verify rather than inheriting a stale "verified"
        // verdict from the deleted copy.
        let marker = self.verified_marker(&info.filename);
        if marker.exists() {
            fs::remove_file(&marker).ok();
        }
        self.refresh_disk_status()?;
        let _ = self
            .events_tx
            .send(ModelEvent::ActiveChanged { model_id: None });
        Ok(())
    }

    /// Stream a model from its catalog URL to disk, resuming if a
    /// `.partial` file already exists. Verifies SHA256 (when known)
    /// and atomically extracts tar.gz archives into the final
    /// directory. Reports progress over the broadcast channel.
    ///
    /// Returns `Ok(())` even on user cancellation (the partial file is
    /// kept so a future call resumes); only network / IO / verify
    /// failures surface as `Err`.
    /// Download a directory model published as loose files.
    ///
    /// Mirrors the single-archive path's contract exactly — same
    /// progress/verification/extraction events, same `.verified` marker,
    /// same cancel semantics — so the GUI cannot tell the two apart. The
    /// differences are forced by the shape of the source: progress is
    /// aggregated across files from the catalog's declared sizes (there
    /// is no single content-length to report), each file verifies with
    /// its own SHA256 (there is no archive checksum), and assembly is a
    /// directory rename rather than an unpack.
    ///
    /// Files land in a `.downloading` directory that is only promoted to
    /// the real name once every part has arrived and verified, so an
    /// interrupted download can never leave a half-populated model dir
    /// that looks installed to the loader.
    async fn download_files(&self, model_id: &str, model_info: &ModelInfo) -> Result<()> {
        let final_dir = self.models_dir.join(&model_info.filename);
        if path_matches_kind(&final_dir, true) && self.dir_verified(&model_info.filename) {
            self.refresh_disk_status()?;
            return Ok(());
        }

        let total_bytes: u64 = model_info.files.iter().map(|f| f.size_bytes).sum();
        tracing::info!(
            model = %model_id,
            files = model_info.files.len(),
            total_bytes,
            "model_multifile_download_start"
        );

        if let Some(m) = self.available_models.lock().get_mut(model_id) {
            m.is_downloading = true;
        }
        let cancel_flag = Arc::new(AtomicBool::new(false));
        self.cancel_flags
            .lock()
            .insert(model_id.to_string(), cancel_flag.clone());
        let mut cleanup = DownloadCleanup {
            available_models: &self.available_models,
            cancel_flags: &self.cancel_flags,
            model_id: model_id.to_string(),
            disarmed: false,
        };

        let staging_dir = self
            .models_dir
            .join(format!("{}.downloading", model_info.filename));
        // Keep an existing staging directory rather than deleting it: each
        // file below is skipped when it is already present and hashes
        // correctly, so an interrupted install resumes instead of
        // re-fetching gigabytes. Anything corrupt or half-written fails
        // its checksum and is re-downloaded.
        fs::create_dir_all(&staging_dir)?;

        let client = reqwest::Client::new();
        let mut completed: u64 = 0;
        self.emit_progress(model_id, 0, total_bytes);
        let mut last_emit = Instant::now();
        let throttle = Duration::from_millis(100);

        for spec in &model_info.files {
            // Reject path separators outright: a catalog entry must not be
            // able to write outside the model directory.
            if spec.name.contains('/') || spec.name.contains('\\') || spec.name.contains("..") {
                let _ = fs::remove_dir_all(&staging_dir);
                let msg = format!("illegal file name in catalog entry: {}", spec.name);
                self.emit_failed(model_id, &msg);
                return Err(anyhow::anyhow!(msg));
            }
            let dest = staging_dir.join(&spec.name);

            // Already fetched by an earlier attempt? Verify rather than
            // re-download — these files run to 2.4 GB.
            if dest.metadata().map(|m| m.len()).ok() == Some(spec.size_bytes) {
                let verify_path = dest.clone();
                let expected = spec.sha256.clone();
                let verify_id = format!("{model_id}:{}", spec.name);
                let already_good = tokio::task::spawn_blocking(move || {
                    verify_sha256(&verify_path, Some(&expected), &verify_id).is_ok()
                })
                .await
                .unwrap_or(false);
                if already_good {
                    tracing::info!(
                        model = %model_id,
                        file = %spec.name,
                        "model_file_already_present_skipping"
                    );
                    completed += spec.size_bytes;
                    self.emit_progress(model_id, completed, total_bytes);
                    continue;
                }
            }

            let mut response = match client.get(&spec.url).send().await {
                Ok(r) => r,
                Err(e) => {
                    let _ = fs::remove_dir_all(&staging_dir);
                    self.emit_failed(model_id, &format!("network error: {e}"));
                    return Err(e.into());
                }
            };
            if !response.status().is_success() {
                let _ = fs::remove_dir_all(&staging_dir);
                let msg = format!(
                    "Failed to download {}: HTTP {}",
                    spec.name,
                    response.status()
                );
                self.emit_failed(model_id, &msg);
                return Err(anyhow::anyhow!(msg));
            }

            let mut file = std::fs::File::create(&dest)?;
            let mut this_file: u64 = 0;
            while let Some(chunk) = response.chunk().await.transpose() {
                if cancel_flag.load(Ordering::Relaxed) {
                    drop(file);
                    let _ = fs::remove_dir_all(&staging_dir);
                    tracing::info!(model = %model_id, "model_download_cancelled");
                    let _ = self.events_tx.send(ModelEvent::DownloadCancelled {
                        model_id: model_id.to_string(),
                    });
                    return Ok(());
                }
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        drop(file);
                        let _ = fs::remove_dir_all(&staging_dir);
                        self.emit_failed(model_id, &format!("stream error: {e}"));
                        return Err(e.into());
                    }
                };
                file.write_all(&chunk)?;
                this_file += chunk.len() as u64;
                if last_emit.elapsed() >= throttle {
                    self.emit_progress(model_id, completed + this_file, total_bytes);
                    last_emit = Instant::now();
                }
            }
            file.flush()?;
            drop(file);
            completed += this_file;
            self.emit_progress(model_id, completed, total_bytes);

            // Verify this part before moving on, so a corrupt 2 GB file is
            // caught here rather than as a confusing load failure later.
            let verify_path = dest.clone();
            let expected = spec.sha256.clone();
            let verify_id = format!("{model_id}:{}", spec.name);
            let verified = tokio::task::spawn_blocking(move || {
                verify_sha256(&verify_path, Some(&expected), &verify_id)
            })
            .await
            .map_err(|e| anyhow::anyhow!("SHA256 task panicked: {e}"))?;
            if let Err(e) = verified {
                let _ = fs::remove_dir_all(&staging_dir);
                self.emit_failed(model_id, &format!("sha256 mismatch for {}: {e}", spec.name));
                return Err(e);
            }
        }

        let _ = self.events_tx.send(ModelEvent::VerificationCompleted {
            model_id: model_id.to_string(),
        });

        // Clear whatever occupies the target path. This must NOT assume a
        // directory: this exact model previously shipped as a single
        // `.nemo` archive, so a user who tried the old entry has a 2.2 GB
        // FILE sitting at the directory's path. `remove_dir_all` on a file
        // fails with ENOTDIR, which aborted the install after all four
        // files had downloaded and verified — the whole 2.5 GB thrown away
        // at the final step, with the UI left spinning because the error
        // propagated instead of being reported.
        if let Err(e) = remove_path_any_kind(&final_dir) {
            let msg = format!("could not clear {}: {e}", final_dir.display());
            self.emit_failed(model_id, &msg);
            return Err(anyhow::anyhow!(msg));
        }
        if let Err(e) = fs::rename(&staging_dir, &final_dir) {
            let msg = format!("could not install into {}: {e}", final_dir.display());
            self.emit_failed(model_id, &msg);
            return Err(anyhow::anyhow!(msg));
        }

        // Same trust marker the archive path drops: a directory model is
        // only considered usable when we ourselves downloaded and
        // verified it.
        let marker = self.verified_marker(&model_info.filename);
        if let Err(e) = fs::write(&marker, b"") {
            tracing::warn!(
                model = %model_id,
                error = %e,
                "model_verified_marker_write_failed"
            );
        }

        tracing::info!(
            model = %model_id,
            path = %final_dir.display(),
            "model_download_done"
        );

        // Success path runs its own cleanup so the guard's Drop doesn't
        // clobber `is_downloaded = true`.
        cleanup.disarmed = true;
        if let Some(m) = self.available_models.lock().get_mut(model_id) {
            m.is_downloading = false;
            m.is_downloaded = true;
            m.partial_size = 0;
        }
        self.cancel_flags.lock().remove(model_id);

        let _ = self.events_tx.send(ModelEvent::DownloadComplete {
            model_id: model_id.to_string(),
        });
        self.refresh_disk_status()?;
        Ok(())
    }

    pub async fn download(&self, model_id: &str) -> Result<()> {
        let model_info = self
            .get(model_id)
            .ok_or_else(|| anyhow::anyhow!("Model not found: {model_id}"))?;
        if !model_info.files.is_empty() {
            return self.download_files(model_id, &model_info).await;
        }
        let url = model_info
            .url
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No download URL for model {model_id}"))?;

        let model_path = self.models_dir.join(&model_info.filename);
        let partial_path = self
            .models_dir
            .join(format!("{}.partial", model_info.filename));

        // Already present and trustworthy — nothing to do; clean up any
        // leftover .partial. For directory models "present" alone is not
        // enough: without our `.verified` marker the on-disk dir can't be
        // trusted (it may be a stale/unmarked or Handy-cached extraction),
        // so we fall through and perform a real verified download.
        let already_usable = if model_info.is_directory {
            path_matches_kind(&model_path, true) && self.dir_verified(&model_info.filename)
        } else {
            model_path.exists()
        };
        if already_usable {
            if partial_path.exists() {
                let _ = fs::remove_file(&partial_path);
            }
            self.refresh_disk_status()?;
            return Ok(());
        }

        let mut resume_from = if partial_path.exists() {
            let size = partial_path.metadata()?.len();
            tracing::info!(model = %model_id, resume_from = size, "model_download_resume");
            size
        } else {
            tracing::info!(model = %model_id, %url, "model_download_start");
            0
        };

        if let Some(m) = self.available_models.lock().get_mut(model_id) {
            m.is_downloading = true;
        }

        let cancel_flag = Arc::new(AtomicBool::new(false));
        self.cancel_flags
            .lock()
            .insert(model_id.to_string(), cancel_flag.clone());

        let mut cleanup = DownloadCleanup {
            available_models: &self.available_models,
            cancel_flags: &self.cancel_flags,
            model_id: model_id.to_string(),
            disarmed: false,
        };

        let client = reqwest::Client::new();
        let mut request = client.get(&url);
        if resume_from > 0 {
            request = request.header("Range", format!("bytes={resume_from}-"));
        }

        let mut response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                self.emit_failed(model_id, &format!("network error: {e}"));
                return Err(e.into());
            }
        };

        // Server doesn't support range requests — restart fresh.
        if resume_from > 0 && response.status() == reqwest::StatusCode::OK {
            tracing::warn!(
                model = %model_id,
                "server_doesnt_support_range_restarting"
            );
            drop(response);
            let _ = fs::remove_file(&partial_path);
            resume_from = 0;
            response = client.get(&url).send().await?;
        }

        if !response.status().is_success()
            && response.status() != reqwest::StatusCode::PARTIAL_CONTENT
        {
            let msg = format!("Failed to download model: HTTP {}", response.status());
            self.emit_failed(model_id, &msg);
            return Err(anyhow::anyhow!(msg));
        }

        let total_size = if resume_from > 0 {
            resume_from + response.content_length().unwrap_or(0)
        } else {
            response.content_length().unwrap_or(0)
        };

        let mut downloaded = resume_from;
        let mut stream = response.bytes_stream();

        let mut file = if resume_from > 0 {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&partial_path)?
        } else {
            std::fs::File::create(&partial_path)?
        };

        self.emit_progress(model_id, downloaded, total_size);
        let mut last_emit = Instant::now();
        let throttle = Duration::from_millis(100);

        while let Some(chunk) = stream.next().await {
            if cancel_flag.load(Ordering::Relaxed) {
                drop(file);
                tracing::info!(model = %model_id, "model_download_cancelled");
                let _ = self.events_tx.send(ModelEvent::DownloadCancelled {
                    model_id: model_id.to_string(),
                });
                return Ok(());
            }

            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    self.emit_failed(model_id, &format!("stream error: {e}"));
                    return Err(e.into());
                }
            };
            file.write_all(&chunk)?;
            downloaded += chunk.len() as u64;

            if last_emit.elapsed() >= throttle {
                self.emit_progress(model_id, downloaded, total_size);
                last_emit = Instant::now();
            }
        }

        // Emit final 100% progress so the UI doesn't pause at 99%.
        self.emit_progress(model_id, downloaded, total_size);
        file.flush()?;
        drop(file);

        if total_size > 0 {
            let actual_size = partial_path.metadata()?.len();
            if actual_size != total_size {
                let _ = fs::remove_file(&partial_path);
                let msg = format!(
                    "Download incomplete: expected {total_size} bytes, got {actual_size} bytes"
                );
                self.emit_failed(model_id, &msg);
                return Err(anyhow::anyhow!(msg));
            }
        }

        // SHA256 verification in a blocking thread so the executor
        // doesn't stall while hashing 1.6 GB models.
        let _ = self.events_tx.send(ModelEvent::VerificationStarted {
            model_id: model_id.to_string(),
        });
        tracing::info!(model = %model_id, "model_sha256_verify_start");
        let verify_path = partial_path.clone();
        let verify_expected = model_info.sha256.clone();
        let verify_id = model_id.to_string();
        let verify_result = tokio::task::spawn_blocking(move || {
            verify_sha256(&verify_path, verify_expected.as_deref(), &verify_id)
        })
        .await
        .map_err(|e| anyhow::anyhow!("SHA256 task panicked: {e}"))?;
        if let Err(e) = verify_result {
            self.emit_failed(model_id, &format!("sha256 mismatch: {e}"));
            return Err(e);
        }
        let _ = self.events_tx.send(ModelEvent::VerificationCompleted {
            model_id: model_id.to_string(),
        });

        // tar.gz extract (directory-based) vs single-file rename.
        if model_info.is_directory {
            self.extracting_models.lock().insert(model_id.to_string());
            let _ = self.events_tx.send(ModelEvent::ExtractionStarted {
                model_id: model_id.to_string(),
            });

            let temp_extract_dir = self
                .models_dir
                .join(format!("{}.extracting", model_info.filename));
            let final_model_dir = self.models_dir.join(&model_info.filename);

            if temp_extract_dir.exists() {
                let _ = fs::remove_dir_all(&temp_extract_dir);
            }
            fs::create_dir_all(&temp_extract_dir)?;

            let tar_gz = File::open(&partial_path)?;
            let tar = GzDecoder::new(tar_gz);
            let mut archive = Archive::new(tar);
            if let Err(e) = archive.unpack(&temp_extract_dir) {
                let _ = fs::remove_dir_all(&temp_extract_dir);
                let _ = fs::remove_file(&partial_path);
                self.extracting_models.lock().remove(model_id);
                let msg = format!("Failed to extract archive: {e}");
                let _ = self.events_tx.send(ModelEvent::ExtractionFailed {
                    model_id: model_id.to_string(),
                    error: msg.clone(),
                });
                return Err(anyhow::anyhow!(msg));
            }

            // Tarballs may or may not wrap their content in a single
            // top-level directory — handle both shapes.
            let extracted_dirs: Vec<_> = fs::read_dir(&temp_extract_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
                .collect();
            if extracted_dirs.len() == 1 {
                let source_dir = extracted_dirs[0].path();
                if final_model_dir.exists() {
                    fs::remove_dir_all(&final_model_dir)?;
                }
                fs::rename(&source_dir, &final_model_dir)?;
                let _ = fs::remove_dir_all(&temp_extract_dir);
            } else {
                if final_model_dir.exists() {
                    fs::remove_dir_all(&final_model_dir)?;
                }
                fs::rename(&temp_extract_dir, &final_model_dir)?;
            }

            self.extracting_models.lock().remove(model_id);

            // Drop the trust marker: we just downloaded the archive,
            // SHA-verified it against the catalog, and extracted it into
            // our own dir. `refresh_disk_status` / `model_path` treat a
            // directory model as usable only when this marker is present,
            // so an unmarked (e.g. Handy-cached) directory is never
            // loaded without a fresh verified download.
            let marker = self.verified_marker(&model_info.filename);
            if let Err(e) = fs::write(&marker, b"") {
                tracing::warn!(
                    model = %model_id,
                    error = %e,
                    "model_verified_marker_write_failed"
                );
            }

            let _ = self.events_tx.send(ModelEvent::ExtractionCompleted {
                model_id: model_id.to_string(),
            });
            let _ = fs::remove_file(&partial_path);
        } else {
            fs::rename(&partial_path, &model_path)?;
        }

        // Success path runs its own cleanup so the guard's Drop doesn't
        // clobber `is_downloaded = true`.
        cleanup.disarmed = true;
        if let Some(m) = self.available_models.lock().get_mut(model_id) {
            m.is_downloading = false;
            m.is_downloaded = true;
            m.partial_size = 0;
        }
        self.cancel_flags.lock().remove(model_id);

        let _ = self.events_tx.send(ModelEvent::DownloadComplete {
            model_id: model_id.to_string(),
        });
        tracing::info!(model = %model_id, path = %model_path.display(), "model_download_done");
        Ok(())
    }

    fn emit_progress(&self, model_id: &str, downloaded: u64, total: u64) {
        let percentage = if total > 0 {
            (downloaded as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        let _ = self
            .events_tx
            .send(ModelEvent::DownloadProgress(DownloadProgress {
                model_id: model_id.to_string(),
                downloaded,
                total,
                percentage,
            }));
    }

    fn emit_failed(&self, model_id: &str, error: &str) {
        let _ = self.events_tx.send(ModelEvent::DownloadFailed {
            model_id: model_id.to_string(),
            error: error.to_string(),
        });
    }
}

/// `~/Library/Application Support/always/models` on macOS.
fn default_models_dir() -> Result<PathBuf> {
    let base = dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("no data dir"))?
        .join("always")
        .join("models");
    Ok(base)
}

/// Handy's models directory, when present. The user may have already
/// downloaded our overlapping model set via Handy
/// (`github.com/cjpais/handy`) — both apps pull from the same
/// `blob.handy.computer` CDN with identical filenames, so we can reuse
/// the file directly instead of forcing a second download.
///
/// Returns `None` outside macOS or when the directory doesn't exist —
/// we never auto-create it (it belongs to Handy, not us).
fn handy_models_dir() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let dir = dirs::data_dir()?.join("com.pais.handy").join("models");
    if dir.is_dir() { Some(dir) } else { None }
}

/// True when `path` exists and matches the expected kind (directory for
/// tar.gz-extracted engines like Parakeet/Canary, regular file for
/// `.bin` Whisper checkpoints). Mirrors the check that both
/// `refresh_disk_status` and `model_path` previously inlined.
fn path_matches_kind(path: &Path, expect_dir: bool) -> bool {
    if !path.exists() {
        return false;
    }
    if expect_dir {
        path.is_dir()
    } else {
        path.is_file()
    }
}

fn verify_sha256(path: &Path, expected: Option<&str>, model_id: &str) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    match compute_sha256(path) {
        Ok(actual) if actual == expected => {
            tracing::info!(model = %model_id, "model_sha256_verified");
            Ok(())
        }
        Ok(actual) => {
            tracing::warn!(
                model = %model_id,
                expected = %expected,
                actual = %actual,
                "model_sha256_mismatch"
            );
            let _ = fs::remove_file(path);
            Err(anyhow::anyhow!(
                "Download verification failed for model {model_id}: file is corrupt. Please retry."
            ))
        }
        Err(e) => {
            let _ = fs::remove_file(path);
            Err(anyhow::anyhow!(
                "Failed to verify download for model {model_id}: {e}. Please retry."
            ))
        }
    }
}

fn compute_sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn discover_custom_whisper_models(
    models_dir: &Path,
    available_models: &mut HashMap<String, ModelInfo>,
) -> Result<()> {
    if !models_dir.exists() {
        return Ok(());
    }

    let predefined_filenames: HashSet<String> = available_models
        .values()
        .filter(|m| matches!(m.engine_type, EngineType::Whisper) && !m.is_directory)
        .map(|m| m.filename.clone())
        .collect();

    for entry in fs::read_dir(models_dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "read_dir_failed");
                continue;
            }
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let filename = match path.file_name().and_then(|s| s.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };
        if filename.starts_with('.') {
            continue;
        }
        if !filename.ends_with(".bin") {
            continue;
        }
        if predefined_filenames.contains(&filename) {
            continue;
        }
        let model_id = filename.trim_end_matches(".bin").to_string();
        if available_models.contains_key(&model_id) {
            continue;
        }

        let display_name = model_id
            .replace(['-', '_'], " ")
            .split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        let size_mb = path
            .metadata()
            .map(|m| m.len() / (1024 * 1024))
            .unwrap_or(0);
        tracing::info!(model = %model_id, %filename, size_mb, "discovered_custom_whisper_model");

        available_models.insert(
            model_id.clone(),
            ModelInfo {
                id: model_id,
                name: display_name,
                description: "Not officially supported".to_string(),
                filename,
                url: None,
                sha256: None,
                size_mb,
                is_downloaded: true,
                is_downloading: false,
                partial_size: 0,
                is_directory: false,
                engine_type: EngineType::Whisper,
                accuracy_score: 0.0,
                speed_score: 0.0,
                supports_translation: false,
                supports_streaming: false,
                is_recommended: false,
                supported_languages: vec![],
                supports_language_selection: true,
                is_custom: true,
                files: Vec::new(),
                is_verifying: false,
            },
        );
    }
    Ok(())
}

/// Hardcoded catalog. URLs + SHA256s lifted verbatim from Handy 0.8.3.
/// We trust their hashes because they're content-addressed — even when
/// we move to our own bucket, the same SHA256 keeps the download safe.
fn populate_catalog(map: &mut HashMap<String, ModelInfo>) {
    let whisper_languages: Vec<String> = [
        "en", "zh", "zh-Hans", "zh-Hant", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl",
        "ca", "nl", "ar", "sv", "it", "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro",
        "da", "hu", "ta", "no", "th", "ur", "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te",
        "fa", "lv", "bn", "sr", "az", "sl", "kn", "et", "mk", "br", "eu", "is", "hy", "ne", "mn",
        "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si", "km", "sn", "yo", "so", "af", "oc", "ka",
        "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo", "ht", "ps", "tk", "nn", "mt", "sa",
        "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln", "ha", "ba", "jw", "su", "yue",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    map.insert(
        "small".into(),
        ModelInfo {
            id: "small".into(),
            name: "Whisper Small".into(),
            description: "Fast and fairly accurate.".into(),
            filename: "ggml-small.bin".into(),
            url: Some("https://blob.handy.computer/ggml-small.bin".into()),
            sha256: Some("1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b".into()),
            size_mb: 465,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: false,
            engine_type: EngineType::Whisper,
            accuracy_score: 0.60,
            speed_score: 0.85,
            supports_translation: true,
            supports_streaming: false,
            is_recommended: false,
            supported_languages: whisper_languages.clone(),
            supports_language_selection: true,
            is_custom: false,
            files: Vec::new(),
            is_verifying: false,
        },
    );

    map.insert(
        "medium".into(),
        ModelInfo {
            id: "medium".into(),
            name: "Whisper Medium".into(),
            description: "Good accuracy, medium speed".into(),
            filename: "whisper-medium-q4_1.bin".into(),
            url: Some("https://blob.handy.computer/whisper-medium-q4_1.bin".into()),
            sha256: Some("79283fc1f9fe12ca3248543fbd54b73292164d8df5a16e095e2bceeaaabddf57".into()),
            size_mb: 469,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: false,
            engine_type: EngineType::Whisper,
            accuracy_score: 0.75,
            speed_score: 0.60,
            supports_translation: true,
            supports_streaming: false,
            is_recommended: false,
            supported_languages: whisper_languages.clone(),
            supports_language_selection: true,
            is_custom: false,
            files: Vec::new(),
            is_verifying: false,
        },
    );

    map.insert(
        "turbo".into(),
        ModelInfo {
            id: "turbo".into(),
            name: "Whisper Turbo".into(),
            description: "Distilled large-v3: near-large accuracy at faster speed. Multilingual, supports translation.".into(),
            filename: "ggml-large-v3-turbo.bin".into(),
            url: Some("https://blob.handy.computer/ggml-large-v3-turbo.bin".into()),
            sha256: Some("1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69".into()),
            size_mb: 1549,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: false,
            engine_type: EngineType::Whisper,
            accuracy_score: 0.80,
            speed_score: 0.40,
            supports_translation: true,
            supports_streaming: false,
            is_recommended: false,
            supported_languages: whisper_languages.clone(),
            supports_language_selection: true,
            is_custom: false,
            files: Vec::new(),
            is_verifying: false,
        },
    );

    map.insert(
        "large".into(),
        ModelInfo {
            id: "large".into(),
            name: "Whisper Large".into(),
            description: "Good accuracy, but slow.".into(),
            filename: "ggml-large-v3-q5_0.bin".into(),
            url: Some("https://blob.handy.computer/ggml-large-v3-q5_0.bin".into()),
            sha256: Some("d75795ecff3f83b5faa89d1900604ad8c780abd5739fae406de19f23ecd98ad1".into()),
            size_mb: 1031,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: false,
            engine_type: EngineType::Whisper,
            accuracy_score: 0.85,
            speed_score: 0.30,
            supports_translation: true,
            supports_streaming: false,
            is_recommended: false,
            supported_languages: whisper_languages.clone(),
            supports_language_selection: true,
            is_custom: false,
            files: Vec::new(),
            is_verifying: false,
        },
    );

    map.insert(
        "breeze-asr".into(),
        ModelInfo {
            id: "breeze-asr".into(),
            name: "Breeze ASR".into(),
            description: "Optimized for Taiwanese Mandarin and Chinese code-switching. Other languages technically accepted but quality is not guaranteed.".into(),
            filename: "breeze-asr-q5_k.bin".into(),
            url: Some("https://blob.handy.computer/breeze-asr-q5_k.bin".into()),
            sha256: Some("8efbf0ce8a3f50fe332b7617da787fb81354b358c288b008d3bdef8359df64c6".into()),
            size_mb: 1030,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: false,
            engine_type: EngineType::Whisper,
            accuracy_score: 0.85,
            speed_score: 0.35,
            supports_translation: false,
            supports_streaming: false,
            is_recommended: false,
            supported_languages: vec!["zh".into(), "zh-Hans".into(), "zh-Hant".into(), "en".into()],
            supports_language_selection: true,
            is_custom: false,
            files: Vec::new(),
            is_verifying: false,
        },
    );

    // Parakeet V2 — English only, recommended for English speakers.
    map.insert(
        "parakeet-tdt-0.6b-v2".into(),
        ModelInfo {
            id: "parakeet-tdt-0.6b-v2".into(),
            name: "Parakeet V2".into(),
            description: "English only. The best model for English speakers.".into(),
            filename: "parakeet-tdt-0.6b-v2-int8".into(),
            url: Some("https://blob.handy.computer/parakeet-v2-int8.tar.gz".into()),
            sha256: Some("ac9b9429984dd565b25097337a887bb7f0f8ac393573661c651f0e7d31563991".into()),
            size_mb: 451,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: true,
            engine_type: EngineType::Parakeet,
            accuracy_score: 0.85,
            speed_score: 0.85,
            supports_translation: false,
            supports_streaming: false,
            // Recommended over V3 in THIS app: transcribe-rs treats both as
            // English-only, so V3's multilingual edge (its whole reason to be
            // preferred upstream in Handy) doesn't function here — leaving V2
            // strictly better for English (higher accuracy, same speed/size).
            is_recommended: true,
            supported_languages: vec!["en".to_string()],
            supports_language_selection: false,
            is_custom: false,
            files: Vec::new(),
            is_verifying: false,
        },
    );

    // NOTE: NVIDIA Parakeet-TDT v3 model weights support 25 European languages,
    // but transcribe-rs treats the Parakeet engine as English-only (ParakeetParams.language
    // is marked "currently unused" and CAPABILITIES.languages = &["en"]).
    // Update this entry when transcribe-rs wires up multilingual Parakeet support.
    map.insert(
        "parakeet-tdt-0.6b-v3".into(),
        ModelInfo {
            id: "parakeet-tdt-0.6b-v3".into(),
            name: "Parakeet V3".into(),
            description: "English only here (multilingual pending in transcribe-rs). For English, Parakeet V2 is more accurate — prefer it.".into(),
            filename: "parakeet-tdt-0.6b-v3-int8".into(),
            url: Some("https://blob.handy.computer/parakeet-v3-int8.tar.gz".into()),
            sha256: Some("43d37191602727524a7d8c6da0eef11c4ba24320f5b4730f1a2497befc2efa77".into()),
            size_mb: 456,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: true,
            engine_type: EngineType::Parakeet,
            accuracy_score: 0.80,
            speed_score: 0.85,
            supports_translation: false,
            supports_streaming: false,
            // Not recommended in THIS app — its multilingual advantage isn't
            // wired up in transcribe-rs, so it's just a lower-accuracy V2 here.
            is_recommended: false,
            supported_languages: vec!["en".to_string()],
            supports_language_selection: false,
            is_custom: false,
            files: Vec::new(),
            is_verifying: false,
        },
    );

    map.insert(
        "moonshine-base".into(),
        ModelInfo {
            id: "moonshine-base".into(),
            name: "Moonshine Base".into(),
            description: "Very fast, English only. Handles accents well.".into(),
            filename: "moonshine-base".into(),
            url: Some("https://blob.handy.computer/moonshine-base.tar.gz".into()),
            sha256: Some("04bf6ab012cfceebd4ac7cf88c1b31d027bbdd3cd704649b692e2e935236b7e8".into()),
            size_mb: 55,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: true,
            engine_type: EngineType::Moonshine,
            accuracy_score: 0.70,
            speed_score: 0.90,
            supports_translation: false,
            supports_streaming: false,
            is_recommended: false,
            supported_languages: vec!["en".into()],
            supports_language_selection: false,
            is_custom: false,
            files: Vec::new(),
            is_verifying: false,
        },
    );
    map.insert(
        "moonshine-tiny-streaming-en".into(),
        ModelInfo {
            id: "moonshine-tiny-streaming-en".into(),
            name: "Moonshine V2 Tiny".into(),
            description: "Ultra-fast, English only".into(),
            filename: "moonshine-tiny-streaming-en".into(),
            url: Some("https://blob.handy.computer/moonshine-tiny-streaming-en.tar.gz".into()),
            sha256: Some("465addcfca9e86117415677dfdc98b21edc53537210333a3ecdb58509a80abaf".into()),
            size_mb: 31,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: true,
            engine_type: EngineType::MoonshineStreaming,
            accuracy_score: 0.55,
            speed_score: 0.95,
            supports_translation: false,
            supports_streaming: true,
            is_recommended: false,
            supported_languages: vec!["en".into()],
            supports_language_selection: false,
            is_custom: false,
            files: Vec::new(),
            is_verifying: false,
        },
    );
    map.insert(
        "moonshine-small-streaming-en".into(),
        ModelInfo {
            id: "moonshine-small-streaming-en".into(),
            name: "Moonshine V2 Small".into(),
            description: "Fast, English only. Good balance of speed and accuracy.".into(),
            filename: "moonshine-small-streaming-en".into(),
            url: Some("https://blob.handy.computer/moonshine-small-streaming-en.tar.gz".into()),
            sha256: Some("dbb3e1c1832bd88a4ac712f7449a136cc2c9a18c5fe33a12ed1b7cb1cfe9cdd5".into()),
            size_mb: 99,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: true,
            engine_type: EngineType::MoonshineStreaming,
            accuracy_score: 0.65,
            speed_score: 0.90,
            supports_translation: false,
            supports_streaming: true,
            is_recommended: false,
            supported_languages: vec!["en".into()],
            supports_language_selection: false,
            is_custom: false,
            files: Vec::new(),
            is_verifying: false,
        },
    );
    map.insert(
        "moonshine-medium-streaming-en".into(),
        ModelInfo {
            id: "moonshine-medium-streaming-en".into(),
            name: "Moonshine V2 Medium".into(),
            description: "English only. High quality.".into(),
            filename: "moonshine-medium-streaming-en".into(),
            url: Some("https://blob.handy.computer/moonshine-medium-streaming-en.tar.gz".into()),
            sha256: Some("07a66f3bff1c77e75a2f637e5a263928a08baae3c29c4c053fc968a9a9373d13".into()),
            size_mb: 192,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: true,
            engine_type: EngineType::MoonshineStreaming,
            accuracy_score: 0.75,
            speed_score: 0.80,
            supports_translation: false,
            supports_streaming: true,
            is_recommended: false,
            supported_languages: vec!["en".into()],
            supports_language_selection: false,
            is_custom: false,
            files: Vec::new(),
            is_verifying: false,
        },
    );

    // zh-Hans/zh-Hant are NOT in the engine's lang2id map — selecting them causes
    // a TranscribeError::Config("Unknown language") at runtime. Use plain "zh".
    let sense_voice_languages: Vec<String> = ["zh", "en", "yue", "ja", "ko"]
        .into_iter()
        .map(String::from)
        .collect();
    map.insert(
        "sense-voice-int8".into(),
        ModelInfo {
            id: "sense-voice-int8".into(),
            name: "SenseVoice".into(),
            description: "Very fast. Chinese, English, Japanese, Korean, Cantonese.".into(),
            filename: "sense-voice-int8".into(),
            url: Some("https://blob.handy.computer/sense-voice-int8.tar.gz".into()),
            sha256: Some("171d611fe5d353a50bbb741b6f3ef42559b1565685684e9aa888ef563ba3e8a4".into()),
            size_mb: 152,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: true,
            engine_type: EngineType::SenseVoice,
            accuracy_score: 0.65,
            speed_score: 0.95,
            supports_translation: false,
            supports_streaming: false,
            is_recommended: false,
            supported_languages: sense_voice_languages,
            supports_language_selection: true,
            is_custom: false,
            files: Vec::new(),
            is_verifying: false,
        },
    );

    map.insert(
        "gigaam-v3-e2e-ctc".into(),
        ModelInfo {
            id: "gigaam-v3-e2e-ctc".into(),
            name: "GigaAM v3".into(),
            description: "Russian speech recognition. Fast and accurate.".into(),
            filename: "giga-am-v3-int8".into(),
            url: Some("https://blob.handy.computer/giga-am-v3-int8.tar.gz".into()),
            sha256: Some("d872462268430db140b69b72e0fc4b787b194c1dbe51b58de39444d55b6da45b".into()),
            size_mb: 151,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: true,
            engine_type: EngineType::GigaAM,
            accuracy_score: 0.85,
            speed_score: 0.75,
            supports_translation: false,
            supports_streaming: false,
            is_recommended: false,
            supported_languages: vec!["ru".into()],
            supports_language_selection: false,
            is_custom: false,
            files: Vec::new(),
            is_verifying: false,
        },
    );

    let canary_flash_languages: Vec<String> = ["en", "de", "es", "fr"]
        .into_iter()
        .map(String::from)
        .collect();
    map.insert(
        "canary-180m-flash".into(),
        ModelInfo {
            id: "canary-180m-flash".into(),
            name: "Canary 180M Flash".into(),
            description: "Very fast. English, German, Spanish, French. Supports translation."
                .into(),
            filename: "canary-180m-flash".into(),
            url: Some("https://blob.handy.computer/canary-180m-flash.tar.gz".into()),
            sha256: Some("6d9cfca6118b296e196eaedc1c8fa9788305a7b0f1feafdb6dc91932ab6e53f7".into()),
            size_mb: 146,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: true,
            engine_type: EngineType::Canary,
            accuracy_score: 0.75,
            speed_score: 0.85,
            supports_translation: true,
            supports_streaming: false,
            is_recommended: false,
            supported_languages: canary_flash_languages,
            supports_language_selection: true,
            is_custom: false,
            files: Vec::new(),
            is_verifying: false,
        },
    );

    let canary_1b_languages: Vec<String> = [
        "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt",
        "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    map.insert(
        "canary-1b-v2".into(),
        ModelInfo {
            id: "canary-1b-v2".into(),
            name: "Canary 1B v2".into(),
            description: "Accurate multilingual. 25 European languages. Supports translation."
                .into(),
            filename: "canary-1b-v2".into(),
            url: Some("https://blob.handy.computer/canary-1b-v2.tar.gz".into()),
            sha256: Some("02305b2a25f9cf3e7deaffa7f94df00efa44f442cd55c101c2cb9c000f904666".into()),
            size_mb: 691,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: true,
            engine_type: EngineType::Canary,
            accuracy_score: 0.85,
            speed_score: 0.70,
            supports_translation: true,
            supports_streaming: false,
            is_recommended: false,
            supported_languages: canary_1b_languages,
            supports_language_selection: true,
            is_custom: false,
            files: Vec::new(),
            is_verifying: false,
        },
    );

    // zh-Hans/zh-Hant are normalised to "zh" by the engine's build_prompt_ids —
    // they are not distinct capabilities. List only what CAPABILITIES declares.
    let cohere_languages: Vec<String> = [
        "en", "fr", "de", "it", "es", "pt", "el", "nl", "pl", "zh", "ja", "ko", "vi", "ar",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    map.insert(
        "cohere-int8".into(),
        ModelInfo {
            id: "cohere-int8".into(),
            name: "Cohere".into(),
            description: "A large, slower, but very accurate multilingual model.".into(),
            filename: "cohere-int8".into(),
            url: Some("https://blob.handy.computer/cohere-int8.tar.gz".into()),
            sha256: Some("ea2257d52434f3644574f187dcdcf666e302cd11b92866116ab8e14cd9c887f0".into()),
            size_mb: 1708,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: true,
            engine_type: EngineType::Cohere,
            accuracy_score: 0.90,
            speed_score: 0.60,
            supports_translation: false,
            supports_streaming: false,
            is_recommended: false,
            supported_languages: cohere_languages,
            supports_language_selection: true,
            is_custom: false,
            files: Vec::new(),
            is_verifying: false,
        },
    );

    // NVIDIA Nemotron-3.5-ASR Streaming 0.6B — multilingual streaming ASR.
    // Now implemented via parakeet-rs 0.3.6 (pinned for ort 2.0.0-rc.12 compatibility).
    // Uses the ONNX export from HuggingFace: pantinor/nemotron-3.5-asr-streaming-0.6b-onnx
    // INT8 build (smcleod/...-int8): byte-compatible filenames and graph, same
    // multilingual tokenizer, verified prompt_index present and left_context 56.
    // 651 MB vs 2.59 GB fp32; measured 2.28x faster per encoder chunk (37.3ms
    // vs 85.2ms) and 1.6x faster to load on an M3.
    let nemotron_languages: Vec<String> = [
        "en", "es", "fr", "de", "it", "pt", "nl", "pl", "ru", "zh", "ja", "ko", "ar", "hi", "tr",
        "vi", "th", "id", "ms", "sv", "da", "no", "fi", "cs", "ro", "hu", "uk", "el", "he", "ca",
        "bg", "hr", "sk", "sl", "et", "lt", "lv", "mt", "sq", "sr", "mk", "bs", "me", "is",
        // Present in parakeet-rs's PROMPT_DICTIONARY as locale-tagged keys and
        // reachable via `nemotron_lang_key`; omitting them here hid them from
        // the Settings picker even though the model supports them.
        "ne", "si", "bn", "ta", "te", "ml", "kn", "mr", "gu", "pa", "ur", "fa", "sw", "am",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    map.insert(
        "nemotron-3.5-asr-streaming-0.6b".into(),
        ModelInfo {
            id: "nemotron-3.5-asr-streaming-0.6b".into(),
            name: "Nemotron 3.5 ASR Streaming 0.6B".into(),
            description: "Multilingual streaming ASR with 40 language-locales, auto language detection, and punctuation.".into(),
            filename: "nemotron-3.5-asr-streaming-0.6b".into(),
            // No archive: HuggingFace publishes these as loose files and
            // has no repo-tarball endpoint, so `files` is used instead of
            // `url`. An invented `.tar.gz` path here returns 404 and the
            // model can never install — that is exactly the failure this
            // entry shipped with before.
            url: None,
            // Per-file checksums live in `files`; there is no archive to
            // hash.
            sha256: None,
            // 2_594_566_700 bytes across the four files below.
            size_mb: 651,
            is_downloaded: false,
            is_downloading: false,
            partial_size: 0,
            is_directory: true,
            engine_type: EngineType::Nemotron,
            accuracy_score: 0.85,
            speed_score: 0.75,
            supports_translation: false,
            supports_streaming: true,
            is_recommended: false,
            supported_languages: nemotron_languages,
            supports_language_selection: true,
            is_custom: false,
            // Names are load-bearing: `parakeet_rs::Nemotron::from_pretrained`
            // looks for these exact filenames in the model directory.
            // Checksums and sizes come from the HuggingFace LFS metadata
            // for the pinned repo revision.
            files: vec![
                ModelFile {
                    name: "encoder.onnx".into(),
                    url: NEMOTRON_FILE_BASE.to_string() + "encoder.onnx",
                    sha256: "a6fd0bbedae97047cb444dba928273b66b9cae36249cf697f4bf7b6f0e167c5d"
                        .into(),
                    size_bytes: 42_963_073,
                },
                ModelFile {
                    name: "encoder.onnx.data".into(),
                    url: NEMOTRON_FILE_BASE.to_string() + "encoder.onnx.data",
                    sha256: "c2f230b026aa4f29b1b5ce099b2fba853db361773157d478d67127b877f64c42"
                        .into(),
                    size_bytes: 614_649_600,
                },
                ModelFile {
                    name: "decoder_joint.onnx".into(),
                    url: NEMOTRON_FILE_BASE.to_string() + "decoder_joint.onnx",
                    sha256: "7fe1a8c2e247b55bbb8ca917ef64cf60227909c6fe63be2da7ea6fc3858d6a69"
                        .into(),
                    size_bytes: 24_483_962,
                },
                ModelFile {
                    name: "tokenizer.model".into(),
                    url: NEMOTRON_FILE_BASE.to_string() + "tokenizer.model",
                    sha256: "ce3895e40806f02a26c3a225161b96ef682d6c0054bae32a245dec4258d7d291"
                        .into(),
                    size_bytes: 406_554,
                },
            ],
            is_verifying: false,
        },
    );
}

/// Remove `path` whether it is a file, a directory, or absent.
///
/// A model id's on-disk path can legitimately change kind across
/// catalogue revisions — Nemotron shipped first as a single `.nemo`
/// archive and later as a directory of ONNX files — so install must not
/// assume the previous shape. Absence is success.
fn remove_path_any_kind(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Where the Nemotron ONNX export's files live.
const NEMOTRON_FILE_BASE: &str =
    "https://huggingface.co/smcleod/nemotron-3.5-asr-streaming-0.6b-int8/resolve/main/";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_includes_all_handy_engines() {
        let mut map = HashMap::new();
        populate_catalog(&mut map);
        let engines: HashSet<EngineType> = map.values().map(|m| m.engine_type).collect();
        assert!(engines.contains(&EngineType::Whisper));
        assert!(engines.contains(&EngineType::Parakeet));
        assert!(engines.contains(&EngineType::Moonshine));
        assert!(engines.contains(&EngineType::MoonshineStreaming));
        assert!(engines.contains(&EngineType::SenseVoice));
        assert!(engines.contains(&EngineType::GigaAM));
        assert!(engines.contains(&EngineType::Canary));
        assert!(engines.contains(&EngineType::Cohere));
        assert!(engines.contains(&EngineType::Nemotron));
    }

    /// Install must survive the target path being the wrong kind.
    ///
    /// Nemotron shipped first as a single `.nemo` archive and later as a
    /// directory of ONNX files, so anyone who tried the old entry has a
    /// 2.2 GB FILE where the directory now goes. `remove_dir_all` on a
    /// file fails with ENOTDIR, which aborted the install after all four
    /// files had downloaded and verified.
    #[test]
    fn removing_the_install_target_handles_file_dir_and_absent() {
        let tmp =
            std::env::temp_dir().join(format!("always-remove-any-kind-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // A file where a directory is expected — the case that broke.
        let as_file = tmp.join("model");
        fs::write(&as_file, b"stale archive").unwrap();
        assert!(as_file.is_file());
        remove_path_any_kind(&as_file).expect("must remove a file");
        assert!(!as_file.exists());

        // A populated directory.
        let as_dir = tmp.join("model");
        fs::create_dir_all(as_dir.join("nested")).unwrap();
        fs::write(as_dir.join("nested").join("f"), b"x").unwrap();
        remove_path_any_kind(&as_dir).expect("must remove a directory tree");
        assert!(!as_dir.exists());

        // Absent is success, not an error.
        remove_path_any_kind(&tmp.join("never-existed")).expect("absent must be ok");

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Every catalogue entry must be installable. The Nemotron entry
    /// shipped twice with a download that could not work — first a
    /// `.nemo` archive no engine could read, then an invented `.tar.gz`
    /// path that returned 404 — and both times the symptom was a model
    /// that downloaded nothing and spun forever.
    #[test]
    fn every_entry_has_a_usable_download_source() {
        let mut map = HashMap::new();
        populate_catalog(&mut map);
        for m in map.values() {
            assert!(
                m.url.is_some() || !m.files.is_empty(),
                "{} has neither a url nor a file list — it can never install",
                m.id
            );
            assert!(
                !(m.url.is_some() && !m.files.is_empty()),
                "{} declares both a url and a file list; `files` silently wins, \
                 so the url is a lie",
                m.id
            );
            for f in &m.files {
                assert!(
                    !m.url.is_some(),
                    "{}: multi-file entries must not also set url",
                    m.id
                );
                assert!(
                    m.is_directory,
                    "{}: a multi-file model lands in a directory, so is_directory must be true",
                    m.id
                );
                assert_eq!(
                    f.sha256.len(),
                    64,
                    "{}: file {} needs a real sha256 — unverified bytes were one of the \
                     original defects",
                    m.id,
                    f.name
                );
                assert!(
                    f.size_bytes > 0,
                    "{}: file {} needs a real size; progress is computed from it",
                    m.id,
                    f.name
                );
                assert!(
                    !f.name.contains('/') && !f.name.contains('\\') && !f.name.contains(".."),
                    "{}: file name {} could escape the model directory",
                    m.id,
                    f.name
                );
            }
            // A declared size that disagrees with the parts is how the
            // progress bar ends up running past 100%.
            if !m.files.is_empty() {
                let total: u64 = m.files.iter().map(|f| f.size_bytes).sum();
                let declared = m.size_mb * 1_000_000;
                let diff = declared.abs_diff(total);
                assert!(
                    diff * 100 / total < 5,
                    "{}: size_mb {} disagrees with the sum of its files ({} bytes)",
                    m.id,
                    m.size_mb,
                    total
                );
            }
        }
    }

    #[test]
    fn recommended_models_exist() {
        let mut map = HashMap::new();
        populate_catalog(&mut map);
        let recommended: Vec<&str> = map
            .values()
            .filter(|m| m.is_recommended)
            .map(|m| m.id.as_str())
            .collect();
        assert!(recommended.contains(&"parakeet-tdt-0.6b-v2"));
    }

    /// Nothing may be advertised that no engine can load. Every engine type
    /// in the catalog must have a corresponding implementation in stt_local.rs.
    #[test]
    fn catalog_never_offers_an_unloadable_engine() {
        let mut map = HashMap::new();
        populate_catalog(&mut map);
        let implemented_engines: HashSet<EngineType> = [
            EngineType::Whisper,
            EngineType::Parakeet,
            EngineType::Moonshine,
            EngineType::MoonshineStreaming,
            EngineType::SenseVoice,
            EngineType::GigaAM,
            EngineType::Canary,
            EngineType::Cohere,
            EngineType::Nemotron,
        ]
        .into_iter()
        .collect();

        for m in map.values() {
            assert!(
                implemented_engines.contains(&m.engine_type),
                "{} uses engine {:?} which has no implementation in stt_local.rs",
                m.id,
                m.engine_type
            );
        }
    }

    /// Superseded by `every_entry_has_a_usable_download_source`, which
    /// checks the same thing plus the multi-file case. Kept as the
    /// narrow single-archive check for entries that use `url`.
    #[test]
    fn each_catalog_entry_has_url_or_files() {
        let mut map = HashMap::new();
        populate_catalog(&mut map);
        for m in map.values() {
            assert!(
                m.url.is_some() || !m.files.is_empty(),
                "{} has no download source",
                m.id
            );
        }
    }

    #[test]
    fn catalog_entries_with_sha256_are_valid_format() {
        let mut map = HashMap::new();
        populate_catalog(&mut map);
        for m in map.values() {
            if let Some(sha) = &m.sha256 {
                assert_eq!(sha.len(), 64, "{} has malformed sha256", m.id);
            }
        }
    }

    /// Bare registry pointed at a temp dir — no background thread, no
    /// catalog — for exercising the verify-cache persistence in isolation.
    fn bare_registry(dir: &Path) -> ModelRegistry {
        let (events_tx, _rx) = broadcast::channel(4);
        ModelRegistry {
            models_dir: dir.to_path_buf(),
            available_models: Arc::new(Mutex::new(HashMap::new())),
            cancel_flags: Arc::new(Mutex::new(HashMap::new())),
            extracting_models: Arc::new(Mutex::new(HashSet::new())),
            events_tx,
            verify_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[test]
    fn verify_cache_round_trips_to_disk() {
        let dir = std::env::temp_dir().join(format!("always-verify-cache-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let key = (dir.join("ggml-small.bin"), 487_000_123u64, mtime);
        let registry = bare_registry(&dir);
        registry.verify_cache.lock().insert(key.clone(), true);
        registry.save_verify_cache();

        let reloaded = bare_registry(&dir);
        reloaded.load_verify_cache();
        assert_eq!(reloaded.verify_cache.lock().get(&key), Some(&true));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_verify_cache_is_discarded_gracefully() {
        let dir = std::env::temp_dir().join(format!(
            "always-verify-cache-corrupt-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".verify-cache.json"), "{not json[").unwrap();

        let registry = bare_registry(&dir);
        registry.load_verify_cache();
        assert!(registry.verify_cache.lock().is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_mtime_misses_cache() {
        let dir = std::env::temp_dir().join(format!("always-verify-stale-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // A real file whose verdict was cached under a DIFFERENT mtime —
        // the lookup must miss (file changed → re-hash required).
        let file = dir.join("model.bin");
        fs::write(&file, b"new bytes").unwrap();
        let stale_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        let registry = bare_registry(&dir);
        registry
            .verify_cache
            .lock()
            .insert((file.clone(), 9, stale_mtime), true);

        assert_eq!(registry.bin_cached_verdict(&file, Some("deadbeef")), None);

        let _ = fs::remove_dir_all(&dir);
    }
}
