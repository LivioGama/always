use anyhow::Result;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::LazyLock;

// Global mutex for atomic file operations. parking_lot is poison-free,
// so a panic in a holder does not stall every future caller.
static STATE_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonState {
    pub listening: bool,
    pub processing: bool,
    pub transcribing: bool, // New field for transcription state
    pub paused: bool,
    pub auto_enter: bool,
    pub voice_activity: bool, // Early energy detection before VAD confirmation
    pub last_transcript: Option<String>,
    pub last_updated: u64,
    pub version: u64, // Version counter for detecting stale state
}

impl Default for DaemonState {
    fn default() -> Self {
        Self {
            listening: false,
            processing: false,
            transcribing: false,
            paused: false,
            auto_enter: false,
            voice_activity: false,
            last_transcript: None,
            last_updated: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            version: 1,
        }
    }
}

impl DaemonState {
    pub fn state_file_path() -> Result<PathBuf> {
        let config_dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
            .join(".config")
            .join("always");

        fs::create_dir_all(&config_dir)?;
        Ok(config_dir.join("state.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::state_file_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&path)?;
        let state: Self = serde_json::from_str(&content)?;
        Ok(state)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::state_file_path()?;

        // Atomic write using temporary file
        let temp_path = path.with_extension("tmp");
        let content = serde_json::to_string_pretty(self)?;

        fs::write(&temp_path, content)?;
        fs::rename(temp_path, path)?;

        Ok(())
    }

    pub fn set_listening(listening: bool) -> Result<()> {
        let _lock = STATE_MUTEX.lock();
        let mut state = Self::load().unwrap_or_default();
        state.listening = listening;
        state.version += 1;
        state.last_updated = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        state.save()
    }

    pub fn set_processing(processing: bool) -> Result<()> {
        let _lock = STATE_MUTEX.lock();
        let mut state = Self::load().unwrap_or_default();
        state.processing = processing;
        state.version += 1;
        state.last_updated = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        state.save()
    }

    pub fn set_transcribing(transcribing: bool) -> Result<()> {
        let _lock = STATE_MUTEX.lock();
        let mut state = Self::load().unwrap_or_default();
        state.transcribing = transcribing;
        state.processing = !transcribing; // Clear processing when starting transcription
        state.version += 1;
        state.last_updated = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        state.save()
    }

    pub fn set_transcript(transcript: String) -> Result<()> {
        let _lock = STATE_MUTEX.lock();
        let mut state = Self::load().unwrap_or_default();
        state.last_transcript = Some(transcript);
        state.processing = false;
        state.transcribing = false;
        state.listening = false; // Stop overlay when paste is done
        state.version += 1;
        state.last_updated = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        state.save()
    }

    pub fn set_paused(paused: bool) -> Result<()> {
        let _lock = STATE_MUTEX.lock();
        let mut state = Self::load().unwrap_or_default();
        state.paused = paused;
        state.version += 1;
        state.last_updated = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        state.save()
    }

    pub fn set_auto_enter(auto_enter: bool) -> Result<()> {
        let _lock = STATE_MUTEX.lock();
        let mut state = Self::load().unwrap_or_default();
        state.auto_enter = auto_enter;
        state.version += 1;
        state.last_updated = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        state.save()
    }

    pub fn set_voice_activity(voice_activity: bool) -> Result<()> {
        let _lock = STATE_MUTEX.lock();
        let mut state = Self::load().unwrap_or_default();
        state.voice_activity = voice_activity;
        state.version += 1;
        state.last_updated = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        state.save()
    }
}
