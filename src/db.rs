//! SQLite database — preferences for STT-only always.
//!
//! All state is persisted in `~/.config/always/always.db` with WAL mode enabled.
//! Schema is auto-migrated on first open.

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::config;

const ENCODED_SECRET_PREFIX: &str = "hex:";

#[derive(Debug, Clone, Default)]
pub struct Preferences {
    pub lang: Option<String>,
    pub stt_threshold: Option<f64>,
    pub stt_energy_threshold: Option<f64>,
    pub stt_cooldown_ms: Option<u32>,
    pub always_log_path: Option<String>,
    pub hear_energy_threshold: Option<f64>,
    pub stt_silence: Option<f64>,
    pub stt_trim_silence: Option<bool>,
    pub stt_auto_enter: Option<bool>,
    pub deepgram_api_key: Option<String>,
    pub groq_api_key: Option<String>,
    pub deepgram_model: Option<String>,
    pub silero_threshold: Option<f64>,
    pub shortcut_pause: Option<String>,
    pub shortcut_auto_enter: Option<String>,
    pub shortcut_force_paste: Option<String>,
    pub postprocess_enabled: Option<bool>,
    pub shortcut_log_correction: Option<String>,
    pub passive_correction_capture: Option<bool>,
    /// Auto-enter countdown delay (ms). 0 = immediate (legacy).
    pub auto_enter_delay_ms: Option<u32>,
    /// Auto-pause after no voice for this many seconds. 0 = disabled.
    pub idle_pause_secs: Option<u32>,
    /// Action to take when idle timeout occurs: "pause" or "pause_and_mute".
    pub idle_pause_action: Option<String>,
    /// Shortcut to open the manual correction dialog (intended-word input).
    pub shortcut_correction_dialog: Option<String>,
    /// JSON-encoded per-app overrides: `{ "<bundle_id>": { "auto_enter": bool, "paused": bool } }`.
    pub per_app_settings_json: Option<String>,
    /// Active STT backend. Stored as `groq` or `local:<model_id>`.
    /// Parsed via [`crate::stt_dispatch::TranscriberBackendChoice`].
    pub transcriber_backend: Option<String>,
    /// Shortcut for the global (master) pause toggle. The plain pause
    /// shortcut is strictly per-app; this chord is the explicit
    /// "pause/resume everything" switch (default ctrl+alt+shift+p).
    pub shortcut_master_pause: Option<String>,
    /// Stream accepted utterances to `~/.always/transcripts.jsonl` for
    /// external consumers (e.g. IRIS). Opt-in (default off).
    pub transcript_stream: Option<bool>,
    /// Extend the end-of-utterance silence window when the speculative
    /// transcript looks mid-sentence, so brief thinking pauses don't
    /// split one thought into two pastes. Default on.
    pub stt_adaptive_silence: Option<bool>,
    /// "My Voice" gate: when enabled AND a voiceprint is enrolled,
    /// only speech matching the enrolled speaker is transcribed.
    /// Default off (opt-in via Settings → My Voice).
    pub speaker_gate_enabled: Option<bool>,
    /// Minimum cosine similarity against the enrolled voiceprint for
    /// an utterance to pass the gate. Default 0.50.
    pub speaker_gate_threshold: Option<f64>,
    /// Status sound setting: off, low, medium, or high.
    pub audible_status_sound: Option<String>,
}

