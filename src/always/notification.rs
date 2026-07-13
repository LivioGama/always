//! Visual notification system for always pause/resume and auto-enter status

use anyhow::Result;

/// Show a system notification using macOS osascript
pub fn notify(title: &str, message: &str, sound: bool) -> Result<()> {
    // Use osascript for macOS notifications
    let mut command = std::process::Command::new("osascript");
    command.arg("-e");

    let script = if sound {
        format!(
            r#"display notification "{}" with title "{}" sound name "Ping""#,
            message.replace("\"", "\\\""),
            title.replace("\"", "\\\"")
        )
    } else {
        format!(
            r#"display notification "{}" with title "{}""#,
            message.replace("\"", "\\\""),
            title.replace("\"", "\\\"")
        )
    };

    command.arg(script);

    match command.output() {
        Ok(_) => Ok(()),
        Err(_) => {
            // Fallback to logging
            tracing::warn!(title, message, "notification_fallback");
            Ok(())
        }
    }
}

/// Show pause status change with notification
pub fn show_pause_status_change(paused: bool) -> Result<()> {
    let (status, icon) = if paused {
        ("PAUSED", "🔇")
    } else {
        ("ACTIVE", "🎤")
    };

    // Show macOS notification with sound
    notify("Always Listening", &format!("{} {}", icon, status), true)?;

    Ok(())
}

/// Show auto-enter status change with notification
pub fn show_auto_enter_status_change(enabled: bool) -> Result<()> {
    let (status, icon) = if enabled {
        ("ENABLED", "⏎")
    } else {
        ("DISABLED", "⏸")
    };

    // Show macOS notification with sound
    notify("Auto-Enter", &format!("{} {}", icon, status), true)?;

    Ok(())
}
