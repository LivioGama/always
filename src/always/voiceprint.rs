//! Enrolled voice profile ("My Voice") — persistence + in-memory cache.
//!
//! The profile stores one speaker embedding per guided enrollment step
//! (normal / lower / louder tone, so the voiceprint spans the user's
//! dynamic range) plus the combined L2-normalised voiceprint the
//! runtime gate compares against. Persisted as JSON next to the
//! daemon's other config at `config_dir()/voiceprints.json`, ONE profile
//! per microphone (keyed by CoreAudio device UID): the same voice through two
//! mics embeds too differently for one voiceprint to verify both. A process-
//! wide cache avoids per-utterance disk reads and is refreshed by the
//! enrollment flow whenever a step is (re)recorded or a profile is deleted.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::always::input_device::{self, InputDevice};
use crate::always::speaker_embed;

/// Guided enrollment steps. Serialised by name — the UDS protocol and
/// the Swift UI use the same strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollStep {
    Normal,
    Lower,
    Louder,
}

impl EnrollStep {
    pub const ALL: [EnrollStep; 3] = [EnrollStep::Normal, EnrollStep::Lower, EnrollStep::Louder];

    pub fn as_str(&self) -> &'static str {
        match self {
            EnrollStep::Normal => "normal",
            EnrollStep::Lower => "lower",
            EnrollStep::Louder => "louder",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "normal" => Some(EnrollStep::Normal),
            "lower" => Some(EnrollStep::Lower),
            "louder" => Some(EnrollStep::Louder),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceProfile {
    /// Format version for forward migration.
    pub version: u32,
    /// Which embedding model produced these vectors — a profile from a
    /// different model must never be compared against embeddings from
    /// this one.
    pub model: String,
    /// Per-step embeddings (L2-normalised), keyed by step name so a
    /// single step can be re-recorded without redoing the others.
    pub steps: BTreeMap<String, Vec<f32>>,
    /// Combined voiceprint: sum of step embeddings, L2-normalised.
    pub voiceprint: Vec<f32>,
    pub created_at: String,
    pub updated_at: String,
}

impl VoiceProfile {
    pub fn is_complete(&self) -> bool {
        EnrollStep::ALL
            .iter()
            .all(|s| self.steps.contains_key(s.as_str()))
    }

    pub fn recorded_steps(&self) -> Vec<String> {
        self.steps.keys().cloned().collect()
    }
}

/// Pre-2026-11-27 single-profile file. Read once and imported as the
/// "unassigned" profile (mic unknown); kept for downgrades until a mic adopts
/// that profile or it is deleted, then removed so no stray copy remains.
pub fn profile_path() -> PathBuf {
    crate::config::config_dir().join("voiceprint.json")
}

/// One voiceprint per microphone (SPEC §6 "One voiceprint per microphone").
pub fn store_path() -> PathBuf {
    crate::config::config_dir().join("voiceprints.json")
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Key used when the live input device cannot be identified (non-macOS
/// builds, or no readable default input).
pub const DEFAULT_MIC_KEY: &str = "default";
/// `DeleteVoiceProfileForMic` key that targets the unassigned profile.
pub const UNASSIGNED_KEY: &str = "unassigned";

/// An unassigned profile is adopted by the mic it verifies the user on this
/// many times — enough that one lucky window on the wrong mic cannot claim it.
pub const ADOPT_AFTER_VERIFIED: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskMic {
    name: String,
    profile: VoiceProfile,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DiskStore {
    version: u32,
    #[serde(default)]
    mics: BTreeMap<String, DiskMic>,
    #[serde(default)]
    unassigned: Option<VoiceProfile>,
    #[serde(default)]
    unassigned_hits: BTreeMap<String, u32>,
    /// `voiceprint.json` was already imported — do not import it again
    /// after the user deletes the unassigned profile.
    #[serde(default)]
    legacy_imported: bool,
}

#[derive(Debug, Clone, Default)]
struct Store {
    mics: BTreeMap<String, (String, Arc<VoiceProfile>)>,
    unassigned: Option<Arc<VoiceProfile>>,
    unassigned_hits: BTreeMap<String, u32>,
    legacy_imported: bool,
    /// Profiles from another embedding model: never scored, but written
    /// back unchanged so they survive until that mic is re-recorded.
    foreign_mics: BTreeMap<String, DiskMic>,
    foreign_unassigned: Option<VoiceProfile>,
    /// The file could not be read or parsed. Nothing may be written over it
    /// (that would destroy every saved voiceprint); mutations are refused.
    /// Retried after [`LOAD_RETRY`], so a fixed file is picked up without a
    /// restart.
    load_failed: bool,
    failed_at: Option<std::time::Instant>,
}

/// How long a failed load is cached before the file is read again.
const LOAD_RETRY: std::time::Duration = std::time::Duration::from_secs(5);

impl Store {
    fn needs_reload(&self) -> bool {
        self.load_failed && self.failed_at.is_none_or(|t| t.elapsed() >= LOAD_RETRY)
    }
}

impl Store {
    fn to_disk(&self) -> DiskStore {
        let mut mics: BTreeMap<String, DiskMic> = self
            .foreign_mics
            .iter()
            .filter(|(k, _)| !self.mics.contains_key(*k))
            .map(|(k, m)| (k.clone(), m.clone()))
            .collect();
        for (k, (name, p)) in &self.mics {
            mics.insert(
                k.clone(),
                DiskMic {
                    name: name.clone(),
                    profile: (**p).clone(),
                },
            );
        }
        DiskStore {
            version: 2,
            mics,
            unassigned: self
                .unassigned
                .as_ref()
                .map(|p| (**p).clone())
                .or_else(|| self.foreign_unassigned.clone()),
            unassigned_hits: self.unassigned_hits.clone(),
            legacy_imported: self.legacy_imported,
        }
    }

    fn from_disk(d: DiskStore) -> Self {
        let usable = |p: &VoiceProfile| {
            let ok = p.model == speaker_embed::MODEL_FILE;
            if !ok {
                tracing::warn!(
                    model = %p.model,
                    "voiceprint from different model — ignoring (re-enroll needed)"
                );
            }
            ok
        };
        let mut store = Store {
            unassigned_hits: d.unassigned_hits,
            legacy_imported: d.legacy_imported,
            ..Store::default()
        };
        for (k, m) in d.mics {
            if usable(&m.profile) {
                store.mics.insert(k, (m.name, Arc::new(m.profile)));
            } else {
                store.foreign_mics.insert(k, m);
            }
        }
        match d.unassigned {
            Some(p) if usable(&p) => store.unassigned = Some(Arc::new(p)),
            Some(p) => store.foreign_unassigned = Some(p),
            None => {}
        }
        store
    }

    fn any_complete(&self) -> bool {
        self.mics.values().any(|(_, p)| p.is_complete())
            || self.unassigned.as_ref().is_some_and(|p| p.is_complete())
    }
}

/// Where the profile the gate is using came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Recorded on the live microphone.
    ThisMic,
    /// Recorded before per-mic storage; which mic is unknown.
    Unassigned,
    /// Recorded on another microphone (name given) — the live mic has none.
    OtherMic(String),
}

#[derive(Debug, Clone)]
pub struct Selection {
    pub profile: Arc<VoiceProfile>,
    pub source: Source,
    /// The mic this selection was made for (`None` = unidentified).
    pub device: Option<InputDevice>,
}

/// One microphone with a saved voiceprint, for the Settings list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicSummary {
    /// Device UID, or [`UNASSIGNED_KEY`] for the pre-per-mic profile.
    pub uid: String,
    pub name: String,
    pub complete: bool,
}

/// What the GUI needs to show My Voice for the live mic.
#[derive(Debug, Clone, Default)]
pub struct MicStatus {
    /// Display name of the live mic (`None` = unknown).
    pub mic: Option<String>,
    /// The live mic has its own complete voiceprint.
    pub mic_enrolled: bool,
    /// Steps recorded on the live mic.
    pub steps: Vec<String>,
    /// Any complete voiceprint exists (the gate can run).
    pub any_enrolled: bool,
    pub mics: Vec<MicSummary>,
}

/// Process-wide cache. `None` = not yet loaded from disk.
static CACHE: RwLock<Option<Arc<Store>>> = RwLock::new(None);

/// Serialises every read-modify-write of the store (and the lazy load), so a
/// gate-thread adoption can never write back a snapshot older than a
/// concurrent enrollment or delete, and two persists never share the tmp file.
static WRITER: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

fn store() -> Arc<Store> {
    if let Some(s) = CACHE.read().as_ref()
        && !s.needs_reload()
    {
        return s.clone();
    }
    let _w = WRITER.lock();
    load_locked()
}

/// The cached store, loading it if needed (or retrying a failed load).
/// Caller holds [`WRITER`].
fn load_locked() -> Arc<Store> {
    if let Some(s) = CACHE.read().as_ref()
        && !s.needs_reload()
    {
        return s.clone();
    }
    let loaded = Arc::new(load_from_disk().unwrap_or_else(|e| {
        tracing::error!(error = %e, "voiceprint_load_failed_store_read_only");
        Store {
            load_failed: true,
            failed_at: Some(std::time::Instant::now()),
            legacy_imported: true,
            ..Store::default()
        }
    }));
    *CACHE.write() = Some(loaded.clone());
    loaded
}

/// Run one read-modify-write under [`WRITER`]. Refused after a failed load.
fn mutate<R>(f: impl FnOnce(&mut Store) -> Result<R>) -> Result<R> {
    mutate_if(|store| f(store).map(|r| (r, true)))
}

/// Like [`mutate`], but the closure says whether it changed the store; an
/// unchanged store is not written (no disk write, no clobbering of a change
/// another process made).
fn mutate_if<R>(f: impl FnOnce(&mut Store) -> Result<(R, bool)>) -> Result<R> {
    let _w = WRITER.lock();
    let current = load_locked();
    if current.load_failed {
        anyhow::bail!(
            "voiceprints.json could not be read — refusing to overwrite it (fix or remove {})",
            store_path().display()
        );
    }
    let mut next = (*current).clone();
    let (r, changed) = f(&mut next)?;
    if changed {
        persist(&next)?;
        *CACHE.write() = Some(Arc::new(next));
    }
    Ok(r)
}

fn mic_key(dev: Option<&InputDevice>) -> String {
    dev.map(|d| d.uid.clone())
        .unwrap_or_else(|| DEFAULT_MIC_KEY.to_string())
}

fn mic_name(dev: Option<&InputDevice>) -> String {
    dev.map(|d| d.name.clone())
        .unwrap_or_else(|| "Default microphone".to_string())
}

/// Pick the profile for `dev`: its own complete one, else the unassigned
/// one, else the most recently updated complete profile of another mic,
/// else whatever partial profile `dev` has (enrollment in progress).
fn select(store: &Store, dev: Option<&InputDevice>) -> Option<Selection> {
    let key = mic_key(dev);
    let own = store.mics.get(&key).map(|(_, p)| p.clone());
    let pick = |profile: Arc<VoiceProfile>, source: Source| Selection {
        profile,
        source,
        device: dev.cloned(),
    };
    if let Some(p) = own.as_ref().filter(|p| p.is_complete()) {
        return Some(pick(p.clone(), Source::ThisMic));
    }
    if let Some(p) = store.unassigned.as_ref().filter(|p| p.is_complete()) {
        return Some(pick(p.clone(), Source::Unassigned));
    }
    if let Some((name, p)) = store
        .mics
        .iter()
        .filter(|(k, (_, p))| **k != key && p.is_complete())
        .map(|(_, v)| v)
        .max_by(|a, b| a.1.updated_at.cmp(&b.1.updated_at))
    {
        return Some(pick(p.clone(), Source::OtherMic(name.clone())));
    }
    own.map(|p| pick(p, Source::ThisMic))
}

/// Device the adaptive pool was filled on; the pool is device-specific.
static POOL_DEVICE: parking_lot::Mutex<Option<String>> = parking_lot::Mutex::new(None);

/// The profile the gate uses for the live mic, with where it came from.
/// Called per utterance, so it stays cheap (cached store + cached device).
pub fn selection() -> Option<Selection> {
    let dev = input_device::current();
    let key = mic_key(dev.as_ref());
    let changed = {
        let mut pool_dev = POOL_DEVICE.lock();
        let changed = pool_dev.as_deref() != Some(key.as_str());
        if changed {
            if pool_dev.is_some() {
                clear_recent_accepted();
            }
            *pool_dev = Some(key);
        }
        changed
    };
    let sel = select(&store(), dev.as_ref());
    if changed {
        tracing::info!(
            mic = %mic_name(dev.as_ref()),
            source = ?sel.as_ref().map(|s| &s.source),
            "voiceprint_selected_for_mic"
        );
    }
    sel
}

/// The profile the gate compares against for the live mic, if any.
pub fn current() -> Option<Arc<VoiceProfile>> {
    selection().map(|s| s.profile)
}

/// True when any complete profile for the current model is enrolled — the
/// precondition for the runtime gate. On a mic without its own voiceprint the
/// gate still runs (against another mic's) so strangers are never typed.
pub fn is_enrolled() -> bool {
    store().any_complete()
}

/// Snapshot for `VoiceProfileStatus`.
pub fn mic_status() -> MicStatus {
    let dev = input_device::current();
    let store = store();
    let key = mic_key(dev.as_ref());
    let own = store.mics.get(&key);
    let mut mics: Vec<MicSummary> = store
        .mics
        .iter()
        .map(|(uid, (name, p))| MicSummary {
            uid: uid.clone(),
            name: name.clone(),
            complete: p.is_complete(),
        })
        .collect();
    if let Some(p) = store.unassigned.as_ref() {
        mics.push(MicSummary {
            uid: UNASSIGNED_KEY.to_string(),
            name: "Earlier recording (mic unknown)".to_string(),
            complete: p.is_complete(),
        });
    }
    MicStatus {
        mic: dev.map(|d| d.name),
        mic_enrolled: own.is_some_and(|(_, p)| p.is_complete()),
        steps: own.map(|(_, p)| p.recorded_steps()).unwrap_or_default(),
        any_enrolled: store.any_complete(),
        mics,
    }
}

fn load_from_disk() -> Result<Store> {
    let path = store_path();
    let mut disk: DiskStore = match std::fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str(&t).context("parsing voiceprints.json")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DiskStore::default(),
        Err(e) => return Err(e).context("reading voiceprints.json"),
    };
    let mut migrated = false;
    if !disk.legacy_imported {
        // A bad legacy file must never block the per-mic store: log, skip,
        // and retry the import on the next start.
        match std::fs::read_to_string(profile_path())
            .map_err(anyhow::Error::from)
            .and_then(|t| serde_json::from_str::<VoiceProfile>(&t).map_err(Into::into))
        {
            Ok(legacy) => {
                tracing::info!("voiceprint_legacy_imported_unassigned");
                disk.unassigned = Some(legacy);
                disk.legacy_imported = true;
                migrated = true;
            }
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
            {
                disk.legacy_imported = true;
            }
            Err(e) => tracing::warn!(error = %e, "voiceprint_legacy_import_skipped"),
        }
    }
    let mut store = Store::from_disk(disk);
    // Without a device identity every capture is the same "default" mic, so
    // the old single profile simply is that mic's (Linux keeps its behaviour).
    if cfg!(not(target_os = "macos"))
        && let Some(p) = store.unassigned.take()
    {
        store
            .mics
            .entry(DEFAULT_MIC_KEY.to_string())
            .or_insert_with(|| (mic_name(None), p));
        migrated = true;
    }
    if migrated {
        match persist(&store) {
            // Adopted by the "default" mic at once: the embeddings now live in
            // the store, so the old file must not linger.
            // A legacy profile from another embedding model is kept, unused,
            // for a downgrade (same rule as `clear_mic`).
            Ok(())
                if store.unassigned.is_none()
                    && store.foreign_unassigned.is_none()
                    && store.legacy_imported =>
            {
                remove_legacy_file()
            }
            Ok(()) => {}
            // The profiles were read fine: keep using them, retry next start.
            Err(e) => tracing::warn!(error = %e, "voiceprint_migration_persist_failed"),
        }
    }
    Ok(store)
}

fn persist(store: &Store) -> Result<()> {
    let path = store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&store.to_disk())?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Delete the pre-per-mic file once its embeddings live in the store under a
/// mic (or were deleted), so no copy of the voiceprint is left behind.
fn remove_legacy_file() {
    match std::fs::remove_file(profile_path()) {
        Ok(()) => tracing::info!("voiceprint_legacy_file_removed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(error = %e, "voiceprint_legacy_file_remove_failed"),
    }
}

/// Record (or re-record) one enrollment step's embedding for the LIVE mic,
/// recombine its voiceprint, persist, and refresh the cache. Other mics'
/// voiceprints are untouched. Returns the updated profile.
pub fn set_step(step: EnrollStep, embedding: Vec<f32>) -> Result<Arc<VoiceProfile>> {
    if embedding.len() != speaker_embed::EMBEDDING_DIM {
        anyhow::bail!(
            "embedding dim {} != {}",
            embedding.len(),
            speaker_embed::EMBEDDING_DIM
        );
    }
    let dev = input_device::refresh();
    // On macOS the mic is always identifiable; saving under "default" would
    // invent a phantom mic and could split one enrollment across two keys.
    if cfg!(target_os = "macos") && dev.is_none() {
        anyhow::bail!("could not identify the current microphone — try again");
    }
    let key = mic_key(dev.as_ref());
    let name = mic_name(dev.as_ref());
    let arc = mutate(|store| {
        let mut profile = store
            .mics
            .get(&key)
            .map(|(_, p)| (**p).clone())
            .unwrap_or_else(|| VoiceProfile {
                version: 1,
                model: speaker_embed::MODEL_FILE.to_string(),
                steps: BTreeMap::new(),
                voiceprint: Vec::new(),
                created_at: now_iso(),
                updated_at: now_iso(),
            });
        profile.steps.insert(step.as_str().to_string(), embedding);
        let all: Vec<Vec<f32>> = profile.steps.values().cloned().collect();
        profile.voiceprint = speaker_embed::combine_embeddings(&all)?;
        profile.updated_at = now_iso();
        let arc = Arc::new(profile);
        store.foreign_mics.remove(&key);
        store.mics.insert(key.clone(), (name.clone(), arc.clone()));
        Ok(arc)
    })?;
    tracing::info!(mic = %name, step = step.as_str(), "voiceprint_step_saved");
    clear_recent_accepted();
    Ok(arc)
}

/// The user was just verified against `verified` (an utterance passed the
/// threshold) on `device`, the mic that utterance was captured from. When
/// that profile is the unassigned one, count it toward `device`; after
/// [`ADOPT_AFTER_VERIFIED`] the unassigned profile becomes that mic's own
/// (replacing any abandoned partial enrollment there). Returns true when an
/// adoption happened (status changed).
pub fn note_verified(device: Option<&InputDevice>, verified: &Arc<VoiceProfile>) -> bool {
    let Some(dev) = device else {
        return false;
    };
    // Cheap pre-check on the cached snapshot: almost every accepted
    // utterance is verified against a mic's own voiceprint, and must not
    // take the writer lock or touch the disk.
    let is_unassigned = |store: &Store| {
        store
            .unassigned
            .as_ref()
            .is_some_and(|u| Arc::ptr_eq(u, verified))
    };
    if !is_unassigned(&store()) {
        return false;
    }
    let result = mutate_if(|store| {
        let has_own = store
            .mics
            .get(&dev.uid)
            .is_some_and(|(_, p)| p.is_complete());
        if !is_unassigned(store) || has_own {
            return Ok((None, false));
        }
        let hits = store.unassigned_hits.entry(dev.uid.clone()).or_insert(0);
        *hits += 1;
        if *hits < ADOPT_AFTER_VERIFIED {
            return Ok((Some(false), true));
        }
        if let Some(p) = store.unassigned.take() {
            store.mics.insert(dev.uid.clone(), (dev.name.clone(), p));
        }
        store.unassigned_hits.clear();
        Ok((Some(true), true))
    });
    match result {
        Ok(Some(true)) => {
            tracing::info!(mic = %dev.name, "voiceprint_adopted_by_mic");
            remove_legacy_file();
            true
        }
        Ok(_) => false,
        Err(e) => {
            tracing::warn!(error = %e, "voiceprint_adopt_persist_failed");
            false
        }
    }
}

/// Delete one mic's voiceprint (`uid` from [`MicSummary`], or
/// [`UNASSIGNED_KEY`]). Other mics are untouched. Errors when `uid` names no
/// saved voiceprint, so a typo is never reported as a deletion.
pub fn clear_mic(uid: &str) -> Result<()> {
    mutate(|store| {
        let found = if uid == UNASSIGNED_KEY {
            store.unassigned_hits.clear();
            store.unassigned.take().is_some() | store.foreign_unassigned.take().is_some()
        } else {
            store.mics.remove(uid).is_some() | store.foreign_mics.remove(uid).is_some()
        };
        if !found {
            anyhow::bail!("no saved voiceprint for mic '{uid}'");
        }
        Ok(())
    })?;
    // The legacy file holds the same embeddings as the unassigned or adopted
    // profile; once it was imported and no unassigned profile (of any model)
    // remains, it has no other purpose. A skipped (unreadable) import is left
    // for the next start's retry.
    let after = store();
    if after.legacy_imported && after.unassigned.is_none() && after.foreign_unassigned.is_none() {
        remove_legacy_file();
    }
    clear_recent_accepted();
    Ok(())
}

/// Delete every voiceprint (all mics, the unassigned one, and the
/// pre-per-mic file).
pub fn clear() -> Result<()> {
    let _w = WRITER.lock();
    for path in [store_path(), profile_path()] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("deleting {}", path.display())),
        }
    }
    *CACHE.write() = Some(Arc::new(Store {
        legacy_imported: true,
        ..Store::default()
    }));
    clear_recent_accepted();
    Ok(())
}

