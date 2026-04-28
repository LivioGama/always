//! Visual notification system for always pause/resume and auto-enter status

use anyhow::Result;

/// Show a system notification
pub fn notify(title: &str, message: &str, _sound: bool) -> Result<()> {
    // Try to use notify-send if available (Linux)
    if let Ok(_) = std::process::Command::new("notify-send")
        .arg(title)
        .arg(message)
        .output()
    {
        return Ok(());
    }

    // Fallback to console output
    eprintln!("🔔 {} - {}", title, message);
    Ok(())
}

/// Show pause status change with notification
pub fn show_pause_status_change(paused: bool) -> Result<()> {
    let (status, icon) = if paused {
        ("PAUSED", "🔇")
    } else {
        ("ACTIVE", "🎤")
    };

    // Show notification with sound
    notify("ALWAYS", &format!("{} ALWAYS {}", icon, status), true)?;

    // Also print to console for daemon logs
    eprintln!("{} ALWAYS {} (Ctrl+Shift+P to toggle)", icon, status.to_lowercase());

    Ok(())
}

/// Show auto-enter status change with notification
pub fn show_auto_enter_status_change(enabled: bool) -> Result<()> {
    let (status, icon) = if enabled {
        ("ENABLED", "⏎")
    } else {
        ("DISABLED", "⏸")
    };

    // Show notification with sound
    notify("ALWAYS Auto-Enter", &format!("{} Auto-Enter {}", icon, status), true)?;

    // Also print to console for daemon logs
    eprintln!("{} Auto-Enter {} (Ctrl+Shift+A to toggle)", icon, status.to_lowercase());

    Ok(())
}