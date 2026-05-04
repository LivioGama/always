use anyhow::Result;
use std::time::{Duration, Instant};

/// Cross-platform microphone usage monitor
pub struct MicrophoneMonitor {
    last_check: Instant,
    check_interval: Duration,
    last_usage_state: bool,
}

impl Default for MicrophoneMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl MicrophoneMonitor {
    pub fn new() -> Self {
        Self {
            last_check: Instant::now(),
            check_interval: Duration::from_millis(3000), // Check every 3 seconds
            last_usage_state: false,
        }
    }

    /// Check if other applications are currently using the microphone
    pub fn is_microphone_in_use(&mut self) -> Result<bool> {
        let now = Instant::now();

        // Throttle checks to avoid excessive system calls
        if now.duration_since(self.last_check) < self.check_interval {
            return Ok(self.last_usage_state);
        }

        self.last_check = now;

        let in_use = self.check_microphone_usage()?;
        self.last_usage_state = in_use;

        Ok(in_use)
    }

    /// Platform-specific microphone usage detection
    fn check_microphone_usage(&self) -> Result<bool> {
        #[cfg(target_os = "macos")]
        return self.check_microphone_usage_macos();

        #[cfg(target_os = "linux")]
        return self.check_microphone_usage_linux();

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            // Fallback for other platforms - just return false for now
            tracing::warn!("microphone_monitoring_not_implemented");
            Ok(false)
        }
    }

    /// Get a list of applications currently using the microphone
    pub fn get_microphone_users(&self) -> Result<Vec<String>> {
        #[cfg(target_os = "macos")]
        return self.get_microphone_users_macos();

        #[cfg(target_os = "linux")]
        return self.get_microphone_users_linux();

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        return Ok(Vec::new());
    }
}

// Platform-specific implementations
#[cfg(target_os = "macos")]
impl MicrophoneMonitor {
    /// Single lightweight subprocess check for microphone usage.
    /// Replaces the old 3-subprocess pipeline (system_profiler + lsof + sh/ps).
    /// Uses one `lsof` call filtered to coreaudiod, excluding our own processes.
    fn check_microphone_usage_macos(&self) -> Result<bool> {
        let output_str = self.run_mic_lsof()?;
        Ok(!output_str.is_empty())
    }

    fn get_microphone_users_macos(&self) -> Result<Vec<String>> {
        let output_str = self.run_mic_lsof()?;
        if output_str.is_empty() {
            return Ok(Vec::new());
        }

        let output_lower = output_str.to_lowercase();
        let mut users = Vec::new();

        const APP_MAPPING: &[(&str, &str)] = &[
            ("zoom", "Zoom"),
            ("skype", "Skype"),
            ("teams", "Microsoft Teams"),
            ("discord", "Discord"),
            ("facetime", "FaceTime"),
            ("obs", "OBS Studio"),
            ("audacity", "Audacity"),
            ("garageband", "GarageBand"),
            ("quicktime", "QuickTime Player"),
            ("voice memos", "Voice Memos"),
        ];

        for (process_name, display_name) in APP_MAPPING {
            if output_lower.contains(process_name) {
                users.push(display_name.to_string());
            }
        }

        if users.is_empty() {
            users.push("Unknown application".to_string());
        }

        Ok(users)
    }

    /// Run a single `lsof` command to find non-Always processes using CoreAudio.
    /// Returns the filtered output (empty string = no other app is using the mic).
    fn run_mic_lsof(&self) -> Result<String> {
        use std::process::Command;

        // Single subprocess: lsof for CoreAudio file descriptors,
        // excluding our own processes (always, rec, sox, coreaudiod itself).
        let output = Command::new("sh")
            .args([
                "-c",
                "lsof -c /coreaudio/i 2>/dev/null | grep -iv -e always -e '\\brec\\b' -e sox -e coreaudiod | head -5",
            ])
            .output();

        match output {
            Ok(result) => {
                let stdout = String::from_utf8_lossy(&result.stdout);
                Ok(stdout.trim().to_string())
            }
            Err(_) => Ok(String::new()),
        }
    }
}

#[cfg(target_os = "linux")]
impl MicrophoneMonitor {
    fn check_microphone_usage_linux(&self) -> Result<bool> {
        use std::process::Command;

        // Method 1: Check PulseAudio/PipeWire sources
        if let Ok(true) = self.check_pulseaudio_sources() {
            return Ok(true);
        }

        // Method 2: Check /proc/asound/cards for active audio
        if let Ok(true) = self.check_alsa_devices() {
            return Ok(true);
        }

        // Method 3: Fallback to common process detection
        self.check_common_audio_apps_linux()
    }