pub fn open() -> Result<Connection> {
    let path = config::db_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create config directory")?;
    }
    let conn = Connection::open(&path).context("Failed to open database")?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS preferences (
            id      INTEGER PRIMARY KEY CHECK (id = 1),
            lang    TEXT,
            stt_threshold REAL,
            stt_silence REAL,
            stt_trim_silence INTEGER,
            stt_auto_enter INTEGER
        );",
    )?;

    // Add STT-related columns if they don't exist (migration for existing DBs)
    let has_stt_energy_threshold = conn
        .prepare("SELECT stt_energy_threshold FROM preferences LIMIT 0")
        .is_ok();
    if !has_stt_energy_threshold {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN stt_energy_threshold REAL;")?;
    }

    let has_stt_cooldown_ms = conn
        .prepare("SELECT stt_cooldown_ms FROM preferences LIMIT 0")
        .is_ok();
    if !has_stt_cooldown_ms {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN stt_cooldown_ms INTEGER;")?;
    }

    let has_always_log_path = conn
        .prepare("SELECT always_log_path FROM preferences LIMIT 0")
        .is_ok();
    if !has_always_log_path {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN always_log_path TEXT;")?;
    }

    let has_hear_energy_threshold = conn
        .prepare("SELECT hear_energy_threshold FROM preferences LIMIT 0")
        .is_ok();
    if !has_hear_energy_threshold {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN hear_energy_threshold REAL;")?;
    }

    let has_groq_api_key = conn
        .prepare("SELECT groq_api_key FROM preferences LIMIT 0")
        .is_ok();
    if !has_groq_api_key {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN groq_api_key TEXT;")?;
    }

    let has_deepgram_api_key = conn
        .prepare("SELECT deepgram_api_key FROM preferences LIMIT 0")
        .is_ok();
    if !has_deepgram_api_key {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN deepgram_api_key TEXT;")?;
    }

    let has_deepgram_model = conn
        .prepare("SELECT deepgram_model FROM preferences LIMIT 0")
        .is_ok();
    if !has_deepgram_model {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN deepgram_model TEXT;")?;
    }

    let has_silero_threshold = conn
        .prepare("SELECT silero_threshold FROM preferences LIMIT 0")
        .is_ok();
    if !has_silero_threshold {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN silero_threshold REAL;")?;
    }

    let has_shortcut_pause = conn
        .prepare("SELECT shortcut_pause FROM preferences LIMIT 0")
        .is_ok();
    if !has_shortcut_pause {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN shortcut_pause TEXT;")?;
    }

    let has_shortcut_auto_enter = conn
        .prepare("SELECT shortcut_auto_enter FROM preferences LIMIT 0")
        .is_ok();
    if !has_shortcut_auto_enter {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN shortcut_auto_enter TEXT;")?;
    }

    let has_shortcut_force_paste = conn
        .prepare("SELECT shortcut_force_paste FROM preferences LIMIT 0")
        .is_ok();
    if !has_shortcut_force_paste {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN shortcut_force_paste TEXT;")?;
    }

    let has_shortcut_master_pause = conn
        .prepare("SELECT shortcut_master_pause FROM preferences LIMIT 0")
        .is_ok();
    if !has_shortcut_master_pause {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN shortcut_master_pause TEXT;")?;
    }

    let has_postprocess_enabled = conn
        .prepare("SELECT postprocess_enabled FROM preferences LIMIT 0")
        .is_ok();
    if !has_postprocess_enabled {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN postprocess_enabled INTEGER;")?;
    }

    let has_shortcut_log_correction = conn
        .prepare("SELECT shortcut_log_correction FROM preferences LIMIT 0")
        .is_ok();
    if !has_shortcut_log_correction {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN shortcut_log_correction TEXT;")?;
    }

    let has_passive_correction_capture = conn
        .prepare("SELECT passive_correction_capture FROM preferences LIMIT 0")
        .is_ok();
    if !has_passive_correction_capture {
        conn.execute_batch(
            "ALTER TABLE preferences ADD COLUMN passive_correction_capture INTEGER;",
        )?;
    }

    let has_auto_enter_delay_ms = conn
        .prepare("SELECT auto_enter_delay_ms FROM preferences LIMIT 0")
        .is_ok();
    if !has_auto_enter_delay_ms {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN auto_enter_delay_ms INTEGER;")?;
    }

    let has_idle_pause_secs = conn
        .prepare("SELECT idle_pause_secs FROM preferences LIMIT 0")
        .is_ok();
    if !has_idle_pause_secs {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN idle_pause_secs INTEGER;")?;
    }

    let has_idle_pause_action = conn
        .prepare("SELECT idle_pause_action FROM preferences LIMIT 0")
        .is_ok();
    if !has_idle_pause_action {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN idle_pause_action TEXT;")?;
    }

    let has_shortcut_correction_dialog = conn
        .prepare("SELECT shortcut_correction_dialog FROM preferences LIMIT 0")
        .is_ok();
    if !has_shortcut_correction_dialog {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN shortcut_correction_dialog TEXT;")?;
    }

    let has_per_app_settings_json = conn
        .prepare("SELECT per_app_settings_json FROM preferences LIMIT 0")
        .is_ok();
    if !has_per_app_settings_json {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN per_app_settings_json TEXT;")?;
    }

    let has_transcriber_backend = conn
        .prepare("SELECT transcriber_backend FROM preferences LIMIT 0")
        .is_ok();
    if !has_transcriber_backend {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN transcriber_backend TEXT;")?;
    }

    let has_transcript_stream = conn
        .prepare("SELECT transcript_stream FROM preferences LIMIT 0")
        .is_ok();
    if !has_transcript_stream {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN transcript_stream INTEGER;")?;
    }

    let has_stt_adaptive_silence = conn
        .prepare("SELECT stt_adaptive_silence FROM preferences LIMIT 0")
        .is_ok();
    if !has_stt_adaptive_silence {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN stt_adaptive_silence INTEGER;")?;
    }

    let has_speaker_gate_enabled = conn
        .prepare("SELECT speaker_gate_enabled FROM preferences LIMIT 0")
        .is_ok();
    if !has_speaker_gate_enabled {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN speaker_gate_enabled INTEGER;")?;
    }

    let has_speaker_gate_threshold = conn
        .prepare("SELECT speaker_gate_threshold FROM preferences LIMIT 0")
        .is_ok();
    if !has_speaker_gate_threshold {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN speaker_gate_threshold REAL;")?;
    }

    let has_legacy_audible_status_cues = conn
        .prepare("SELECT audible_status_cues FROM preferences LIMIT 0")
        .is_ok();
    let has_legacy_audible_status_cue_volume = conn
        .prepare("SELECT audible_status_cue_volume FROM preferences LIMIT 0")
        .is_ok();
    let has_audible_status_sound = conn
        .prepare("SELECT audible_status_sound FROM preferences LIMIT 0")
        .is_ok();
    if !has_audible_status_sound {
        conn.execute_batch("ALTER TABLE preferences ADD COLUMN audible_status_sound TEXT;")?;
    }
    if has_legacy_audible_status_cues {
        if has_legacy_audible_status_cue_volume {
            conn.execute_batch(
                "UPDATE preferences
                 SET audible_status_sound = CASE
                     WHEN COALESCE(audible_status_cues, 0) != 0
                         THEN COALESCE(audible_status_cue_volume, 'medium')
                     ELSE 'off'
                 END
                 WHERE audible_status_sound IS NULL;",
            )?;
        } else {
            conn.execute_batch(
                "UPDATE preferences
                 SET audible_status_sound = CASE
                     WHEN COALESCE(audible_status_cues, 0) != 0 THEN 'medium'
                     ELSE 'off'
                 END
                 WHERE audible_status_sound IS NULL;",
            )?;
        }
    }

    encode_plaintext_groq_key(conn)?;

    Ok(())
}

