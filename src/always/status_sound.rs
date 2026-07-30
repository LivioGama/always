use std::str::FromStr;
use std::sync::atomic::{AtomicU8, Ordering};

static AUDIBLE_STATUS_SOUND: AtomicU8 = AtomicU8::new(StatusSoundSetting::Off as u8);
static PLAYBACK_IN_FLIGHT: AtomicU8 = AtomicU8::new(0);

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
    if PLAYBACK_IN_FLIGHT
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    std::thread::spawn(move || {
        // `afplay -v` affects only this cue. Never mutate global macOS output
        // volume: user or system changes made during a cue must persist.
        let _ = play_file(path, volume);
        PLAYBACK_IN_FLIGHT.store(0, Ordering::Release);
    });
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
    fn cue_playback_gate_starts_idle() {
        PLAYBACK_IN_FLIGHT.store(0, Ordering::Relaxed);
        assert_eq!(PLAYBACK_IN_FLIGHT.load(Ordering::Relaxed), 0);
    }
}