    fn check_pulseaudio_sources(&self) -> Result<bool> {
        use std::process::Command;

        // Try pactl first (PulseAudio)
        let output = Command::new("pactl")
            .args(&["list", "source-outputs"])
            .output();

        match output {
            Ok(result) if result.status.success() => {
                let output_str = String::from_utf8_lossy(&result.stdout);
                // If there are any source outputs, something is using the microphone
                return Ok(output_str.contains("Source Output #"));
            }
            _ => {}
        }

        // Try pw-cli (PipeWire)
        let output = Command::new("pw-cli").args(&["list-objects"]).output();

        match output {
            Ok(result) if result.status.success() => {
                let output_str = String::from_utf8_lossy(&result.stdout);
                return Ok(output_str.contains("node.name") && output_str.contains("input"));
            }
            _ => {}
        }

        Ok(false)
    }

    fn check_alsa_devices(&self) -> Result<bool> {
        use std::fs;

        // Check if any ALSA capture devices are in use
        match fs::read_to_string("/proc/asound/cards") {
            Ok(content) => {
                // Basic check - if there are sound cards present, we can do more detailed checks
                if !content.is_empty() {
                    // Check for active PCM streams
                    for card_dir in fs::read_dir("/proc/asound/").unwrap_or_else(|_| {
                        return std::fs::read_dir("/dev/null").unwrap();
                    }) {
                        if let Ok(entry) = card_dir {
                            let path = entry.path().join("pcm0c").join("info");
                            if path.exists() {
                                return Ok(true);
                            }
                        }
                    }
                }
            }
            Err(_) => {}
        }

        Ok(false)
    }

    fn check_common_audio_apps_linux(&self) -> Result<bool> {
        use std::process::Command;

        let output = Command::new("ps")
            .args(&["-axo", "comm"])
            .output()
            .context("Failed to run ps command")?;

        if !output.status.success() {
            return Ok(false);
        }

        let processes = String::from_utf8(output.stdout).context("Invalid UTF-8 in ps output")?;

        const AUDIO_APPS: &[&str] = &[
            "zoom",
            "skype",
            "teams",
            "discord",
            "firefox",
            "chrome",
            "obs",
            "audacity",
            "kdenlive",
            "openshot",
            "audacious",
            "pulseaudio",
            "pipewire",
            "jack",
        ];

        let processes_lower = processes.to_lowercase();
        for app in AUDIO_APPS {
            if processes_lower.contains(app) {
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn get_microphone_users_linux(&self) -> Result<Vec<String>> {
        use std::process::Command;

        let mut users = Vec::new();

        // Try to get detailed info from PulseAudio
        let output = Command::new("pactl")
            .args(&["list", "source-outputs"])
            .output();

        match output {
            Ok(result) if result.status.success() => {
                let output_str = String::from_utf8_lossy(&result.stdout);
                // Parse application names from PulseAudio output
                for line in output_str.lines() {
                    if line.trim().starts_with("application.name = ") {
                        if let Some(app_name) = line.split('"').nth(1) {
                            users.push(app_name.to_string());
                        }
                    }
                }
            }
            _ => {
                // Fallback to process detection
                return self.get_common_audio_apps_linux();
            }
        }

        Ok(users)
    }

    fn get_common_audio_apps_linux(&self) -> Result<Vec<String>> {
        use std::process::Command;

        let mut users = Vec::new();

        let output = Command::new("ps")
            .args(&["-axo", "comm"])
            .output()
            .context("Failed to run ps command")?;

        if !output.status.success() {
            return Ok(users);
        }

        let processes = String::from_utf8(output.stdout).context("Invalid UTF-8 in ps output")?;

        const AUDIO_APPS: &[(&str, &str)] = &[
            ("zoom", "Zoom"),
            ("skype", "Skype"),
            ("teams", "Microsoft Teams"),
            ("discord", "Discord"),
            ("firefox", "Firefox"),
            ("chrome", "Chrome"),
            ("obs", "OBS Studio"),
            ("audacity", "Audacity"),
        ];

        let processes_lower = processes.to_lowercase();
        for (process_name, display_name) in AUDIO_APPS {
            if processes_lower.contains(process_name) {
                users.push(display_name.to_string());
            }
        }

        Ok(users)
    }
}

#[cfg(test)]
mod tests {
    use super::MicrophoneMonitor;

    #[test]
    fn can_create_monitor() {
        let _monitor = MicrophoneMonitor::new();
    }

    #[test]
    fn can_check_usage() {
        let mut monitor = MicrophoneMonitor::new();
        // This might return true or false depending on system state
        let _result = monitor.is_microphone_in_use();
    }
}