/// Recently ACCEPTED own-voice utterance embeddings — an in-memory
/// adaptive extension of the enrolled profile (never persisted, never
/// leaves the process; I9).
///
/// Enrollment is recorded in a quiet room; real dictation happens over
/// music and video, where the mixture's embedding drifts away from the
/// clean enrollment styles (measured 2026-10-14 on the owner's own
/// recordings mixed with local music / TTS speech: own-voice utterance
/// pass rate at 0 dB music 0.64, under 5 dB background speech 0.32).
/// Scoring against "the best of enrolled styles + the user's own recent
/// accepted utterances" raised those to 0.72 / 0.60 (0.84 / 0.76 with
/// the media decision budget), with the max stranger / music-only /
/// stranger-over-music window score unchanged at <= 0.30 vs the 0.50
/// threshold.
///
/// Drift guard: an embedding is only admitted when it clears the
/// threshold against the ENROLLED profile alone (see the caller), so the
/// pool can never chain away from the enrolled voice.
const RECENT_ACCEPTED_MAX: usize = 8;

/// The pool may only lift a window that already sounds like the user
/// against the ENROLLED profile. An accepted utterance spoken over a
/// video is a mixture, so its pool embedding carries the video's voice
/// too, and that voice alone then matched it: 2026-11-28 prod, video
/// speech verified at 0.52-0.56 via "recent" (3 pastes) while the same
/// video scored 0.05-0.20 everywhere else. 0.30 is the measured stranger
/// ceiling against the enrolled profile (see above and
/// `speaker_trim::FOREIGN_MAX_SCORE`).
pub const RECENT_POOL_ENROLLED_FLOOR: f32 = 0.30;

