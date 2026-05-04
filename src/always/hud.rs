//! HUD (Heads-Up Display) overlay for always status

/// Show a status bar notification
pub fn show_status_bar_indicator(paused: bool) -> anyhow::Result<()> {
    let icon = if paused { "🔇" } else { "🎤" };
    let status = if paused { "PAUSED" } else { "ACTIVE" };
    eprintln!("{} ALWAYS {}", icon, status);
    Ok(())
}

/// Show enhanced visual feedback with notification only
pub fn show_enhanced_status(paused: bool) -> anyhow::Result<()> {
    // Show status bar indicator
    show_status_bar_indicator(paused)?;
    Ok(())
}