fn encode_plaintext_groq_key(conn: &Connection) -> Result<()> {
    let value: Option<String> = conn
        .query_row(
            "SELECT groq_api_key FROM preferences WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or(None);
    let Some(value) = value else {
        return Ok(());
    };
    if value.is_empty() || value.starts_with(ENCODED_SECRET_PREFIX) {
        return Ok(());
    }
    let encoded = encode_secret(&value);
    conn.execute(
        "UPDATE preferences SET groq_api_key = ?1 WHERE id = 1",
        [encoded.as_str()],
    )?;
    Ok(())
}

// --- Preferences ---

pub fn get_preferences(conn: &Connection) -> Result<Preferences> {
    let mut stmt = conn.prepare(
        "SELECT lang, stt_threshold, stt_energy_threshold, stt_cooldown_ms, always_log_path, hear_energy_threshold, stt_silence, stt_trim_silence, stt_auto_enter, deepgram_api_key, groq_api_key, deepgram_model, silero_threshold, shortcut_pause, shortcut_auto_enter, shortcut_force_paste, postprocess_enabled, shortcut_log_correction, passive_correction_capture, auto_enter_delay_ms, idle_pause_secs, idle_pause_action, shortcut_correction_dialog, per_app_settings_json, transcriber_backend, shortcut_master_pause, transcript_stream, stt_adaptive_silence, speaker_gate_enabled, speaker_gate_threshold, audible_status_sound FROM preferences WHERE id = 1",
    )?;
    let result = stmt.query_row([], |row| {
        Ok(Preferences {
            lang: row.get(0)?,
            stt_threshold: row.get(1)?,
            stt_energy_threshold: row.get(2)?,
            stt_cooldown_ms: row.get(3)?,
            always_log_path: row.get(4)?,
            hear_energy_threshold: row.get(5)?,
            stt_silence: row.get(6)?,
            stt_trim_silence: row.get::<_, Option<i64>>(7)?.map(|v| v != 0),
            stt_auto_enter: row.get::<_, Option<i64>>(8)?.map(|v| v != 0),
            deepgram_api_key: row.get(9)?,
            groq_api_key: decode_secret(row.get(10)?),
            deepgram_model: row.get(11)?,
            silero_threshold: row.get(12)?,
            shortcut_pause: row.get(13)?,
            shortcut_auto_enter: row.get(14)?,
            shortcut_force_paste: row.get(15)?,
            postprocess_enabled: row.get::<_, Option<i64>>(16)?.map(|v| v != 0),
            shortcut_log_correction: row.get(17)?,
            passive_correction_capture: row.get::<_, Option<i64>>(18)?.map(|v| v != 0),
            auto_enter_delay_ms: row.get::<_, Option<i64>>(19)?.map(|v| v as u32),
            idle_pause_secs: row.get::<_, Option<i64>>(20)?.map(|v| v as u32),
            idle_pause_action: row.get(21)?,
            shortcut_correction_dialog: row.get(22)?,
            per_app_settings_json: row.get(23)?,
            transcriber_backend: row.get(24)?,
            shortcut_master_pause: row.get(25)?,
            transcript_stream: row.get::<_, Option<i64>>(26)?.map(|v| v != 0),
            stt_adaptive_silence: row.get::<_, Option<i64>>(27)?.map(|v| v != 0),
            speaker_gate_enabled: row.get::<_, Option<i64>>(28)?.map(|v| v != 0),
            speaker_gate_threshold: row.get(29)?,
            audible_status_sound: row.get(30)?,
        })
    });
    match result {
        Ok(prefs) => Ok(prefs),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Preferences::default()),
        Err(e) => Err(e.into()),
    }
}