/// Combine the enrolled score with the best pool score. Returns the
/// score to judge and whether the pool supplied it. Never below
/// `enrolled`; the pool is ignored when `enrolled` is under
/// [`RECENT_POOL_ENROLLED_FLOOR`].
pub fn lift_with_recent(enrolled: f32, recent: f32) -> (f32, bool) {
    if enrolled >= RECENT_POOL_ENROLLED_FLOOR && recent > enrolled {
        (recent, true)
    } else {
        (enrolled, false)
    }
}

static RECENT_ACCEPTED: parking_lot::Mutex<std::collections::VecDeque<Vec<f32>>> =
    parking_lot::Mutex::new(std::collections::VecDeque::new());

/// Admit one accepted own-voice embedding (oldest evicted beyond
/// `RECENT_ACCEPTED_MAX`). Wrong-dimension embeddings are ignored.
pub fn remember_accepted(embedding: Vec<f32>) {
    if embedding.len() != speaker_embed::EMBEDDING_DIM {
        return;
    }
    let mut pool = RECENT_ACCEPTED.lock();
    pool.push_back(embedding);
    while pool.len() > RECENT_ACCEPTED_MAX {
        pool.pop_front();
    }
}

/// Snapshot of the adaptive pool (cheap: <= 8 x 256 floats).
pub fn recent_accepted() -> Vec<Vec<f32>> {
    RECENT_ACCEPTED.lock().iter().cloned().collect()
}

