//! Enrolled voice profile ("My Voice") — persistence + in-memory cache.
//!
//! The profile stores one speaker embedding per guided enrollment step
//! (normal / lower / louder tone, so the voiceprint spans the user's
//! dynamic range) plus the combined L2-normalised voiceprint the
//! runtime gate compares against. Persisted as JSON next to the
//! daemon's other config at `config_dir()/voiceprint.json`; a process-
//! wide cache avoids per-utterance disk reads and is refreshed by the
//! enrollment flow whenever a step is (re)recorded or the profile is
//! deleted.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

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

pub fn profile_path() -> PathBuf {
    crate::config::config_dir().join("voiceprint.json")
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Process-wide cache. `None` in the outer Option = not yet loaded
/// from disk; inner `None` = loaded, no profile exists.
#[allow(clippy::type_complexity)]
static CACHE: RwLock<Option<Option<Arc<VoiceProfile>>>> = RwLock::new(None);

/// The enrolled profile, if any — cached after first disk read. This
/// is what the per-utterance gate calls, so it must stay cheap.
pub fn current() -> Option<Arc<VoiceProfile>> {
    if let Some(loaded) = CACHE.read().as_ref() {
        return loaded.clone();
    }
    let loaded = load_from_disk().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "voiceprint_load_failed");
        None
    });
    let arc = loaded.map(Arc::new);
    *CACHE.write() = Some(arc.clone());
    arc
}

/// True when a complete profile for the current model is enrolled —
/// the precondition for the runtime gate.
pub fn is_enrolled() -> bool {
    current()
        .map(|p| p.is_complete() && p.model == speaker_embed::MODEL_FILE)
        .unwrap_or(false)
}

fn load_from_disk() -> Result<Option<VoiceProfile>> {
    let path = profile_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("reading voiceprint.json"),
    };
    let profile: VoiceProfile = serde_json::from_str(&text).context("parsing voiceprint.json")?;
    if profile.model != speaker_embed::MODEL_FILE {
        tracing::warn!(
            model = %profile.model,
            "voiceprint from different model — ignoring (re-enroll needed)"
        );
        return Ok(None);
    }
    Ok(Some(profile))
}

fn persist(profile: &VoiceProfile) -> Result<()> {
    let path = profile_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(profile)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Record (or re-record) one enrollment step's embedding, recombine
/// the voiceprint, persist, and refresh the cache. Returns the updated
/// profile.
pub fn set_step(step: EnrollStep, embedding: Vec<f32>) -> Result<Arc<VoiceProfile>> {
    if embedding.len() != speaker_embed::EMBEDDING_DIM {
        anyhow::bail!(
            "embedding dim {} != {}",
            embedding.len(),
            speaker_embed::EMBEDDING_DIM
        );
    }
    let mut profile = current().map(|p| (*p).clone()).unwrap_or_else(|| VoiceProfile {
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
    persist(&profile)?;
    let arc = Arc::new(profile);
    *CACHE.write() = Some(Some(arc.clone()));
    Ok(arc)
}

/// Delete the enrolled profile (file + cache).
pub fn clear() -> Result<()> {
    let path = profile_path();
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("deleting voiceprint.json"),
    }
    *CACHE.write() = Some(None);
    Ok(())
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
        let r = f();
        unsafe { std::env::remove_var("ALWAYS_CONFIG_DIR") };
        *CACHE.write() = None;
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    fn fake_embedding(seed: f32) -> Vec<f32> {
        let v: Vec<f32> = (0..speaker_embed::EMBEDDING_DIM)
            .map(|i| ((i as f32) * 0.01 + seed).sin())
            .collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / norm).collect()
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