#[allow(clippy::collapsible_match)]
// Each match arm validates a distinct key. Collapsing the inner `if` into
// a guard would force the validation logic onto the arm pattern line and
// make the rules harder to scan.
pub fn set_preference(conn: &Connection, key: &str, value: &str) -> Result<()> {
    let valid_keys = [
        "lang",
        "stt_threshold",
        "stt_energy_threshold",
        "stt_cooldown_ms",
        "always_log_path",
        "hear_energy_threshold",
        "stt_silence",
        "stt_trim_silence",
        "stt_auto_enter",
        "deepgram_api_key",
        "groq_api_key",
        "deepgram_model",
        "silero_threshold",
        "shortcut_pause",
        "shortcut_auto_enter",
        "shortcut_force_paste",
        "postprocess_enabled",
        "shortcut_log_correction",
        "passive_correction_capture",
        "auto_enter_delay_ms",
        "idle_pause_secs",
        "idle_pause_action",
        "shortcut_correction_dialog",
        "per_app_settings_json",
        "transcriber_backend",
        "shortcut_master_pause",
        "transcript_stream",
        "stt_adaptive_silence",
        "speaker_gate_enabled",
        "speaker_gate_threshold",
        "audible_status_sound",
    ];
    if !valid_keys.contains(&key) {
        anyhow::bail!(
            "Unknown preference: {key}. Valid keys: {}",
            valid_keys.join(", ")
        );
    }

    // Validate specific keys
    match key {
        "lang" => {
            if !config::SUPPORTED_LANGS.contains(&value) {
                anyhow::bail!(
                    "Unsupported language: {value}. Supported: {}",
                    config::SUPPORTED_LANGS.join(", ")
                );
            }
        }
        "stt_threshold" => {
            let parsed = value
                .parse::<f64>()
                .context("stt_threshold must be a number")?;
            if !(0.1..=10.0).contains(&parsed) {
                anyhow::bail!("stt_threshold must be between 0.1 and 10.0 (percent)");
            }
        }
        "stt_energy_threshold" => {
            let parsed = value
                .parse::<f64>()
                .context("stt_energy_threshold must be a number")?;
            if !(0.0..=1.0).contains(&parsed) {
                anyhow::bail!("stt_energy_threshold must be between 0.0 and 1.0");
            }
        }
        "stt_cooldown_ms" => {
            let parsed = value
                .parse::<u32>()
                .context("stt_cooldown_ms must be a number")?;
            if !(0..=5000).contains(&parsed) {
                anyhow::bail!("stt_cooldown_ms must be between 0 and 5000 milliseconds");
            }
        }
        "always_log_path" => {
            if value.is_empty() {
                anyhow::bail!("always_log_path cannot be empty");
            }
        }
        "hear_energy_threshold" => {
            let parsed = value
                .parse::<f64>()
                .context("hear_energy_threshold must be a number")?;
            if !(0.0..=1.0).contains(&parsed) {
                anyhow::bail!("hear_energy_threshold must be between 0.0 and 1.0");
            }
        }
        "stt_silence" => {
            let parsed = value
                .parse::<f64>()
                .context("stt_silence must be a number")?;
            // Mirror config::SILENCE_SECS_MIN/MAX — the resolver clamps
            // to the same range, so rejecting here keeps the stored
            // value and the effective value identical.
            if !(0.3..=15.0).contains(&parsed) {
                anyhow::bail!("stt_silence must be between 0.3 and 15.0 seconds");
            }
        }
        "silero_threshold" => {
            let parsed = value
                .parse::<f64>()
                .context("silero_threshold must be a number")?;
            if !(0.1..=0.9).contains(&parsed) {
                anyhow::bail!("silero_threshold must be between 0.1 and 0.9");
            }
        }
        "stt_trim_silence"
        | "stt_auto_enter"
        | "postprocess_enabled"
        | "passive_correction_capture"
        | "transcript_stream"
        | "stt_adaptive_silence"
        | "speaker_gate_enabled" => {
            if !matches!(value, "true" | "false" | "1" | "0") {
                anyhow::bail!("{key} must be one of: true, false, 1, 0");
            }
        }
        "speaker_gate_threshold" => {
            let parsed = value
                .parse::<f64>()
                .context("speaker_gate_threshold must be a number")?;
            // Below 0.30 nearly everything passes; above 0.80 the
            // enrolled speaker starts getting rejected on short or
            // noisy utterances.
            if !(0.30..=0.80).contains(&parsed) {
                anyhow::bail!("speaker_gate_threshold must be between 0.30 and 0.80");
            }
        }
        "auto_enter_delay_ms" => {
            let parsed = value
                .parse::<u32>()
                .context("auto_enter_delay_ms must be a non-negative integer")?;
            if parsed > 60_000 {
                anyhow::bail!("auto_enter_delay_ms must be <= 60000");
            }
        }
        "idle_pause_secs" => {
            let parsed = value
                .parse::<u32>()
                .context("idle_pause_secs must be a non-negative integer")?;
            if parsed > 86_400 {
                anyhow::bail!("idle_pause_secs must be <= 86400 (1 day)");
            }
        }
        "idle_pause_action" => {
            if !matches!(value, "pause" | "pause_and_mute") {
                anyhow::bail!("idle_pause_action must be one of: pause, pause_and_mute");
            }
        }
        "audible_status_sound" => {
            let _: crate::always::status_sound::StatusSoundSetting = value.parse()?;
        }
        "per_app_settings_json" => {
            if !value.is_empty() {
                let _: serde_json::Value = serde_json::from_str(value)
                    .context("per_app_settings_json must be valid JSON")?;
            }
        }
        "transcriber_backend" => {
            // Reuse the canonical parser so `groq` / `local:<id>` is
            // the single source of truth for the wire format.
            let _: crate::stt_dispatch::TranscriberBackendChoice = value
                .parse()
                .context("transcriber_backend must be 'groq' or 'local:<model_id>'")?;
        }
        _ => {}
    }

    // Upsert: insert or update
    conn.execute(
        "INSERT INTO preferences (id, lang, stt_threshold, stt_energy_threshold, stt_cooldown_ms, always_log_path, hear_energy_threshold, stt_silence, stt_trim_silence, stt_auto_enter, deepgram_api_key, groq_api_key, deepgram_model, silero_threshold)
         VALUES (1, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)
         ON CONFLICT(id) DO NOTHING",
        [],
    )?;
    let sql = format!("UPDATE preferences SET {key} = ?1 WHERE id = 1");
    let normalized = match key {
        "groq_api_key" => encode_secret(value),
        "audible_status_sound" => value
            .parse::<crate::always::status_sound::StatusSoundSetting>()?
            .as_str()
            .to_string(),
        "stt_trim_silence"
        | "stt_auto_enter"
        | "postprocess_enabled"
        | "passive_correction_capture"
        | "transcript_stream"
        | "stt_adaptive_silence"
        | "speaker_gate_enabled" => {
            if matches!(value, "true" | "1") {
                "1".to_string()
            } else {
                "0".to_string()
            }
        }
        _ => value.to_string(),
    };
    conn.execute(&sql, [normalized.as_str()])?;

    // Invalidate the in-memory per-app overrides cache when the JSON
    // blob changes — otherwise the daemon keeps applying the previous
    // overrides until the next process restart.
    if key == "per_app_settings_json" {
        crate::always::per_app::invalidate_cache();
    }
    Ok(())
}