/// Forget the adaptive pool — on any (re-)enrollment or profile delete,
/// so a new voiceprint never inherits the previous one's samples.
pub fn clear_recent_accepted() {
    RECENT_ACCEPTED.lock().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialise access: every test rebinds ALWAYS_CONFIG_DIR and the
    /// process-wide CACHE, so they must not interleave.
    static TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    fn with_temp_config<R>(f: impl FnOnce() -> R) -> R {
        let _guard = TEST_LOCK.lock();
        let dir = std::env::temp_dir().join(format!("always-vp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: guarded by TEST_LOCK; tests in this mod are the only
        // users of this env var.
        unsafe { std::env::set_var("ALWAYS_CONFIG_DIR", &dir) };
        *CACHE.write() = None;
        *POOL_DEVICE.lock() = None;
        input_device::set_override(Some(Some(mic("elgato"))));
        let r = f();
        input_device::set_override(None);
        unsafe { std::env::remove_var("ALWAYS_CONFIG_DIR") };
        *CACHE.write() = None;
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    fn mic(uid: &str) -> InputDevice {
        InputDevice {
            uid: uid.to_string(),
            name: format!("{uid} mic"),
        }
    }

    fn use_mic(uid: &str) {
        input_device::set_override(Some(Some(mic(uid))));
    }

    fn enroll_all(seed: f32) {
        for (i, step) in EnrollStep::ALL.iter().enumerate() {
            set_step(*step, fake_embedding(seed + i as f32)).unwrap();
        }
    }

    fn write_legacy(seed: f32) {
        write_legacy_model(seed, speaker_embed::MODEL_FILE);
    }

    fn write_legacy_model(seed: f32, model: &str) {
        let steps: BTreeMap<String, Vec<f32>> = EnrollStep::ALL
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str().to_string(), fake_embedding(seed + i as f32)))
            .collect();
        let all: Vec<Vec<f32>> = steps.values().cloned().collect();
        let legacy = VoiceProfile {
            version: 1,
            model: model.to_string(),
            voiceprint: speaker_embed::combine_embeddings(&all).unwrap(),
            steps,
            created_at: now_iso(),
            updated_at: now_iso(),
        };
        std::fs::write(profile_path(), serde_json::to_string(&legacy).unwrap()).unwrap();
        *CACHE.write() = None;
    }

    #[test]
    fn each_mic_keeps_its_own_voiceprint() {
        with_temp_config(|| {
            use_mic("elgato");
            enroll_all(1.0);
            let elgato = current().unwrap().voiceprint.clone();
            // The built-in mic has none of its own: the gate still runs on the
            // Elgato's, and the status says this mic is not set up.
            use_mic("builtin");
            let sel = selection().unwrap();
            assert_eq!(sel.source, Source::OtherMic("elgato mic".into()));
            let st = mic_status();
            assert!(st.any_enrolled && !st.mic_enrolled);
            assert_eq!(st.mic.as_deref(), Some("builtin mic"));
            assert!(is_enrolled());
            // Recording on the built-in mic never touches the Elgato's.
            enroll_all(5.0);
            assert_eq!(selection().unwrap().source, Source::ThisMic);
            let builtin = current().unwrap().voiceprint.clone();
            assert_ne!(builtin, elgato);
            use_mic("elgato");
            assert_eq!(current().unwrap().voiceprint, elgato, "auto-switch back");
            // Survives a fresh process.
            *CACHE.write() = None;
            assert_eq!(current().unwrap().voiceprint, elgato);
            assert_eq!(mic_status().mics.len(), 2);
        });
    }

    #[test]
    fn switching_mic_resets_adaptive_pool() {
        with_temp_config(|| {
            enroll_all(1.0);
            let _ = selection();
            remember_accepted(fake_embedding(7.0));
            let _ = selection();
            assert_eq!(recent_accepted().len(), 1, "same mic keeps the pool");
            use_mic("builtin");
            let _ = selection();
            assert!(recent_accepted().is_empty(), "embeddings are mic-specific");
        });
    }

    /// Verify on `uid` against whatever the gate selects for it.
    fn verify_on(uid: &str) -> bool {
        use_mic(uid);
        let sel = selection().unwrap();
        note_verified(sel.device.as_ref(), &sel.profile)
    }

    // macOS only: elsewhere the legacy profile is the "default" mic's at once.
    #[cfg(target_os = "macos")]
    #[test]
    fn legacy_profile_is_adopted_by_the_mic_it_verifies_on() {
        with_temp_config(|| {
            write_legacy(1.0);
            use_mic("elgato");
            let sel = selection().unwrap();
            assert_eq!(sel.source, Source::Unassigned);
            assert!(!mic_status().mic_enrolled);
            assert!(store_path().exists(), "import persisted");
            assert!(profile_path().exists(), "legacy file kept until adopted");
            for _ in 0..(ADOPT_AFTER_VERIFIED - 1) {
                assert!(!verify_on("elgato"));
            }
            assert!(verify_on("elgato"), "third verification adopts");
            assert_eq!(selection().unwrap().source, Source::ThisMic);
            assert!(mic_status().mic_enrolled);
            assert!(!profile_path().exists(), "no stray copy after adoption");
            // Deleting it later does not resurrect anything.
            clear_mic("elgato").unwrap();
            *CACHE.write() = None;
            assert!(current().is_none());
            assert!(clear_mic("elgato").is_err(), "unknown uid is an error");
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn verifications_on_another_mic_do_not_adopt_for_this_one() {
        with_temp_config(|| {
            write_legacy(1.0);
            assert!(!verify_on("builtin"));
            assert!(!verify_on("elgato"));
            assert!(!verify_on("elgato"));
            assert!(verify_on("elgato"), "hits are counted per mic");
            assert!(mic_status().mics.iter().any(|m| m.uid == "elgato"));
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn abandoned_partial_enrollment_does_not_block_adoption() {
        with_temp_config(|| {
            write_legacy(1.0);
            use_mic("elgato");
            set_step(EnrollStep::Normal, fake_embedding(9.0)).unwrap();
            assert_eq!(selection().unwrap().source, Source::Unassigned);
            for _ in 0..ADOPT_AFTER_VERIFIED {
                verify_on("elgato");
            }
            assert!(mic_status().mic_enrolled);
        });
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn legacy_profile_is_the_default_mic_without_device_identity() {
        with_temp_config(|| {
            input_device::set_override(Some(None));
            write_legacy(1.0);
            assert_eq!(selection().unwrap().source, Source::ThisMic);
            assert!(mic_status().mic_enrolled);
        });
    }

    #[test]
    fn legacy_profile_from_another_model_is_kept_on_disk() {
        with_temp_config(|| {
            write_legacy_model(1.0, "old-model.onnx");
            assert!(current().is_none(), "another model's profile is unused");
            assert!(store_path().exists(), "import persisted");
            assert!(profile_path().exists(), "kept for a downgrade");
        });
    }

    #[test]
    fn fixed_store_is_reread_without_restart() {
        with_temp_config(|| {
            enroll_all(1.0);
            let good = std::fs::read_to_string(store_path()).unwrap();
            std::fs::write(store_path(), "{ not json").unwrap();
            *CACHE.write() = None;
            assert!(!is_enrolled());
            std::fs::write(store_path(), good).unwrap();
            // Force the retry window to have elapsed.
            if let Some(s) = CACHE.write().as_mut() {
                let mut st = (**s).clone();
                st.failed_at = None;
                *s = Arc::new(st);
            }
            assert!(is_enrolled(), "fixed file picked up");
        });
    }

    #[test]
    fn verifying_against_an_own_voiceprint_writes_nothing() {
        with_temp_config(|| {
            enroll_all(1.0);
            let before = std::fs::metadata(store_path()).unwrap().modified().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
            let sel = selection().unwrap();
            assert!(!note_verified(sel.device.as_ref(), &sel.profile));
            let after = std::fs::metadata(store_path()).unwrap().modified().unwrap();
            assert_eq!(before, after);
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn deleting_another_mic_keeps_an_unimported_legacy_file() {
        with_temp_config(|| {
            enroll_all(1.0);
            let mut disk: DiskStore =
                serde_json::from_str(&std::fs::read_to_string(store_path()).unwrap()).unwrap();
            disk.legacy_imported = false;
            std::fs::write(store_path(), serde_json::to_string(&disk).unwrap()).unwrap();
            std::fs::write(profile_path(), "unreadable for now").unwrap();
            *CACHE.write() = None;
            clear_mic("elgato").unwrap();
            assert!(profile_path().exists(), "kept for the next start's retry");
        });
    }

    #[test]
    fn unreadable_store_is_never_overwritten() {
        with_temp_config(|| {
            enroll_all(1.0);
            std::fs::write(store_path(), "{ not json").unwrap();
            *CACHE.write() = None;
            assert!(!is_enrolled());
            assert!(set_step(EnrollStep::Normal, fake_embedding(2.0)).is_err());
            assert!(clear_mic("elgato").is_err());
            assert_eq!(std::fs::read_to_string(store_path()).unwrap(), "{ not json");
        });
    }

    #[test]
    fn corrupt_legacy_file_does_not_hide_per_mic_profiles() {
        with_temp_config(|| {
            enroll_all(1.0);
            let mut disk: DiskStore =
                serde_json::from_str(&std::fs::read_to_string(store_path()).unwrap()).unwrap();
            disk.legacy_imported = false;
            std::fs::write(store_path(), serde_json::to_string(&disk).unwrap()).unwrap();
            std::fs::write(profile_path(), "garbage").unwrap();
            *CACHE.write() = None;
            assert!(is_enrolled());
            assert_eq!(selection().unwrap().source, Source::ThisMic);
        });
    }

    #[test]
    fn other_model_profiles_survive_rewrites() {
        with_temp_config(|| {
            enroll_all(1.0);
            let mut disk: DiskStore =
                serde_json::from_str(&std::fs::read_to_string(store_path()).unwrap()).unwrap();
            let mut old = disk.mics["elgato"].clone();
            old.profile.model = "old-model.onnx".into();
            disk.mics.insert("old-mic".into(), old);
            std::fs::write(store_path(), serde_json::to_string(&disk).unwrap()).unwrap();
            *CACHE.write() = None;
            use_mic("builtin");
            enroll_all(5.0);
            let disk: DiskStore =
                serde_json::from_str(&std::fs::read_to_string(store_path()).unwrap()).unwrap();
            assert!(disk.mics.contains_key("old-mic"), "kept until re-recorded");
        });
    }

    #[test]
    fn clear_mic_removes_only_that_mic() {
        with_temp_config(|| {
            enroll_all(1.0);
            use_mic("builtin");
            enroll_all(5.0);
            clear_mic("builtin").unwrap();
            assert_eq!(
                selection().unwrap().source,
                Source::OtherMic("elgato mic".into())
            );
            clear().unwrap();
            assert!(current().is_none());
            assert!(!store_path().exists());
        });
    }

    fn fake_embedding(seed: f32) -> Vec<f32> {
        let v: Vec<f32> = (0..speaker_embed::EMBEDDING_DIM)
            .map(|i| ((i as f32) * 0.01 + seed).sin())
            .collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / norm).collect()
    }

    #[test]
    fn recent_accepted_pool_is_bounded_and_reset_by_enrollment() {
        with_temp_config(|| {
            clear_recent_accepted();
            remember_accepted(vec![0.0; 3]); // wrong dim: ignored
            assert!(recent_accepted().is_empty());
            for i in 0..(RECENT_ACCEPTED_MAX + 3) {
                remember_accepted(fake_embedding(i as f32));
            }
            let pool = recent_accepted();
            assert_eq!(pool.len(), RECENT_ACCEPTED_MAX);
            // Oldest evicted first.
            assert_eq!(pool[0], fake_embedding(3.0));
            // A (re-)enrollment never inherits the previous pool.
            set_step(EnrollStep::Normal, fake_embedding(9.0)).unwrap();
            assert!(recent_accepted().is_empty());
            remember_accepted(fake_embedding(1.0));
            clear().unwrap();
            assert!(recent_accepted().is_empty());
        });
    }

    #[test]
    fn recent_pool_never_lifts_a_window_far_from_the_enrolled_voice() {
        // Video voice matching a mixture in the pool (2026-11-28 prod).
        assert_eq!(lift_with_recent(0.12, 0.54), (0.12, false));
        assert_eq!(lift_with_recent(0.29, 0.90), (0.29, false));
        // Own voice over media: near the enrolled voice, the pool lifts it.
        assert_eq!(lift_with_recent(0.35, 0.56), (0.56, true));
        assert_eq!(lift_with_recent(RECENT_POOL_ENROLLED_FLOOR, 0.6), (0.6, true));
        // Never below the enrolled-only score.
        assert_eq!(lift_with_recent(0.55, 0.40), (0.55, false));
        assert_eq!(lift_with_recent(0.55, f32::MIN), (0.55, false));
    }

    #[test]
    fn no_profile_initially() {
        with_temp_config(|| {
            assert!(current().is_none());
            assert!(!is_enrolled());
        });
    }

    #[test]
    fn enrollment_completes_after_all_three_steps() {
        with_temp_config(|| {
            set_step(EnrollStep::Normal, fake_embedding(1.0)).unwrap();
            assert!(!is_enrolled(), "one step is not enough");
            set_step(EnrollStep::Lower, fake_embedding(2.0)).unwrap();
            set_step(EnrollStep::Louder, fake_embedding(3.0)).unwrap();
            assert!(is_enrolled());
            let p = current().unwrap();
            assert_eq!(p.steps.len(), 3);
            assert_eq!(p.voiceprint.len(), speaker_embed::EMBEDDING_DIM);
            let norm: f32 = p.voiceprint.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-4);
        });
    }

    #[test]
    fn profile_survives_cache_flush_via_disk() {
        with_temp_config(|| {
            for (i, step) in EnrollStep::ALL.iter().enumerate() {
                set_step(*step, fake_embedding(i as f32)).unwrap();
            }
            *CACHE.write() = None; // simulate fresh process
            assert!(is_enrolled(), "profile should reload from disk");
        });
    }

    #[test]
    fn re_recording_one_step_updates_voiceprint() {
        with_temp_config(|| {
            for (i, step) in EnrollStep::ALL.iter().enumerate() {
                set_step(*step, fake_embedding(i as f32)).unwrap();
            }
            let before = current().unwrap().voiceprint.clone();
            set_step(EnrollStep::Normal, fake_embedding(9.0)).unwrap();
            let after = current().unwrap().voiceprint.clone();
            assert_ne!(before, after);
            assert_eq!(current().unwrap().steps.len(), 3, "still 3 steps");
        });
    }

    #[test]
    fn clear_removes_profile() {
        with_temp_config(|| {
            set_step(EnrollStep::Normal, fake_embedding(1.0)).unwrap();
            clear().unwrap();
            assert!(current().is_none());
            assert!(!profile_path().exists());
            assert!(!store_path().exists());
        });
    }

    #[test]
    fn step_parse_round_trips() {
        for s in EnrollStep::ALL {
            assert_eq!(EnrollStep::parse(s.as_str()), Some(s));
        }
        assert_eq!(EnrollStep::parse("bogus"), None);
    }
}
