use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use uuid::Uuid;

use always::always::correction::{self, CaptureOutcome};
use always::always::correction_queue::{PendingCorrection, global_queue};
use always::db;

mod cli;
use cli::LogsCommand;

#[cfg(feature = "overlay")]
mod overlay_integration;

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
        /// Language code for transcription (ISO 639-1, or "auto" to let the engine detect)
        #[arg(short = 'l', long, default_value = "auto")]
        lang: String,
        /// Maximum recording duration per phrase in seconds
        #[arg(short = 't', long, default_value = "30")]
        timeout: u32,
        /// Seconds of silence before considering phrase complete.
        /// Omit to use the saved preference/canonical default.
        #[arg(short = 's', long)]
        silence: Option<f64>,
        /// Press Enter automatically after pasting transcript. Omitting
        /// the flag (the default) reads `stt_auto_enter` from the prefs
        /// table — so the GUI toggle is the source of truth. Pass
        /// `--auto-enter` or `--no-auto-enter` to override explicitly.
        #[arg(long, num_args = 0..=1, default_missing_value = "true", action = clap::ArgAction::Set)]
        auto_enter: Option<bool>,
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
        /// Language code for transcription (ISO 639-1, or "auto" to let the engine detect)
        #[arg(short = 'l', long, default_value = "auto")]
        lang: String,
        /// Maximum recording duration per phrase in seconds
        #[arg(short = 't', long, default_value = "30")]
        timeout: u32,
        /// Seconds of silence before considering phrase complete.
        /// Omit to use the saved preference/canonical default.
        #[arg(short = 's', long)]
        silence: Option<f64>,
        /// Press Enter automatically after pasting transcript. See
        /// `Start::auto_enter` — same opt-in semantics; DB pref wins
        /// when the flag is omitted.
        #[arg(long, num_args = 0..=1, default_missing_value = "true", action = clap::ArgAction::Set)]
        auto_enter: Option<bool>,
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
    /// "My Voice" speaker enrollment (record, status, enable, disable, clear)
    Voice {
        #[command(subcommand)]
        action: VoiceAction,
    },
    /// View and manage Always logs
    Logs {
        #[command(flatten)]
        args: LogsCommand,
    },
    /// Toggle pause/resume state
    TogglePause,
    /// Toggle auto-enter state
    ToggleAutoEnter,
    /// Reset macOS menu bar / Control Center cache for Always
    MenuBar {
        #[command(subcommand)]
        action: MenuBarAction,
    },
    /// Run overlay companion process (Linux)
    #[cfg(feature = "overlay")]
    Overlay {
        #[command(subcommand)]
        action: OverlayAction,
    },
}

#[derive(clap::Subcommand)]
enum MenuBarAction {
    /// Delete Control Center displayablemenuextras registry and stale NSStatusItem keys
    Reset,
}

#[cfg(feature = "overlay")]
#[derive(clap::Subcommand)]
enum OverlayAction {
    /// Run overlay companion process
    Run,
}

#[derive(clap::Subcommand)]
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
enum VoiceAction {
    /// Record one guided enrollment sample via the running daemon.
    /// Speak until the daemon reports the sample captured (~5s voice).
    Record {
        /// Which sample: normal, lower, or louder
        step: String,
    },
    /// Cancel an in-flight enrollment recording
    Cancel,
    /// Show enrollment + gate status from the local profile
    Status,
    /// Enable the "only listen to my voice" gate (requires enrollment)
    Enable,
    /// Disable the gate (voiceprint is kept)
    Disable,
    /// Delete the enrolled voiceprint
    Clear,
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
    /// Delete a saved API key
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

