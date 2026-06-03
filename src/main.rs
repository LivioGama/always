use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::json;

use always::db;
use always::always::pause;

#[derive(Parser)]
#[command(name = "always", version, about = "Always-on voice activation daemon — Groq STT with intelligent transcription")]
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
        #[arg(short = 's', long, default_value = "0.4")]
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
        #[arg(short = 's', long, default_value = "0.4")]
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
    /// Toggle pause/resume state
    TogglePause,
    /// Toggle auto-enter state
    ToggleAutoEnter,
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
    /// Reset all preferences to defaults
    Reset,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Start {
            lang,
            timeout,
            silence,
            auto_enter,
        }) => always::always::daemon::start(&always_config(lang, timeout, silence, auto_enter)?),
        Some(Commands::Stop) => always::always::daemon::stop(),
        Some(Commands::Status) => always::always::daemon::status(),
        Some(Commands::GetState) => Ok(handle_get_state()),
        Some(Commands::RunForeground {
            lang,
            timeout,
            silence,
            auto_enter,
        }) => always::always::run(&always_config(lang, timeout, silence, auto_enter)?),
        Some(Commands::Config { action }) => handle_config(action),
        Some(Commands::Vocab { action }) => handle_vocab(action),
        Some(Commands::TogglePause) => handle_toggle_pause(),
        Some(Commands::ToggleAutoEnter) => handle_toggle_auto_enter(),
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
            eprintln!("  vocab             Manage vocabulary and corrections");
            eprintln!("  toggle-pause      Toggle pause/resume state");
            eprintln!("  toggle-auto-enter Toggle auto-enter state");
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
            println!(
                "deepgram_api_key: {}",
                prefs
                    .deepgram_api_key
                    .as_deref()
                    .map(|k| format!("{}...", &k[..k.len().min(12)]))
                    .unwrap_or("(not set)".to_string())
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
                prefs
                    .groq_api_key
                    .as_deref()
                    .map(|k| format!("{}...", &k[..k.len().min(12)]))
                    .unwrap_or_else(|| "(not set)".to_string())
            );
        }
        ConfigAction::Set { key, value } => {
            db::set_preference(&conn, &key, &value)?;
            println!("{key} = {value}");
        }
        ConfigAction::Reset => {
            db::reset_preferences(&conn)?;
            println!("Preferences reset to defaults.");
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
            let detected = always::always::vocab::detect_stt_software();
            if detected.is_empty() {
                println!("No speech-to-text software detected.");
            } else {
                println!("Detected: {}", detected.join(", "));
                println!("Importing vocabulary...");
                let imported = always::always::vocab::import_vocabulary(&detected)?;
                println!("Imported {} vocabulary terms.", imported.len());
            }
        }
    }
    Ok(())
}

fn handle_toggle_pause() -> Result<()> {
    let new_state = always::always::pause::toggle_pause();
    
    // Send distributed notification via Swift script
    let notification_name = if new_state { "com.always.pause" } else { "com.always.resume" };
    std::process::Command::new("swift")
        .arg("scripts/send_notification.swift")
        .arg(notification_name)
        .output()?;
    
    // Broadcast event via UDS
    if new_state {
        always::always::event::global_broadcaster().paused();
    } else {
        always::always::event::global_broadcaster().resumed();
    }
    
    println!("Pause state: {}", if new_state { "paused" } else { "resumed" });
    Ok(())
}

fn handle_toggle_auto_enter() -> Result<()> {
    let new_state = always::always::pause::toggle_auto_enter();
    
    // Send distributed notification via Swift script
    let notification_name = if new_state { "com.always.autoEnterEnabled" } else { "com.always.autoEnterDisabled" };
    std::process::Command::new("swift")
        .arg("scripts/send_notification.swift")
        .arg(notification_name)
        .output()?;
    
    // Broadcast event via UDS
    if new_state {
        always::always::event::global_broadcaster().auto_enter_enabled();
    } else {
        always::always::event::global_broadcaster().auto_enter_disabled();
    }
    
    println!("Auto-enter state: {}", if new_state { "enabled" } else { "disabled" });
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