pub fn reset_preferences(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM preferences WHERE id = 1", [])?;
    // A full reset wipes per_app_settings_json too, so the cache must
    // re-read from a now-empty source.
    crate::always::per_app::invalidate_cache();
    Ok(())
}

pub fn get_silero_threshold(conn: &Connection) -> Result<Option<f64>> {
    let prefs = get_preferences(conn)?;
    Ok(prefs.silero_threshold)
}

pub fn set_silero_threshold(conn: &Connection, value: f64) -> Result<()> {
    set_preference(conn, "silero_threshold", &value.to_string())
}

fn encode_secret(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let mut encoded = String::with_capacity(ENCODED_SECRET_PREFIX.len() + value.len() * 2);
    encoded.push_str(ENCODED_SECRET_PREFIX);
    for byte in value.as_bytes() {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

fn decode_secret(value: Option<String>) -> Option<String> {
    let value = value?;
    let Some(hex) = value.strip_prefix(ENCODED_SECRET_PREFIX) else {
        return if value.is_empty() { None } else { Some(value) };
    };
    if hex.len() % 2 != 0 {
        return Some(value);
    }

    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().chunks_exact(2) {
        let Ok(pair) = std::str::from_utf8(pair) else {
            return Some(value);
        };
        let Ok(byte) = u8::from_str_radix(pair, 16) else {
            return Some(value);
        };
        bytes.push(byte);
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod secret_encoding_tests {
    use super::{decode_secret, encode_secret};

    #[test]
    fn groq_secret_is_hex_encoded_and_decoded() {
        let encoded = encode_secret("gsk_live_test");

        assert_ne!(encoded, "gsk_live_test");
        assert!(encoded.starts_with("hex:"));
        assert_eq!(
            decode_secret(Some(encoded)),
            Some("gsk_live_test".to_string())
        );
    }

    #[test]
    fn legacy_plain_groq_secret_still_reads() {
        assert_eq!(
            decode_secret(Some("gsk_legacy".to_string())),
            Some("gsk_legacy".to_string())
        );
    }

    #[test]
    fn empty_groq_secret_reads_as_missing() {
        assert_eq!(decode_secret(Some(String::new())), None);
    }
}

#[cfg(test)]
mod audible_status_sound_tests {
    use super::{get_preferences, migrate, set_preference};
    use rusqlite::Connection;

    fn memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn audible_status_sound_defaults_unset() {
        let conn = memory_db();
        let prefs = get_preferences(&conn).unwrap();

        assert_eq!(prefs.audible_status_sound, None);
    }

    #[test]
    fn audible_status_sound_accepts_and_canonicalizes_levels() {
        let conn = memory_db();

        set_preference(&conn, "audible_status_sound", "loud").unwrap();
        let prefs = get_preferences(&conn).unwrap();

        assert_eq!(prefs.audible_status_sound.as_deref(), Some("high"));
    }

    #[test]
    fn audible_status_sound_rejects_unknown_levels() {
        let conn = memory_db();

        let err = set_preference(&conn, "audible_status_sound", "silent").unwrap_err();

        assert!(err.to_string().contains("off, low, medium, high"));
    }
}