    match cli.command {
        Some(Commands::Start {
            lang,
            timeout,
            silence,
            auto_enter,
        }) => always::always::daemon::start(&always_config(lang, timeout, silence, auto_enter)?),
        Some(Commands::Stop) => always::always::daemon::stop(),
        Some(Commands::Status) => always::always::daemon::status(),
        Some(Commands::GetState) => handle_get_state(),
        Some(Commands::RunForeground {
            lang,
            timeout,
            silence,
            auto_enter,
        }) => always::always::run(&always_config(lang, timeout, silence, auto_enter)?),
        Some(Commands::Config { action }) => handle_config(action),
        Some(Commands::Vocab { action }) => handle_vocab(action),
        Some(Commands::Corrections { action }) => handle_corrections(action),
        Some(Commands::Voice { action }) => handle_voice(action),
        Some(Commands::TogglePause) => handle_toggle_pause(),
        Some(Commands::ToggleAutoEnter) => handle_toggle_auto_enter(),
        Some(Commands::Logs { args }) => cli::handle_logs(args),
        Some(Commands::MenuBar { action }) => match action {
            MenuBarAction::Reset => cli::menu_bar::reset(),
        },
        #[cfg(feature = "overlay")]
        Some(Commands::Overlay { action }) => match action {
            OverlayAction::Run => overlay_integration::run(),
        },
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
            eprintln!("  menu-bar reset    Clear macOS menu bar cache for Always");
            #[cfg(feature = "overlay")]
            eprintln!("  overlay run       Run overlay companion process (Linux)");
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

            println!("lang: {}", prefs.lang.as_deref().unwrap_or("auto"));
            println!(
                "deepgram_api_key: {}",
                if prefs.deepgram_api_key.is_some() {
                    "*** (in database)".to_string()
                } else {
                    "(not set)".to_string()
                }
            );
            // Defaults below MUST match `AlwaysConfig::default` / the
            // GUI `Config.defaultConfig` / the Normal sensitivity preset.
            // Any drift here silently shows the user the wrong value in
            // `always config show` when the DB column is NULL.
            println!(
                "stt_energy_threshold: {}",
                prefs
                    .stt_energy_threshold
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "0.012".to_string())
            );
            println!(
                "hear_energy_threshold: {}",
                prefs
                    .hear_energy_threshold
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "0.001".to_string())
            );
            // Print the canonical `stt_cooldown_ms` key + value so the
            // CLI ↔ Swift contract is unit-consistent. The previous
            // output emitted `stt_cooldown_secs` with the value divided
            // by 1000, which (a) Swift wasn't parsing, and (b) made
            // CLI-only debugging misleading because the column is
            // stored in milliseconds.
            println!(
                "stt_cooldown_ms: {}",
                prefs
                    .stt_cooldown_ms
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "150".to_string())
            );
            println!(
                "always_log_path: {}",
                prefs.always_log_path.as_deref().unwrap_or("(default)")
            );
            println!(
                "stt_silence_secs: {}",
                prefs
                    .stt_silence
                    .map(|v| format!("{v}"))
                    .unwrap_or_else(|| always::always::config::DEFAULT_SILENCE_SECS.to_string())
            );
            println!(
                "stt_adaptive_silence: {}",
                prefs
                    .stt_adaptive_silence
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "true".to_string())
            );
            println!(
                "speaker_gate_enabled: {}",
                prefs
                    .speaker_gate_enabled
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "false".to_string())
            );
            println!(
                "speaker_gate_threshold: {}",
                prefs
                    .speaker_gate_threshold
                    .map(|v| format!("{v}"))
                    .unwrap_or_else(
                        || always::always::config::DEFAULT_SPEAKER_GATE_THRESHOLD.to_string()
                    )
            );
            println!(
                "stt_auto_enter: {}",
                prefs
                    .stt_auto_enter
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "false".to_string())
            );
            println!(
                "auto_enter_delay_ms: {}",
                prefs
                    .auto_enter_delay_ms
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "4000".to_string())
            );
            println!(
                "groq_api_key: {}",
                if prefs.groq_api_key.is_some() {
                    "*** (saved)".to_string()
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
                "shortcut_correction_dialog: {}",
                prefs
                    .shortcut_correction_dialog
                    .as_deref()
                    .unwrap_or("ctrl+alt+w")
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
            println!(
                "per_app_settings_json: {}",
                prefs.per_app_settings_json.as_deref().unwrap_or("{}")
            );
            println!(
                "idle_pause_secs: {}",
                prefs
                    .idle_pause_secs
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "120".to_string())
            );
            println!(
                "idle_pause_action: {}",
                prefs.idle_pause_action.as_deref().unwrap_or("pause")
            );
            println!(
                "transcript_stream: {}",
                prefs
                    .transcript_stream
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "false".to_string())
            );
            println!(
                "audible_status_sound: {}",
                prefs.audible_status_sound.as_deref().unwrap_or("off")
            );
        }
        ConfigAction::Set { key, value } => {
            // Store API keys in the preferences DB. Keychain access prompts
            // are unacceptable in local rebuilds because debug signing changes
            // the code requirement often enough for macOS to re-authorize.
            if key == "groq_api_key" {
                db::set_preference(&conn, &key, &value)?;
                println!("groq_api_key = *** (saved)");
            } else if key == "deepgram_api_key" {
                db::set_preference(&conn, &key, &value)?;
                println!("deepgram_api_key = *** (saved)");
            } else {
                db::set_preference(&conn, &key, &value)?;
                println!("{key} = {value}");
            }
        }
        ConfigAction::DeleteKey { key } => match key.as_str() {
            "groq_api_key" => {
                db::set_preference(&conn, "groq_api_key", "")?;
                println!("groq_api_key deleted from saved settings");
            }
            "deepgram_api_key" => {
                db::set_preference(&conn, "deepgram_api_key", "")?;
                println!("deepgram_api_key deleted from saved settings");
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
    // The pause state lives in process-local atomics inside the running
    // daemon. A fresh `always toggle-pause` invocation has its own
    // empty atomics, so flipping them locally was a no-op. Send the
    // toggle through the UDS socket so the actual daemon process
    // receives it.
    if always::always::daemon::is_running() {
        send_uds_command(r#"{"type":"TogglePause"}"#)?;
        println!("Sent TogglePause to running daemon.");
        return Ok(());
    }
    // No daemon: keep the legacy local toggle so downstream scripts
    // that grep for "Pause state:" don't break. `toggle_pause` now
    // returns `(effective, changed)` — we print the effective value
    // since that's what the user cares about.
    let (effective, _changed) = always::always::pause::toggle_pause();
    println!(
        "Pause state: {} (no daemon running; toggle was local-only)",
        if effective { "paused" } else { "resumed" }
    );
    Ok(())
}

fn handle_toggle_auto_enter() -> Result<()> {
    // Same rationale as `handle_toggle_pause`: dial the running daemon
    // when one exists so the toggle reaches the right process-local
    // atomic.
    if always::always::daemon::is_running() {
        send_uds_command(r#"{"type":"ToggleAutoEnter"}"#)?;
        println!("Sent ToggleAutoEnter to running daemon.");
        return Ok(());
    }
    let new_state = always::always::pause::toggle_auto_enter();
    println!(
        "Auto-enter state: {} (no daemon running; toggle was local-only)",
        if new_state { "enabled" } else { "disabled" }
    );
    Ok(())
}

/// Send a single JSON line to the daemon's UDS socket. Caller is
/// responsible for terminating with `\n`-or-not; we append one.
/// Times out after 2 s so a stuck daemon doesn't hang the CLI.
fn handle_voice(action: VoiceAction) -> Result<()> {
    use always::always::voiceprint;
    match action {
        VoiceAction::Record { step } => {
            let Some(parsed) = voiceprint::EnrollStep::parse(&step) else {
                anyhow::bail!("unknown step '{step}' — expected: normal, lower, louder");
            };
            if !always::always::daemon::is_running() {
                anyhow::bail!("daemon is not running — start Always first");
            }
            send_uds_command(&format!(
                r#"{{"type":"StartVoiceEnrollment","data":{{"step":"{}"}}}}"#,
                parsed.as_str()
            ))?;
            println!(
                "Recording queued for step '{}'. Speak in that tone until the daemon \
                 captures ~5s of voice (watch `always logs --pretty` or the Settings tab).",
                parsed.as_str()
            );
            Ok(())
        }
        VoiceAction::Cancel => {
            send_uds_command(r#"{"type":"CancelVoiceEnrollment"}"#)?;
            println!("Enrollment cancel requested.");
            Ok(())
        }
        VoiceAction::Status => {
            // Read the profile from disk directly — status must work even
            // when the daemon is down.
            let conn = db::open()?;
            let prefs = db::get_preferences(&conn)?;
            let enabled = prefs.speaker_gate_enabled.unwrap_or(false);
            let threshold = prefs
                .speaker_gate_threshold
                .unwrap_or(always::always::config::DEFAULT_SPEAKER_GATE_THRESHOLD);
            match voiceprint::current() {
                Some(p) => {
                    println!("enrolled: {}", p.is_complete());
                    println!("steps_recorded: {}", p.recorded_steps().join(", "));
                    println!("model: {}", p.model);
                    println!("updated_at: {}", p.updated_at);
                }
                None => {
                    println!("enrolled: false");
                    println!("steps_recorded: (none)");
                }
            }
            println!("gate_enabled: {enabled}");
            println!("gate_threshold: {threshold}");
            Ok(())
        }
        VoiceAction::Enable => {
            if !voiceprint::is_enrolled() {
                anyhow::bail!(
                    "no complete voiceprint enrolled — record all three steps first \
                     (always voice record normal|lower|louder)"
                );
            }
            let conn = db::open()?;
            db::set_preference(&conn, "speaker_gate_enabled", "true")?;
            // Hot-apply when the daemon is up; harmless if it isn't.
            let _ =
                send_uds_command(r#"{"type":"SetVoiceProfileEnabled","data":{"enabled":true}}"#);
            println!("My Voice gate ENABLED — Always now only listens to the enrolled voice.");
            Ok(())
        }
        VoiceAction::Disable => {
            let conn = db::open()?;
            db::set_preference(&conn, "speaker_gate_enabled", "false")?;
            let _ =
                send_uds_command(r#"{"type":"SetVoiceProfileEnabled","data":{"enabled":false}}"#);
            println!("My Voice gate disabled — Always listens to any voice again.");
            Ok(())
        }
        VoiceAction::Clear => {
            voiceprint::clear()?;
            let _ = send_uds_command(r#"{"type":"DeleteVoiceProfile"}"#);
            println!("Voiceprint deleted.");
            Ok(())
        }
    }
}

fn send_uds_command(json: &str) -> Result<()> {
    use anyhow::Context as _;
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::net::UnixStream;

    let Some(sock_path) = always::always::daemon::socket_path() else {
        anyhow::bail!("no UDS socket path available on this platform");
    };
    #[cfg(unix)]
    {
        use std::io::Read as _;
        let mut stream = UnixStream::connect(&sock_path)
            .with_context(|| format!("failed to connect to daemon at {}", sock_path.display()))?;
        stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
        writeln!(stream, "{json}").context("failed to send UDS command")?;
        // Drain the daemon's initial-state burst briefly instead of
        // closing immediately. The daemon writes that burst BEFORE it
        // starts reading commands — a client that hangs up right after
        // writing makes that burst write fail, and the daemon drops the
        // connection without ever reading the command we just sent.
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .ok();
        let mut sink = [0u8; 4096];
        let deadline = std::time::Instant::now() + Duration::from_millis(700);
        while std::time::Instant::now() < deadline {
            match stream.read(&mut sink) {
                Ok(0) | Err(_) => break, // EOF or timeout — command is in
                Ok(_) => continue,
            }
        }
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("UDS socket communication not supported on this platform");
    }
    Ok(())
}

fn handle_get_state() -> Result<()> {
    if always::always::daemon::is_running() {
        let state = read_daemon_state()?;
        println!("{}", state);
        return Ok(());
    }

    let is_paused = always::always::pause::is_paused();
    let is_auto_enter = always::always::pause::is_auto_enter_enabled();
    let state = json!({
        "isPaused": is_paused,
        "isAutoEnter": is_auto_enter
    });
    println!("{}", state);
    Ok(())
}

fn read_daemon_state() -> Result<Value> {
    use anyhow::Context as _;
    use std::io::{BufRead as _, BufReader};
    #[cfg(unix)]
    use std::os::unix::net::UnixStream;

    let Some(sock_path) = always::always::daemon::socket_path() else {
        anyhow::bail!("no UDS socket path available on this platform");
    };

    #[cfg(unix)]
    {
        let stream = UnixStream::connect(&sock_path)
            .with_context(|| format!("failed to connect to daemon at {}", sock_path.display()))?;
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();

        let mut reader = BufReader::new(stream);
        let mut is_paused = None;
        let mut is_auto_enter = None;

        for _ in 0..16 {
            let mut line = String::new();
            let bytes = reader
                .read_line(&mut line)
                .context("failed to read daemon state")?;
            if bytes == 0 {
                break;
            }

            update_state_from_daemon_event(&line, &mut is_paused, &mut is_auto_enter)?;
            if is_paused.is_some() && is_auto_enter.is_some() {
                return Ok(json!({
                    "isPaused": is_paused.unwrap_or(false),
                    "isAutoEnter": is_auto_enter.unwrap_or(false)
                }));
            }
        }

        anyhow::bail!("daemon did not send complete state");
    }

    #[cfg(not(unix))]
    {
        anyhow::bail!("daemon state query not supported on this platform");
    }
}

fn update_state_from_daemon_event(
    line: &str,
    is_paused: &mut Option<bool>,
    is_auto_enter: &mut Option<bool>,
) -> Result<()> {
    let event: Value = serde_json::from_str(line.trim())?;
    match event.get("type").and_then(Value::as_str) {
        Some("Paused" | "PausedQuietly") => *is_paused = Some(true),
        Some("Resumed" | "ResumedQuietly") => *is_paused = Some(false),
        Some("AutoEnterEnabled") => *is_auto_enter = Some(true),
        Some("AutoEnterDisabled") => *is_auto_enter = Some(false),
        _ => {}
    }
    Ok(())
}

fn always_config(
    lang: String,
    timeout: u32,
    silence: Option<f64>,
    auto_enter: Option<bool>,
) -> Result<always::always::AlwaysConfig> {
    always::always::AlwaysConfig::from_cli(lang, timeout, silence, auto_enter)
}

#[cfg(test)]
mod tests {
    use super::update_state_from_daemon_event;

    #[test]
    fn daemon_state_parser_tracks_pause_and_auto_enter() {
        let mut paused = None;
        let mut auto_enter = None;

        update_state_from_daemon_event(
            r#"{"type":"Hello","data":{"version":1}}"#,
            &mut paused,
            &mut auto_enter,
        )
        .unwrap();
        update_state_from_daemon_event(r#"{"type":"Resumed"}"#, &mut paused, &mut auto_enter)
            .unwrap();
        update_state_from_daemon_event(
            r#"{"type":"AutoEnterEnabled"}"#,
            &mut paused,
            &mut auto_enter,
        )
        .unwrap();

        assert_eq!(paused, Some(false));
        assert_eq!(auto_enter, Some(true));
    }

    #[test]
    fn daemon_state_parser_treats_quiet_pause_as_effective_pause() {
        let mut paused = None;
        let mut auto_enter = None;

        update_state_from_daemon_event(r#"{"type":"PausedQuietly"}"#, &mut paused, &mut auto_enter)
            .unwrap();
        update_state_from_daemon_event(
            r#"{"type":"AutoEnterDisabled"}"#,
            &mut paused,
            &mut auto_enter,
        )
        .unwrap();

        assert_eq!(paused, Some(true));
        assert_eq!(auto_enter, Some(false));
    }
}
