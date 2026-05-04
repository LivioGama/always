use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::json;
use uuid::Uuid;

use always::always::correction::{self, CaptureOutcome};
use always::always::correction_queue::{PendingCorrection, global_queue};
use always::db;

mod cli;
use cli::LogsCommand;

/// Full version string: semver + git short SHA stamped at build time.
/// Read by `--version` and emitted in the daemon-start tracing event so
/// the Mac app can detect daemon/app revision drift.
const FULL_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("ALWAYS_BUILD_SHA"),
    ")"
);

#[derive(Parser)]
#[command(
    name = "always",
    version = FULL_VERSION,
    about = "Always-on voice activation daemon — Groq STT with intelligent transcription"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start always-on daemon in background
    Start {
        /// Language code for transcription
        #[arg(short = 'l', long, default_value = "en")]
        lang: String,
        /// Maximum recording duration per phrase in seconds
        #[arg(short = 't', long, default_value = "30")]
        timeout: u32,
        /// Seconds of silence before considering phrase complete
        #[arg(short = 's', long, default_value = "2.0")]
        silence: f64,
        /// Press Enter automatically after pasting transcript
        #[arg(long, default_value_t = false)]
        auto_enter: bool,
    },
    /// Stop always-on daemon
    Stop,
    /// Show always-on daemon status
    Status,
    /// Get current daemon state (pause, auto-enter, etc.)
    GetState,
    /// Run always-on in foreground (for debugging)
    #[command(name = "run")]
    RunForeground {
        /// Language code for transcription
        #[arg(short = 'l', long, default_value = "en")]
        lang: String,
        /// Maximum recording duration per phrase in seconds
        #[arg(short = 't', long, default_value = "30")]
        timeout: u32,
        /// Seconds of silence before considering phrase complete
        #[arg(short = 's', long, default_value = "2.0")]
        silence: f64,
        /// Press Enter automatically after pasting transcript
        #[arg(long, default_value_t = false)]
        auto_enter: bool,
    },
    /// Manage preferences
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Manage vocabulary and context-aware corrections
    Vocab {
        #[command(subcommand)]
        action: VocabAction,
    },
    /// Review/apply manual corrections (list, approve, reject, clear, capture)
    Corrections {
        #[command(subcommand)]
        action: CorrectionsAction,
    },
    /// View and manage Always logs
    Logs {
        #[command(flatten)]
        args: LogsCommand,
    },
    TogglePause,
    /// Toggle auto-enter state
    ToggleAutoEnter,
}

#[derive(Subcommand)]
enum CorrectionsAction {
    /// List pending corrections awaiting review.
    List,
    /// Approve a queued correction by ID — applies the (wrong → right)
    /// pair to ~/.always/glossary.json and removes it from the queue.
    Approve {
        /// Pending-correction UUID (see `corrections list`).
        id: String,
    },
    /// Drop a queued correction without applying.
    Reject {
        /// Pending-correction UUID (see `corrections list`).
        id: String,
    },
    /// Drop every entry in the queue.
    Clear,
    /// Manually trigger capture-from-selection (same flow as the
    /// ⌃⌥X hotkey). Reads the user's current text selection via Cmd+C
    /// and diffs against the most recently pasted transcript.
    Capture,
}

