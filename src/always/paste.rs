use std::io::Write as _;

use anyhow::{Context, Result};

pub fn copy_to_clipboard(text: String) -> Result<()> {
    let mut pbcopy = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("Failed to run pbcopy")?;
    pbcopy
        .stdin
        .as_mut()
        .context("Failed to open pbcopy stdin")?
        .write_all(text.as_bytes())
        .context("Failed to write transcript to clipboard")?;
    let status = pbcopy.wait().context("Failed waiting for pbcopy")?;
    if !status.success() {
        anyhow::bail!("pbcopy failed");
    }
    Ok(())
}

pub fn paste_text(auto_enter: bool) -> Result<()> {
    let mut script =
        String::from("tell application \"System Events\" to keystroke \"v\" using command down");
    if auto_enter {
        script.push_str("\ntell application \"System Events\" to key code 36");
    }

    let status = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .status()
        .context("Failed to run osascript for paste automation")?;
    if !status.success() {
        anyhow::bail!(
            "Paste automation failed. Ensure Accessibility permissions are enabled for always/Terminal"
        );
    }

    Ok(())
}

pub fn paste(text: &str, auto_enter: bool) -> Result<()> {
    copy_to_clipboard(text.to_string())?;
    paste_text(auto_enter)?;
    Ok(())
}
