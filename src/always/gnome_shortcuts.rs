//! Global shortcuts on GNOME Wayland. Wayland hides key events from ordinary
//! clients, so the configured combos are registered as GNOME custom shortcuts
//! that run `always shortcut <action>`. Entries the user created are kept.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

const LIST_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const ENTRY_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const BASE_PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings";

/// One action to register. `accel` is `None` when the action has no shortcut.
pub struct Binding {
    pub id: &'static str,
    pub label: &'static str,
    pub action: &'static str,
    pub accel: Option<String>,
}

pub fn is_gnome() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("gnome")))
}

/// A combo in GNOME's accelerator syntax; `None` for keys GNOME cannot bind.
pub fn accelerator(ctrl: bool, shift: bool, alt: bool, key: &str) -> Option<String> {
    if key.is_empty() || key == "fn" {
        return None;
    }
    let mut accel = String::new();
    for (held, name) in [(ctrl, "<Control>"), (shift, "<Shift>"), (alt, "<Alt>")] {
        if held {
            accel.push_str(name);
        }
    }
    accel.push_str(key);
    Some(accel)
}

fn path_for(id: &str) -> String {
    format!("{BASE_PATH}/always-{id}/")
}

fn parse_list(value: &str) -> Vec<String> {
    value
        .split('\'')
        .skip(1)
        .step_by(2)
        .map(ToString::to_string)
        .collect()
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn format_list(items: &[String]) -> String {
    if items.is_empty() {
        return "@as []".to_string();
    }
    let quoted: Vec<String> = items.iter().map(|i| quote(i)).collect();
    format!("[{}]", quoted.join(", "))
}

/// The user's entries, then ours for every action that has a combo.
fn merged_list(current: Vec<String>, bindings: &[Binding]) -> Vec<String> {
    let ours: Vec<String> = bindings.iter().map(|b| path_for(b.id)).collect();
    let mut merged: Vec<String> = current.into_iter().filter(|p| !ours.contains(p)).collect();
    merged.extend(
        bindings
            .iter()
            .filter(|b| b.accel.is_some())
            .map(|b| path_for(b.id)),
    );
    merged
}

fn gsettings(args: &[&str]) -> Result<String> {
    let output = Command::new("gsettings")
        .args(args)
        .output()
        .context("run gsettings")?;
    if !output.status.success() {
        bail!(
            "gsettings {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The installed CLI: the running binary may live in an AppImage's temporary
/// extraction, which is gone after the next launch.
fn cli_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    home.map(|h| h.join(".local/bin/always"))
        .into_iter()
        .chain(["/usr/local/bin/always", "/usr/bin/always"].map(PathBuf::from))
        .find(|p| p.is_file())
        .map_or_else(
            || std::env::current_exe().context("locate the always binary"),
            Ok,
        )
}

fn shell_word(path: &Path) -> String {
    let path = path.display().to_string();
    if path.contains(char::is_whitespace) {
        format!("\"{path}\"")
    } else {
        path
    }
}

pub fn sync(bindings: &[Binding]) -> Result<()> {
    let cli = shell_word(&cli_path()?);
    for binding in bindings {
        let Some(accel) = &binding.accel else {
            continue;
        };
        let schema = format!("{ENTRY_SCHEMA}:{}", path_for(binding.id));
        let command = format!("{cli} shortcut {}", binding.action);
        gsettings(&["set", &schema, "name", &quote(binding.label)])?;
        gsettings(&["set", &schema, "command", &quote(&command)])?;
        gsettings(&["set", &schema, "binding", &quote(accel)])?;
    }
    let current = parse_list(&gsettings(&["get", LIST_SCHEMA, "custom-keybindings"])?);
    gsettings(&[
        "set",
        LIST_SCHEMA,
        "custom-keybindings",
        &format_list(&merged_list(current, bindings)),
    ])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(id: &'static str, accel: Option<&str>) -> Binding {
        Binding {
            id,
            label: id,
            action: id,
            accel: accel.map(ToString::to_string),
        }
    }

    #[test]
    fn accelerators_use_gnome_syntax() {
        assert_eq!(
            accelerator(true, false, true, "p").as_deref(),
            Some("<Control><Alt>p")
        );
        assert_eq!(
            accelerator(true, true, true, "space").as_deref(),
            Some("<Control><Shift><Alt>space")
        );
        assert_eq!(accelerator(false, false, false, "fn"), None);
    }

    #[test]
    fn list_round_trips_and_keeps_user_entries() {
        assert!(parse_list("@as []").is_empty());
        let user = format!("{BASE_PATH}/custom0/");
        let current = parse_list(&format_list(&[user.clone(), path_for("pause")]));
        let merged = merged_list(
            current,
            &[
                binding("pause", None),
                binding("auto-enter", Some("<Alt>a")),
            ],
        );
        assert_eq!(merged, vec![user, path_for("auto-enter")]);
        assert_eq!(format_list(&[]), "@as []");
    }

    #[test]
    fn quoting_escapes_gvariant_strings() {
        assert_eq!(quote(r"a'b\c"), r"'a\'b\\c'");
        assert_eq!(shell_word(Path::new("/a b/always")), "\"/a b/always\"");
    }
}
