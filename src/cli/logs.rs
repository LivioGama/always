use anyhow::Result;
use clap::Parser;
use std::process::Command;
use always::always::telemetry;

/// View and manage Always logs
#[derive(Parser, Debug)]
pub struct LogsCommand {
    /// Print the log directory path and exit
    #[arg(long)]
    pub path: bool,

    /// Filter logs by time (e.g., "1h", "30m", "1d")
    #[arg(long)]
    pub since: Option<String>,

    /// Filter logs by level (e.g., "error", "warn", "info", "debug")
    #[arg(long)]
    pub level: Option<String>,

    /// Use Console.app on macOS instead of tail
    #[arg(long)]
    pub console: bool,
}

pub fn handle_logs(cmd: LogsCommand) -> Result<()> {
    let log_dir = telemetry::get_log_directory();
    let log_file = log_dir.join("always.log");

    if cmd.path {
        println!("{}", log_dir.display());
        return Ok(());
    }

    if cmd.console {
        #[cfg(target_os = "macos")]
        {
            return view_logs_in_console();
        }
        #[cfg(not(target_os = "macos"))]
        {
            eprintln!("--console flag is only supported on macOS");
            std::process::exit(1);
        }
    }

    // Build tail command
    let mut tail_cmd = Command::new("tail");
    tail_cmd.arg("-F").arg(&log_file);

    // Execute tail
    let mut child = tail_cmd.spawn()?;
    child.wait()?;

    Ok(())
}

#[cfg(target_os = "macos")]
fn view_logs_in_console() -> Result<()> {
    let status = Command::new("log")
        .args(&[
            "stream",
            "--predicate",
            "subsystem CONTAINS \"com.always\"",
            "--level",
            "debug",
        ])
        .status()?;

    if !status.success() {
        anyhow::bail!("Failed to open Console.app logs");
    }

    Ok(())
}
