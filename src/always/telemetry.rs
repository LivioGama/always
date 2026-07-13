//! Enterprise-grade logging infrastructure with tracing
//!
//! Provides structured logging with:
//! - Daily log rotation with 7-day retention
//! - JSON format to file for machine parsing
//! - Pretty format to stderr when running in foreground
//! - oslog integration on macOS for Console.app visibility
//! - Privacy controls for sensitive content (transcripts)

use anyhow::Result;
use std::path::PathBuf;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{
    EnvFilter, Layer,
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

/// Initialize logging infrastructure
///
/// Returns a WorkerGuard that must be kept alive for the duration of the program.
/// When the guard is dropped, all buffered logs are flushed.
pub fn init_logging(foreground: bool) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    // Determine log directory based on platform
    let log_dir = get_log_directory();

    // Create log directory if it doesn't exist
    std::fs::create_dir_all(&log_dir)?;

    // Set up rolling file appender with daily rotation
    let file_appender = RollingFileAppender::new(Rotation::DAILY, log_dir, "always");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // Build the subscriber
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("always=info,warn"));

    // JSON format to file
    let file_layer = fmt::layer()
        .json()
        .with_file(true)
        .with_line_number(true)
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(non_blocking)
        .with_filter(env_filter.clone());

    // Only add stderr layer in foreground mode
    if foreground {
        let stderr_layer = fmt::layer()
            .pretty()
            .with_thread_ids(false)
            .with_file(false)
            .with_line_number(false)
            .with_filter(env_filter);

        tracing_subscriber::registry()
            .with(file_layer)
            .with(stderr_layer)
            .init();
    } else {
        tracing_subscriber::registry().with(file_layer).init();
    }

    // Add oslog layer on macOS
    #[cfg(all(target_os = "macos", feature = "macos"))]
    {
        init_oslog();
    }

    Ok(guard)
}

/// Initialize macOS oslog integration
#[cfg(all(target_os = "macos", feature = "macos"))]
fn init_oslog() {
    use oslog::OsLogger;

    // Ignore "already initialized" — tracing_subscriber owns the log crate
    // global logger, so this always fails for short-lived CLI invocations.
    // Not an error; oslog Console.app visibility is best-effort.
    let _ = OsLogger::new("com.always.daemon").init();
}

/// Get the platform-specific log directory.
///
/// Falls back to `std::env::temp_dir()/always` if the platform's home or
/// config directory cannot be resolved. Logging is best-effort and must
/// never panic the daemon.
pub fn get_log_directory() -> PathBuf {
    fn fallback() -> PathBuf {
        tracing::warn!("home/config directory not resolvable; logs will go to a temp directory");
        std::env::temp_dir().join("always")
    }

    #[cfg(target_os = "macos")]
    {
        dirs::home_dir()
            .map(|h| h.join("Library/Logs/Always"))
            .unwrap_or_else(fallback)
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(state_home) = std::env::var("XDG_STATE_HOME") {
            PathBuf::from(state_home).join("always")
        } else {
            dirs::home_dir()
                .map(|h| h.join(".local/state/always"))
                .unwrap_or_else(fallback)
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
            PathBuf::from(local_appdata).join("Always/Logs")
        } else {
            dirs::home_dir()
                .map(|h| h.join("AppData/Local/Always/Logs"))
                .unwrap_or_else(fallback)
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        dirs::config_dir()
            .map(|c| c.join("always"))
            .unwrap_or_else(fallback)
    }
}

/// Check if transcript logging is enabled.
/// Debug builds: on by default for dev visibility. Release builds: off (privacy);
/// opt-in via `ALWAYS_LOG_TRANSCRIPTS=1`. Setting `=0`/`=false` forces off in any build.
pub fn should_log_transcripts() -> bool {
    match std::env::var("ALWAYS_LOG_TRANSCRIPTS") {
        Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") => true,
        Ok(v) if v == "0" || v.eq_ignore_ascii_case("false") => false,
        _ => cfg!(debug_assertions),
    }
}
