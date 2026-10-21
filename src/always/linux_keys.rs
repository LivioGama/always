//! Real key presses for the user's keyboard layout (Linux, Hyprland).
//!
//! `wtype` types through the virtual-keyboard protocol with a keymap it
//! invents per call: each character gets an arbitrary keycode (9, 10, …).
//! Terminals and toolkits read the keysym and are fine. Browser engines are
//! not: Chromium and Firefox derive the DOM `code` from the keycode's
//! physical position, so typed letters reach web apps as other keys —
//! keycode 9 is Escape, 22 Backspace, 23 Tab. Measured: in Chromium
//! (Ozone/Wayland) "…in Slack" arrived as `S`=Backspace, `l`=Tab, and the
//! first character of every call was dropped; in the Slack web app the
//! Escape-coded first key left the open conversation.
//!
//! Here each character is pressed on the physical key that produces it in
//! the active layout (`xkbcli how-to-type`), through `ydotool` — a uinput
//! keyboard the compositor decodes with that same layout, like the user's
//! own fingers. Used only where it matters: windows whose process runs a
//! browser engine.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};

/// evdev codes.
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_ENTER: u16 = 28;
const KEY_LEFTCTRL: u16 = 29;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_RIGHTALT: u16 = 100;

/// The focused window (Hyprland) belongs to a process that ships a browser
/// engine: Chromium/Electron (`resources.pak` beside the executable) or
/// Gecko (`omni.ja`). False off Hyprland or when it cannot be told.
pub fn focused_window_runs_browser_engine() -> bool {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return false;
    }
    let Some(pid) = Command::new("hyprctl")
        .args(["activewindow", "-j"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .and_then(|w| w["pid"].as_i64())
    else {
        return false;
    };
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.to_path_buf()))
        .is_some_and(|dir| dir.join("resources.pak").is_file() || dir.join("omni.ja").is_file())
}

/// Type `text` as real key presses. Fails — and sends nothing — when a
/// character has no direct key in the layout (dead keys, compose) or the
/// tools are missing; the caller then uses its other injector.
pub fn type_text(text: &str) -> Result<()> {
    let layout = active_layout().context("no keyboard layout from hyprctl")?;
    let mut events = Vec::new();
    for ch in text.chars() {
        let (key, mods) = if ch == '\n' {
            (KEY_ENTER, Vec::new())
        } else {
            key_for(&layout, ch).with_context(|| format!("no key for {ch:?} in this layout"))?
        };
        press(&mut events, &mods, key);
    }
    send(&events)
}

/// Ctrl + the key that types `ch` in the active layout (`'v'` = paste,
/// wherever V sits).
pub fn ctrl_chord(ch: char, shift: bool) -> Result<()> {
    let layout = active_layout().context("no keyboard layout from hyprctl")?;
    let (key, _) = key_for(&layout, ch).with_context(|| format!("no key for {ch:?}"))?;
    let mut mods = vec![KEY_LEFTCTRL];
    if shift {
        mods.push(KEY_LEFTSHIFT);
    }
    let mut events = Vec::new();
    press(&mut events, &mods, key);
    send(&events)
}

/// `key` (layout-independent position: Enter, Backspace) `repeat` times.
pub fn tap(key: u16, repeat: usize) -> Result<()> {
    let mut events = Vec::new();
    for _ in 0..repeat {
        press(&mut events, &[], key);
    }
    send(&events)
}

fn press(events: &mut Vec<String>, mods: &[u16], key: u16) {
    events.extend(mods.iter().map(|m| format!("{m}:1")));
    events.push(format!("{key}:1"));
    events.push(format!("{key}:0"));
    events.extend(mods.iter().rev().map(|m| format!("{m}:0")));
}

fn send(events: &[String]) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let status = Command::new("ydotool")
        .args(["key", "-d", "2"])
        .args(events)
        .status()
        .context("run ydotool")?;
    if !status.success() {
        bail!("ydotool exited with {status}");
    }
    Ok(())
}

/// What `xkbcli` needs to rebuild the keymap the compositor applies to
/// ydotool's virtual keyboard (that device's entry, else the main keyboard).
#[derive(Clone, PartialEq, Eq, Hash)]
struct Layout {
    layout: String,
    variant: String,
    options: String,
    /// 1-based layout index as `xkbcli` prints it.
    index: u32,
}

fn active_layout() -> Option<Layout> {
    let out = Command::new("hyprctl")
        .args(["devices", "-j"])
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let keyboards = v["keyboards"].as_array()?;
    let kb = keyboards
        .iter()
        .find(|k| k["name"].as_str().is_some_and(|n| n.contains("ydotoold")))
        .or_else(|| keyboards.iter().find(|k| k["main"].as_bool() == Some(true)))?;
    let s = |f: &str| kb[f].as_str().unwrap_or("").to_string();
    Some(Layout {
        layout: s("layout"),
        variant: s("variant"),
        options: s("options"),
        index: kb["active_layout_index"].as_u64().unwrap_or(0) as u32 + 1,
    })
}

