//! Configuration paths and platform defaults for STT-only always.

use std::path::PathBuf;
use std::sync::OnceLock;

pub const SUPPORTED_LANGS: &[&str] = &[
    "en", "fr", "es", "de", "it", "pt", "zh", "ja", "ko", "ru", "ar", "nl", "auto",
];

/// Runtime instance discriminator. "prod" is the shipped app; any other
/// value isolates config/db/pid/socket paths so a development identity can
/// run alongside the production install without touching it.
///
/// Detection order: `ALWAYS_INSTANCE` env var (CLI/dev runs), then the
/// executable's own path — a daemon bundled inside `Always Dev.app` is dev
/// no matter who launches it, so the GUI needs no env plumbing.
pub fn instance() -> &'static str {
    static INSTANCE: OnceLock<String> = OnceLock::new();
    INSTANCE
        .get_or_init(|| {
            if let Ok(v) = std::env::var("ALWAYS_INSTANCE") {
                let v = v.trim().to_lowercase();
                return if v.is_empty() || v == "prod" {
                    "prod".to_string()
                } else {
                    v
                };
            }
            if let Ok(exe) = std::env::current_exe() {
                if exe.to_string_lossy().contains("Always Dev.app") {
                    return "dev".to_string();
                }
            }
            "prod".to_string()
        })
        .as_str()
}

/// `"-dev"` for the dev instance, `""` for prod — the suffix applied to
/// every per-instance path (config dir, socket, pid file, lock file).
pub fn instance_suffix() -> String {
    match instance() {
        "prod" => String::new(),
        i => format!("-{i}"),
    }
}

pub fn config_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ALWAYS_CONFIG_DIR") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(format!("always{}", instance_suffix()))
}

pub fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("ALWAYS_DB_PATH") {
        return PathBuf::from(p);
    }
    config_dir().join("always.db")
}
