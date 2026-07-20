use std::str::FromStr;
use std::sync::{
    LazyLock, Mutex,
    atomic::{AtomicU8, Ordering},
};

static AUDIBLE_STATUS_SOUND: AtomicU8 = AtomicU8::new(StatusSoundSetting::Off as u8);
static PLAYBACK_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusSound {
    Listening,
    Transcribing,
    Success,
    Failure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum StatusSoundSetting {
    #[default]
    Off = 0,
    Low = 1,
    Medium = 2,
    High = 3,
}

impl StatusSoundSetting {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    pub fn afplay_volume(self) -> &'static str {
        match self {
            Self::Off => "0",
            Self::Low => "0.45",
            Self::Medium => "0.75",
            Self::High => "2.0",
        }
    }

    pub fn is_enabled(self) -> bool {
        self != Self::Off
    }
}

impl FromStr for StatusSoundSetting {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_lowercase().as_str() {
            "off" | "false" | "0" => Ok(Self::Off),
            "low" => Ok(Self::Low),
            "medium" | "normal" => Ok(Self::Medium),
            "high" | "loud" | "true" | "1" => Ok(Self::High),
            _ => anyhow::bail!("audible_status_sound must be one of: off, low, medium, high"),
        }
    }
}

pub fn set_setting(setting: StatusSoundSetting) {
    AUDIBLE_STATUS_SOUND.store(setting as u8, Ordering::Relaxed);
}

pub fn setting() -> StatusSoundSetting {
    match AUDIBLE_STATUS_SOUND.load(Ordering::Relaxed) {
        1 => StatusSoundSetting::Low,
        2 => StatusSoundSetting::Medium,
        3 => StatusSoundSetting::High,
        _ => StatusSoundSetting::Off,
    }
}

pub fn is_enabled() -> bool {
    setting().is_enabled()
}

pub fn cue(sound: StatusSound) {
    if !is_enabled() {
        return;
    }
    play(sound);
}

pub fn sound_path(sound: StatusSound) -> &'static str {
    match sound {
        StatusSound::Listening => "/System/Library/Sounds/Pop.aiff",
        StatusSound::Transcribing => "/System/Library/Sounds/Tink.aiff",
        StatusSound::Success => "/System/Library/Sounds/Ping.aiff",
        StatusSound::Failure => "/System/Library/Sounds/Basso.aiff",
    }
}

#[cfg(target_os = "macos")]
fn play(sound: StatusSound) {
    let path = sound_path(sound);
    let setting = setting();
    let volume = setting.afplay_volume();
    std::thread::spawn(move || {
        let _guard = PLAYBACK_LOCK.lock().ok();
        if setting == StatusSoundSetting::High {
            let _ = play_with_ducked_output(path, volume);
        } else {
            let _ = play_file(path, volume);
        }
    });
}

#[cfg(target_os = "macos")]
fn play_with_ducked_output(path: &str, volume: &str) -> std::io::Result<std::process::ExitStatus> {
    let original = current_output_volume();
    if let Some(original) = original {
        let ducked = ducked_output_volume(original);
        if ducked < original {
            let _ = set_output_volume(ducked);
        }
    }
    let result = play_file(path, volume);
    if let Some(original) = original {
        let _ = set_output_volume(original);
    }
    result
}

#[cfg(target_os = "macos")]
fn play_file(path: &str, volume: &str) -> std::io::Result<std::process::ExitStatus> {
    std::process::Command::new("/usr/bin/afplay")
        .arg("-v")
        .arg(volume)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
}

fn ducked_output_volume(original: u8) -> u8 {
    if original > 45 {
        35
    } else if original > 20 {
        original.saturating_sub(12)
    } else {
        original
    }
}

#[cfg(target_os = "macos")]
fn current_output_volume() -> Option<u8> {
    let output = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg("output volume of (get volume settings)")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

#[cfg(target_os = "macos")]
fn set_output_volume(volume: u8) -> std::io::Result<std::process::ExitStatus> {
    std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(format!("set volume output volume {volume}"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
}

#[cfg(not(target_os = "macos"))]
fn play(_sound: StatusSound) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_disabled() {
        set_setting(StatusSoundSetting::Off);
        assert!(!is_enabled());
    }

    #[test]
    fn can_set_runtime_setting() {
        set_setting(StatusSoundSetting::Off);
        assert!(!is_enabled());
        set_setting(StatusSoundSetting::Low);
        assert!(is_enabled());
        assert_eq!(setting(), StatusSoundSetting::Low);
        set_setting(StatusSoundSetting::Off);
    }

    #[test]
    fn parses_sound_levels() {
        assert_eq!(
            "off".parse::<StatusSoundSetting>().unwrap(),
            StatusSoundSetting::Off
        );
        assert_eq!(
            "low".parse::<StatusSoundSetting>().unwrap(),
            StatusSoundSetting::Low
        );
        assert_eq!(
            "normal".parse::<StatusSoundSetting>().unwrap(),
            StatusSoundSetting::Medium
        );
        assert_eq!(
            "loud".parse::<StatusSoundSetting>().unwrap(),
            StatusSoundSetting::High
        );
        assert!("silent".parse::<StatusSoundSetting>().is_err());
    }

    #[test]
    fn tonal_status_sounds_are_distinct() {
        let paths = [
            sound_path(StatusSound::Listening),
            sound_path(StatusSound::Transcribing),
            sound_path(StatusSound::Success),
            sound_path(StatusSound::Failure),
        ];
        let unique = std::collections::BTreeSet::from(paths);
        assert_eq!(unique.len(), paths.len());
    }

    #[test]
    fn success_uses_ping_sound() {
        assert_eq!(
            sound_path(StatusSound::Success),
            "/System/Library/Sounds/Ping.aiff"
        );
    }

    #[test]
    fn high_setting_boosts_cue_gain() {
        assert_eq!(StatusSoundSetting::High.afplay_volume(), "2.0");
    }

    #[test]
    fn ducked_volume_is_lower_but_not_silent() {
        assert_eq!(ducked_output_volume(80), 35);
        assert_eq!(ducked_output_volume(35), 23);
        assert_eq!(ducked_output_volume(15), 15);
    }
}