#[derive(Subcommand)]
enum VocabAction {
    /// Extract vocabulary from current project
    Extract {
        /// Project root directory (default: current directory)
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Import vocabulary from installed speech-to-text software
    Import,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Show current preferences
    Show,
    /// Set a preference
    Set {
        /// Preference key
        key: String,
        /// Preference value
        value: String,
    },
    /// Delete an API key from keychain
    DeleteKey {
        /// Key name (groq_api_key or deepgram_api_key)
        key: String,
    },
    /// Reset all preferences to defaults
    Reset,
    /// Apply a Mic Sensitivity preset (writes stt_energy_threshold +
    /// hear_energy_threshold to the underlying preferences). The same
    /// values are written when the GUI preset picker is used.
    Preset {
        /// One of: low, normal (alias: medium), high.
        level: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize structured logging (writes to ~/Library/Logs/Always/ on macOS).
    // Foreground only for `Run` (the explicit foreground/debug subcommand).
    let foreground = matches!(cli.command, Some(Commands::RunForeground { .. }));
    let _logging_guard = always::always::telemetry::init_logging(foreground)
        .map_err(|e| anyhow::anyhow!("Failed to initialize logging: {}", e))?;

    // One-shot migration: move any plaintext API keys from SQLite to the OS Keychain.
    // Non-fatal — if Keychain access fails (e.g., headless Linux without secret-service),
    // existing config flow still works.
    if let Err(e) = always::always::keyring::migrate_keys_from_db() {
        tracing::warn!(error = %e, "key migration to keychain failed");
    }

    match cli.command {
        Some(Commands::Start {
            lang,
            timeout,
            silence,
            auto_enter,
        }) => always::always::daemon::start(&always_config(lang, timeout, silence, auto_enter)?),
        Some(Commands::Stop) => always::always::daemon::stop(),
        Some(Commands::Status) => always::always::daemon::status(),
        Some(Commands::GetState) => {
            handle_get_state();
            Ok(())
        }
        Some(Commands::RunForeground {
            lang,
            timeout,
            silence,
            auto_enter,
        }) => always::always::run(&always_config(lang, timeout, silence, auto_enter)?),
        Some(Commands::Config { action }) => handle_config(action),
        Some(Commands::Vocab { action }) => handle_vocab(action),
        Some(Commands::Corrections { action }) => handle_corrections(action),
        Some(Commands::TogglePause) => handle_toggle_pause(),
        Some(Commands::ToggleAutoEnter) => handle_toggle_auto_enter(),
        Some(Commands::Logs { args }) => cli::handle_logs(args),
        None => {
            eprintln!("always: always-on voice activation daemon");
            eprintln!("Usage: always <COMMAND>");
            eprintln!();
            eprintln!("Commands:");
            eprintln!("  start             Start always-on daemon in background");
            eprintln!("  stop              Stop always-on daemon");
            eprintln!("  status            Show always-on daemon status");
            eprintln!("  run               Run always-on in foreground (for debugging)");
            eprintln!("  config            Manage preferences");
            eprintln!(
                "  corrections       Review/apply manual corrections (list, approve, reject, clear, capture)"
            );
            eprintln!("  vocab             Manage vocabulary and corrections");
            eprintln!("  toggle-pause      Toggle pause/resume state");
            eprintln!("  toggle-auto-enter Toggle auto-enter state");
            eprintln!("  logs              View and manage Always logs");
            eprintln!();
            eprintln!("Use 'always <COMMAND> --help' for more information on a command.");
            Ok(())
        }
    }
}

fn handle_config(action: ConfigAction) -> Result<()> {
    let conn = db::open()?;

    match action {
        ConfigAction::Show => {
            let prefs = db::get_preferences(&conn)?;

            // Check keychain for API keys
            let groq_in_keychain = always::always::keyring::get_groq_api_key()
                .unwrap_or(None)
                .is_some();
            let deepgram_in_keychain = always::always::keyring::get_deepgram_api_key()
                .unwrap_or(None)
                .is_some();

            println!(
                "deepgram_api_key: {}",
                if deepgram_in_keychain {
                    "*** (in keychain)".to_string()
                } else if prefs.deepgram_api_key.is_some() {
                    "*** (in database)".to_string()
                } else {
                    "(not set)".to_string()
                }
            );
            println!(
                "stt_energy_threshold: {}",
                prefs
                    .stt_energy_threshold
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "0.05".to_string())
            );
            println!(
                "hear_energy_threshold: {}",
                prefs
                    .hear_energy_threshold
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "0.002".to_string())
            );
            println!(
                "stt_cooldown_ms: {}",
                prefs
                    .stt_cooldown_ms
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "1500".to_string())
            );
            println!(
                "always_log_path: {}",
                prefs.always_log_path.as_deref().unwrap_or("(default)")
            );
            println!(
                "stt_silence: {}",
                prefs
                    .stt_silence
                    .map(|v| format!("{v}s"))
                    .unwrap_or_else(|| "2.0s".to_string())
            );
            println!(
                "stt_auto_enter: {}",
                prefs
                    .stt_auto_enter
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "false".to_string())
            );
            println!(
                "groq_api_key: {}",
                if groq_in_keychain {
                    "*** (in keychain)".to_string()
                } else if prefs.groq_api_key.is_some() {
                    "*** (in database)".to_string()
                } else {
                    "(not set)".to_string()
                }
            );
            println!(
                "silero_threshold: {}",
                prefs
                    .silero_threshold
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "0.5".to_string())
            );
            println!(
                "shortcut_pause: {}",
                prefs.shortcut_pause.as_deref().unwrap_or("ctrl+alt+p")
            );
            println!(
                "shortcut_auto_enter: {}",
                prefs.shortcut_auto_enter.as_deref().unwrap_or("ctrl+alt+a")
            );
            println!(
                "shortcut_force_paste: {}",
                prefs
                    .shortcut_force_paste
                    .as_deref()
                    .unwrap_or("ctrl+alt+v")
            );
            println!(
                "shortcut_log_correction: {}",
                prefs
                    .shortcut_log_correction
                    .as_deref()
                    .unwrap_or("ctrl+alt+x")
            );
            println!(
                "passive_correction_capture: {}",
                prefs
                    .passive_correction_capture
                    .map(|v| if v { "true" } else { "false" })
                    .unwrap_or("false")
            );
            println!(
                "postprocess_enabled: {}",
                prefs
                    .postprocess_enabled
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "true".to_string())
            );
        }
        ConfigAction::Set { key, value } => {
            // Handle API keys specially - store in keychain
            if key == "groq_api_key" {
                always::always::keyring::set_groq_api_key(&value)?;
                println!("groq_api_key = *** (stored in keychain)");
            } else if key == "deepgram_api_key" {
                always::always::keyring::set_deepgram_api_key(&value)?;
                println!("deepgram_api_key = *** (stored in keychain)");
            } else {
                db::set_preference(&conn, &key, &value)?;
                println!("{key} = {value}");
            }
        }
        ConfigAction::DeleteKey { key } => match key.as_str() {
            "groq_api_key" => {
                always::always::keyring::delete_groq_api_key()?;
                println!("groq_api_key deleted from keychain");
            }
            "deepgram_api_key" => {
                always::always::keyring::delete_deepgram_api_key()?;
                println!("deepgram_api_key deleted from keychain");
            }
            _ => {
                eprintln!("Unknown key: {key}. Valid keys: groq_api_key, deepgram_api_key");
                std::process::exit(1);
            }
        },
        ConfigAction::Reset => {
            db::reset_preferences(&conn)?;
            println!("Preferences reset to defaults.");
        }
        ConfigAction::Preset { level } => {
            use std::str::FromStr;
            let preset = always::always::config::SensitivityPreset::from_str(&level)?;
            let (stt, hear) = preset.thresholds();
            db::set_preference(&conn, "stt_energy_threshold", &stt.to_string())?;
            db::set_preference(&conn, "hear_energy_threshold", &hear.to_string())?;
            println!(
                "Sensitivity preset = {preset}\n  stt_energy_threshold = {stt}\n  hear_energy_threshold = {hear}"
            );
        }
    }
    Ok(())
}