type KeyCache = HashMap<(Layout, char), Option<(u16, Vec<u16>)>>;
static CACHE: Mutex<Option<KeyCache>> = Mutex::new(None);

/// (evdev key, modifier keys) that type `ch` in `layout`; cached.
fn key_for(layout: &Layout, ch: char) -> Option<(u16, Vec<u16>)> {
    let mut guard = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(hit) = cache.get(&(layout.clone(), ch)) {
        return hit.clone();
    }
    let mut cmd = Command::new("xkbcli");
    cmd.args(["how-to-type", "--layout", &layout.layout]);
    if !layout.variant.is_empty() {
        cmd.args(["--variant", &layout.variant]);
    }
    if !layout.options.is_empty() {
        cmd.args(["--options", &layout.options]);
    }
    let found = cmd
        .arg(format!("U+{:04X}", ch as u32))
        .output()
        .ok()
        .and_then(|o| parse_how_to_type(&String::from_utf8_lossy(&o.stdout), layout.index));
    cache.insert((layout.clone(), ch), found.clone());
    found
}

/// First "Direct access" row of `xkbcli how-to-type` for layout `index`
/// whose modifiers we can press (none, Shift, AltGr = Mod5/LevelThree);
/// rows needing Lock or anything else are skipped.
fn parse_how_to_type(out: &str, index: u32) -> Option<(u16, Vec<u16>)> {
    let direct = out.split("=== Direct access ===").nth(1)?;
    let direct = direct.split("===").next()?;
    direct.lines().find_map(|line| {
        let (cols, mods) = line.split_once('[')?;
        let cols: Vec<&str> = cols.split_whitespace().collect();
        let keycode: u16 = cols.first()?.parse().ok()?;
        let layout: u32 = cols.get(2)?.parse().ok()?;
        if layout != index || keycode < 8 {
            return None;
        }
        let mut keys = Vec::new();
        for m in mods
            .trim_end_matches(|c: char| c == ']' || c.is_whitespace())
            .split_whitespace()
        {
            match m {
                "Shift" => keys.push(KEY_LEFTSHIFT),
                "Mod5" | "LevelThree" => {
                    if !keys.contains(&KEY_RIGHTALT) {
                        keys.push(KEY_RIGHTALT);
                    }
                }
                _ => return None,
            }
        }
        Some((keycode - 8, keys))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const Z_DE: &str = "keysym: Z (0x005a)

=== Direct access ===

KEYCODE  KEY NAME  LAYOUT   LAYOUT NAME          LEVEL#  MODIFIERS
29       AD06      1        German               2       [ Shift ]
29       AD06      1        German               2       [ Lock ]

=== Access via Compose sequences ===
";
    const AT_DE: &str = "keysym: at (0x0040)

=== Direct access ===

KEYCODE  KEY NAME  LAYOUT   LAYOUT NAME          LEVEL#  MODIFIERS
24       AD01      1        German               3       [ Mod5 LevelThree ]
24       AD01      1        German               3       [ Lock Mod5 LevelThree ]
";
    const LOCK_ONLY: &str = "=== Direct access ===
KEYCODE  KEY NAME  LAYOUT   LAYOUT NAME          LEVEL#  MODIFIERS
29       AD06      1        German               2       [ Lock ]
";

    #[test]
    fn rows_become_evdev_keys_with_pressable_modifiers() {
        // xkb keycode 29 = evdev 21 (the key labelled Z on a German board).
        assert_eq!(parse_how_to_type(Z_DE, 1), Some((21, vec![KEY_LEFTSHIFT])));
        // @ on German = AltGr + Q.
        assert_eq!(parse_how_to_type(AT_DE, 1), Some((16, vec![KEY_RIGHTALT])));
        // Only Caps Lock reaches it, or another layout: not pressable.
        assert_eq!(parse_how_to_type(LOCK_ONLY, 1), None);
        assert_eq!(parse_how_to_type(Z_DE, 2), None);
        assert_eq!(parse_how_to_type("", 1), None);
    }

    #[test]
    fn a_press_wraps_the_key_in_its_modifiers() {
        let mut ev = Vec::new();
        press(&mut ev, &[KEY_LEFTCTRL, KEY_LEFTSHIFT], 47);
        assert_eq!(ev, ["29:1", "42:1", "47:1", "47:0", "42:0", "29:0"]);
    }

    /// Against the real `xkbcli` when present: German puts z and y where
    /// US has y and z — exactly what position-based typing gets wrong.
    #[test]
    fn german_layout_resolves_z_and_y_to_their_german_keys() {
        if Command::new("xkbcli").arg("--help").output().is_err() {
            return;
        }
        let de = Layout {
            layout: "de".into(),
            variant: String::new(),
            options: String::new(),
            index: 1,
        };
        assert_eq!(key_for(&de, 'z'), Some((21, vec![])));
        assert_eq!(key_for(&de, 'y'), Some((44, vec![])));
        assert_eq!(key_for(&de, 'Z'), Some((21, vec![KEY_LEFTSHIFT])));
    }
}
