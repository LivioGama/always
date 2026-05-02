//! SQLite database — preferences for STT-only always.
//!
//! All state is persisted in `~/.config/always/always.db` with WAL mode enabled.
//! Schema is auto-migrated on first open.

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::config;

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

    Ok(())
}

// --- Preferences ---

pub fn get_preferences(conn: &Connection) -> Result<Preferences> {
    let mut stmt = conn.prepare(
        "SELECT lang, stt_threshold, stt_energy_threshold, stt_cooldown_ms, always_log_path, hear_energy_threshold, stt_silence, stt_trim_silence, stt_auto_enter, deepgram_api_key, groq_api_key, deepgram_model, silero_threshold FROM preferences WHERE id = 1",
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
            groq_api_key: row.get(10)?,
            deepgram_model: row.get(11)?,
            silero_threshold: row.get(12)?,
        })
    });
    match result {
        Ok(prefs) => Ok(prefs),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Preferences::default()),
        Err(e) => Err(e.into()),
    }
}

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
            if !(0.2..=15.0).contains(&parsed) {
                anyhow::bail!("stt_silence must be between 0.2 and 15.0 seconds");
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
        "stt_trim_silence" | "stt_auto_enter" => {
            if !matches!(value, "true" | "false" | "1" | "0") {
                anyhow::bail!("{key} must be one of: true, false, 1, 0");
            }
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
        "stt_trim_silence" | "stt_auto_enter" => {
            if matches!(value, "true" | "1") {
                "1"
            } else {
                "0"
            }
        }
        _ => value,
    };
    conn.execute(&sql, [normalized])?;
    Ok(())
}

pub fn reset_preferences(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM preferences WHERE id = 1", [])?;
    Ok(())
}

pub fn get_silero_threshold(conn: &Connection) -> Result<Option<f64>> {
    let prefs = get_preferences(conn)?;
    Ok(prefs.silero_threshold)
}

pub fn set_silero_threshold(conn: &Connection, value: f64) -> Result<()> {
    set_preference(conn, "silero_threshold", &value.to_string())
}