fn handle_vocab(action: VocabAction) -> Result<()> {
    match action {
        VocabAction::Extract { path } => {
            let project_root = path.unwrap_or_else(|| ".".to_string());
            println!("Vocabulary extraction not yet implemented for: {project_root}");
        }
        VocabAction::Import => {
            println!("Scanning for installed speech-to-text software...");
            // Legacy detection (Dragon only) — plugins run independently
            // inside `import_vocabulary` regardless.
            let detected = always::always::vocab::detect_stt_software();
            if !detected.is_empty() {
                println!("Detected legacy software: {}", detected.join(", "));
            }
            // Enumerate plugin sources (real per-app extractors).
            let plugins = always::always::vocab::plugins::get_all_plugins();
            let active: Vec<&str> = plugins
                .iter()
                .filter(|p| p.is_installed())
                .map(|p| p.name())
                .collect();
            if active.is_empty() {
                println!("No vocabulary plugins active on this system.");
            } else {
                println!("Active plugins: {}", active.join(", "));
            }
            println!("Importing vocabulary...");
            let imported = always::always::vocab::import_vocabulary(&detected)?;
            println!(
                "Scanned {} unique terms. Merged into ~/.always/glossary.json (existing entries preserved).",
                imported.len()
            );
        }
    }
    Ok(())
}

