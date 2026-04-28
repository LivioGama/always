//! Configuration paths and platform defaults for STT-only always.

use std::path::PathBuf;

pub const SUPPORTED_LANGS: &[&str] = &[
    "en", "fr", "es", "de", "it", "pt", "zh", "ja", "ko", "ru", "ar", "nl",
];

pub fn config_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ALWAYS_CONFIG_DIR") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("always")
}

pub fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("ALWAYS_DB_PATH") {
        return PathBuf::from(p);
    }
    config_dir().join("always.db")
}
