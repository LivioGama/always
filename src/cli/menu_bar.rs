use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;

/// OS-level menu bar cache reset (Control Center displayablemenuextras + NSStatusItem keys).
pub fn reset() -> Result<()> {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/deep-reset-menu-bar.sh");
    if !script.is_file() {
        anyhow::bail!(
            "missing {}; run from the always repo checkout",
            script.display()
        );
    }
    let status = Command::new("/bin/bash")
        .arg(&script)
        .status()
        .context("failed to run deep-reset-menu-bar.sh")?;
    if !status.success() {
        anyhow::bail!("deep-reset-menu-bar.sh exited with {}", status);
    }
    Ok(())
}