fn handle_corrections(action: CorrectionsAction) -> Result<()> {
    // The queue is a singleton mirrored to ~/.always/pending_corrections.json,
    // so every CLI invocation sees the same state the daemon does.
    let queue = global_queue()?;

    match action {
        CorrectionsAction::List => {
            let mut entries = queue.list();
            if entries.is_empty() {
                println!("No pending corrections.");
                return Ok(());
            }

            // Stable display order: oldest first so users review FIFO.
            entries.sort_by_key(|e| e.queued_at_unix_ms);

            // Full UUID is printed (no truncation) so the value can be
            // copy-pasted into `approve`/`reject` without ambiguity.
            // Header row — width specifiers keep columns aligned with
            // the data rows below.
            let header = format!(
                "{:<38} {:<10} {:<14} Wrong → Right",
                "ID", "Source", "Queued"
            );
            println!("{header}");
            for entry in &entries {
                println!(
                    "{:<38} {:<10} {:<14} {} → {}",
                    entry.id,
                    source_label(entry),
                    relative_queued(entry.queued_at_unix_ms),
                    entry.pair.wrong,
                    entry.pair.right,
                );
            }
        }
        CorrectionsAction::Approve { id } => {
            // Validate the UUID before touching the queue so a typo can't
            // be silently mistaken for "not found".
            let uuid = Uuid::parse_str(&id)
                .map_err(|e| anyhow::anyhow!("invalid correction id '{id}': {e}"))?;
            let Some(entry) = queue.take(uuid) else {
                // Exit 1 on unknown ID so shell scripts can detect a stale
                // reference (e.g. ID already approved/rejected concurrently).
                eprintln!("unknown correction id: {id}");
                std::process::exit(1);
            };
            correction::apply_pairs_to_glossary(std::slice::from_ref(&entry.pair))?;
            println!("Applied: {} → {}", entry.pair.wrong, entry.pair.right);
        }
        CorrectionsAction::Reject { id } => {
            let uuid = Uuid::parse_str(&id)
                .map_err(|e| anyhow::anyhow!("invalid correction id '{id}': {e}"))?;
            let Some(entry) = queue.take(uuid) else {
                eprintln!("unknown correction id: {id}");
                std::process::exit(1);
            };
            println!("Dropped: {} → {}", entry.pair.wrong, entry.pair.right);
        }
        CorrectionsAction::Clear => {
            // Print the count *before* clearing so users see what they
            // just discarded — `clear` is destructive and irreversible.
            let n = queue.len();
            println!("Removing {n} pending correction(s).");
            queue.clear()?;
            println!("Cleared.");
        }
        CorrectionsAction::Capture => {
            // 60s window matches the default hotkey behaviour: a paste
            // older than that is almost certainly unrelated to whatever
            // the user has selected now.
            match correction::capture_via_hotkey(Duration::from_secs(60))? {
                CaptureOutcome::NoRecentPaste => {
                    eprintln!("No recent paste to diff against.");
                    std::process::exit(1);
                }
                CaptureOutcome::NoChange => {
                    println!("Selection matches the last paste — nothing to record.");
                }
                CaptureOutcome::NoCorrectionPairs => {
                    println!("Selection differs but no word pairs cleared the similarity gate.");
                }
                CaptureOutcome::Applied { pairs, applied } => {
                    for pair in &pairs {
                        println!("Recorded: {} → {}", pair.wrong, pair.right);
                    }
                    println!("Wrote {applied} new mistranscription(s) to ~/.always/glossary.json");
                }
            }
        }
    }

    Ok(())
}

fn source_label(entry: &PendingCorrection) -> &'static str {
    use always::always::correction_queue::CorrectionSource;
    match entry.source {
        CorrectionSource::Hotkey => "hotkey",
        CorrectionSource::Passive => "passive",
    }
}

/// Format a unix-millis timestamp as "N units ago" relative to now.
/// Uses humantime over a `Duration` rounded to seconds so the output
/// is stable ("2min ago", "5s ago"); humantime emits a verbose
/// multi-unit form, so we pull just the leading component for a
/// compact list display.
fn relative_queued(queued_at_unix_ms: u64) -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if queued_at_unix_ms == 0 || queued_at_unix_ms > now_ms {
        return "just now".to_string();
    }
    let secs = (now_ms - queued_at_unix_ms) / 1000;
    if secs == 0 {
        return "just now".to_string();
    }
    let formatted = humantime::format_duration(Duration::from_secs(secs)).to_string();
    // humantime returns e.g. "2m 13s 400ms" — keep only the most
    // significant chunk so the table column stays narrow.
    let head = formatted.split_whitespace().next().unwrap_or(&formatted);
    format!("{head} ago")
}

fn handle_toggle_pause() -> Result<()> {
    let new_state = always::always::pause::toggle_pause();

    // Broadcast event via UDS (Swift app receives this and shows overlay)
    if new_state {
        always::always::event::global_broadcaster().paused();
    } else {
        always::always::event::global_broadcaster().resumed();
    }

    println!(
        "Pause state: {}",
        if new_state { "paused" } else { "resumed" }
    );
    Ok(())
}

fn handle_toggle_auto_enter() -> Result<()> {
    let new_state = always::always::pause::toggle_auto_enter();

    // Broadcast event via UDS (Swift app receives this and shows overlay)
    if new_state {
        always::always::event::global_broadcaster().auto_enter_enabled();
    } else {
        always::always::event::global_broadcaster().auto_enter_disabled();
    }

    println!(
        "Auto-enter state: {}",
        if new_state { "enabled" } else { "disabled" }
    );
    Ok(())
}

fn handle_get_state() {
    let is_paused = always::always::pause::is_paused();
    let is_auto_enter = always::always::pause::is_auto_enter_enabled();
    let state = json!({
        "isPaused": is_paused,
        "isAutoEnter": is_auto_enter
    });
    println!("{}", state);
}

fn always_config(
    lang: String,
    timeout: u32,
    silence: f64,
    auto_enter: bool,
) -> Result<always::always::AlwaysConfig> {
    always::always::AlwaysConfig::from_cli(lang, timeout, silence, auto_enter)
}
