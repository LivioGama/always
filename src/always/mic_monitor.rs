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
            // The CoreAudio process-object probe is an in-process query
            // (microseconds), so a 1s cadence keeps call detection snappy
            // without measurable cost. The lsof fallback only runs on
            // macOS < 14.4 where the probe is unavailable.
            check_interval: Duration::from_millis(1000),
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

/// CoreAudio process-object probe (macOS 14.4+): asks coreaudiod which
/// processes are actually running audio *input* right now — the same
/// source Control Center uses for the orange-dot attribution. Catches
/// browser calls (Meet in Chrome), Slack huddles, Zoom, FaceTime —
/// anything that opens a capture stream — with zero subprocesses.
///
/// The old `lsof -c /coreaudio/i` probe only ever matched processes
/// *named* `coreaudio*` (i.e. coreaudiod itself, which was then excluded
/// as a helper), so mic-conflict auto-pause had never fired once in the
/// field. It is kept below only as a fallback for macOS < 14.4 where the
/// process-object properties don't exist.
#[cfg(target_os = "macos")]
mod coreaudio_probe {
    use anyhow::{Result, bail};
    use std::ffi::c_void;

    #[repr(C)]
    struct AudioObjectPropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    const fn fourcc(b: &[u8; 4]) -> u32 {
        ((b[0] as u32) << 24) | ((b[1] as u32) << 16) | ((b[2] as u32) << 8) | (b[3] as u32)
    }

    const SYSTEM_OBJECT: u32 = 1; // kAudioObjectSystemObject
    const SCOPE_GLOBAL: u32 = fourcc(b"glob"); // kAudioObjectPropertyScopeGlobal
    const ELEMENT_MAIN: u32 = 0; // kAudioObjectPropertyElementMain
    const PROP_PROCESS_OBJECT_LIST: u32 = fourcc(b"prs#"); // kAudioHardwarePropertyProcessObjectList
    const PROP_PROCESS_PID: u32 = fourcc(b"ppid"); // kAudioProcessPropertyPID
    const PROP_PROCESS_BUNDLE_ID: u32 = fourcc(b"pbid"); // kAudioProcessPropertyBundleID
    const PROP_PROCESS_IS_RUNNING_INPUT: u32 = fourcc(b"piri"); // kAudioProcessPropertyIsRunningInput
    const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectGetPropertyDataSize(
            object_id: u32,
            address: *const AudioObjectPropertyAddress,
            qualifier_size: u32,
            qualifier_data: *const c_void,
            out_size: *mut u32,
        ) -> i32;
        fn AudioObjectGetPropertyData(
            object_id: u32,
            address: *const AudioObjectPropertyAddress,
            qualifier_size: u32,
            qualifier_data: *const c_void,
            io_size: *mut u32,
            out_data: *mut c_void,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringGetCString(
            string: *const c_void,
            buffer: *mut u8,
            buffer_size: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        // LaunchServices: resolve a bundle id to the installed .app URL.
        fn LSCopyApplicationURLsForBundleIdentifier(
            bundle_id: *const c_void, // CFString
            error: *mut *mut c_void,   // CFErrorRef *
        ) -> *const c_void; // CFArrayRef
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(array: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(array: *const c_void, idx: isize) -> *const c_void;
        fn CFURLGetFileSystemRepresentation(
            url: *const c_void,
            resolve_against_base: u8,
            buffer: *mut u8,
            max_buf_len: isize,
        ) -> u8;
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const u8,
            encoding: u32,
        ) -> *const c_void;
        fn CFBundleCreate(
            alloc: *const c_void,
            bundle_url: *const c_void,
        ) -> *const c_void;
        fn CFBundleGetValueForInfoDictionaryKey(
            bundle: *const c_void,
            key: *const c_void, // CFString
        ) -> *const c_void;
        fn CFURLCreateFromFileSystemRepresentation(
            alloc: *const c_void,
            buffer: *const u8,
            size: isize,
            is_directory: u8,
        ) -> *const c_void;
    }

    unsafe extern "C" {
        // libproc (part of libSystem — no extra link needed).
        fn proc_pidpath(pid: i32, buffer: *mut u8, buffer_size: u32) -> i32;
    }

    /// Always-on system listeners that legitimately run input around the
    /// clock ("Hey Siri" wake word etc.). Never treat these as a call.
    const SYSTEM_LISTENER_BUNDLES: &[&str] = &[
        "com.apple.CoreSpeech",
        "com.apple.corespeechd",
        "com.apple.assistantd",
        "com.apple.siriactionsd",
        "com.apple.SiriNCService",
    ];

    /// macOS Settings extensions that open a capture stream only to drive
    /// an input-level meter, not to record. The Sound extension in
    /// particular can outlive the visible System Settings window and keep
    /// the stream open, which would permanently pause Always if treated as
    /// a real conflict.
    const METERING_ONLY_BUNDLES: &[&str] = &["com.apple.Sound-Settings.extension"];

    /// Our own capture chain: the daemon (`always` / `always-daemon`)
    /// records through a spawned `rec`/`sox` child, and coreaudiod is
    /// the audio server itself.
    const HELPER_BASENAMES: &[&str] = &["always", "always-daemon", "rec", "sox", "coreaudiod"];

    fn addr(selector: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            selector,
            scope: SCOPE_GLOBAL,
            element: ELEMENT_MAIN,
        }
    }

    fn get_u32(object: u32, selector: u32) -> Option<u32> {
        let address = addr(selector);
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                (&raw mut value).cast(),
            )
        };
        (status == 0).then_some(value)
    }

    fn get_pid(object: u32) -> Option<i32> {
        let address = addr(PROP_PROCESS_PID);
        let mut value: i32 = 0;
        let mut size = std::mem::size_of::<i32>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                (&raw mut value).cast(),
            )
        };
        (status == 0).then_some(value)
    }

    fn get_bundle_id(object: u32) -> Option<String> {
        let address = addr(PROP_PROCESS_BUNDLE_ID);
        let mut cf: *const c_void = std::ptr::null();
        let mut size = std::mem::size_of::<*const c_void>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                (&raw mut cf).cast(),
            )
        };
        if status != 0 || cf.is_null() {
            return None;
        }
        let mut buf = [0u8; 512];
        let ok = unsafe {
            CFStringGetCString(
                cf,
                buf.as_mut_ptr(),
                buf.len() as isize,
                CF_STRING_ENCODING_UTF8,
            )
        };
        unsafe { CFRelease(cf) };
        if ok == 0 {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        let s = String::from_utf8_lossy(&buf[..end]).into_owned();
        (!s.is_empty()).then_some(s)
    }

    fn pid_basename(pid: i32) -> Option<String> {
        let mut buf = [0u8; 4096];
        let len = unsafe { proc_pidpath(pid, buf.as_mut_ptr(), buf.len() as u32) };
        if len <= 0 {
            return None;
        }
        let path = String::from_utf8_lossy(&buf[..len as usize]).into_owned();
        path.rsplit('/').next().map(str::to_string)
    }

    /// Resolve a bundle id to a human-readable display name via
    /// LaunchServices + the app's Info.plist. Falls back to the last
    /// path component of the bundle id if resolution fails.
    fn bundle_display_name(bundle_id: &str) -> String {
        let display = resolve_bundle_display_name(bundle_id);
        display.unwrap_or_else(|| {
            // "com.superduper.superwhisper" → "superwhisper"
            bundle_id
                .rsplit('.')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or(bundle_id)
                .to_string()
        })
    }

    fn resolve_bundle_display_name(bundle_id: &str) -> Option<String> {
        unsafe {
            let cf_bundle_id = CFStringCreateWithCString(
                std::ptr::null(),
                bundle_id.as_ptr(),
                CF_STRING_ENCODING_UTF8,
            );
            if cf_bundle_id.is_null() {
                return None;
            }
            let mut error: *mut c_void = std::ptr::null_mut();
            let urls = LSCopyApplicationURLsForBundleIdentifier(cf_bundle_id, &raw mut error);
            CFRelease(cf_bundle_id);
            if !error.is_null() {
                CFRelease(error);
            }
            if urls.is_null() {
                return None;
            }
            let count = CFArrayGetCount(urls);
            if count == 0 {
                CFRelease(urls);
                return None;
            }
            let url = CFArrayGetValueAtIndex(urls, 0);
            if url.is_null() {
                CFRelease(urls);
                return None;
            }
            let mut path_buf = [0u8; 4096];
            let ok = CFURLGetFileSystemRepresentation(url, 1, path_buf.as_mut_ptr(), path_buf.len() as isize);
            CFRelease(urls);
            if ok == 0 {
                return None;
            }
            let path_end = path_buf.iter().position(|&b| b == 0).unwrap_or(path_buf.len());
            let app_url = String::from_utf8_lossy(&path_buf[..path_end]).into_owned();

            // Create a CFBundle from the .app URL and read the display
            // name from its Info.plist.
            let cf_url = create_cf_url_from_path(&app_url)?;
            let bundle = CFBundleCreate(std::ptr::null(), cf_url);
            if bundle.is_null() {
                CFRelease(cf_url);
                return None;
            }
            let cf_key = CFStringCreateWithCString(
                std::ptr::null(),
                b"CFBundleDisplayName\0".as_ptr(),
                CF_STRING_ENCODING_UTF8,
            );
            let mut value = CFBundleGetValueForInfoDictionaryKey(bundle, cf_key);
            CFRelease(cf_key);
            // Fall back to CFBundleName if CFBundleDisplayName is absent.
            if value.is_null() {
                let cf_key2 = CFStringCreateWithCString(
                    std::ptr::null(),
                    b"CFBundleName\0".as_ptr(),
                    CF_STRING_ENCODING_UTF8,
                );
                value = CFBundleGetValueForInfoDictionaryKey(bundle, cf_key2);
                CFRelease(cf_key2);
            }
            let result = if !value.is_null() {
                cf_string_to_string(value)
            } else {
                None
            };
            CFRelease(bundle);
            CFRelease(cf_url);
            result
        }
    }

    /// Wrap a POSIX path in a CFURLRef (file URL).
    unsafe fn create_cf_url_from_path(path: &str) -> Option<*const c_void> {
        let url = unsafe {
            CFURLCreateFromFileSystemRepresentation(
                std::ptr::null(),
                path.as_ptr(),
                path.len() as isize,
                1, // .app is a directory
            )
        };
        if url.is_null() {
            None
        } else {
            Some(url)
        }
    }

    unsafe fn cf_string_to_string(cf: *const c_void) -> Option<String> {
        let mut buf = [0u8; 512];
        let ok = unsafe {
            CFStringGetCString(cf, buf.as_mut_ptr(), buf.len() as isize, CF_STRING_ENCODING_UTF8)
        };
        if ok == 0 {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        let s = String::from_utf8_lossy(&buf[..end]).into_owned();
        (!s.is_empty()).then_some(s)
    }

    /// Human-facing labels of processes other than Always (and always-on
    /// system listeners) that are running audio input right now. Empty =
    /// no call / no foreign capture in progress.
    pub fn other_input_captors() -> Result<Vec<String>> {
        let address = addr(PROP_PROCESS_OBJECT_LIST);
        let mut size: u32 = 0;
        let status = unsafe {
            AudioObjectGetPropertyDataSize(SYSTEM_OBJECT, &address, 0, std::ptr::null(), &mut size)
        };
        if status != 0 {
            bail!("process object list size query failed (status {status}) — macOS < 14.4?");
        }
        let count = size as usize / std::mem::size_of::<u32>();
        let mut objects = vec![0u32; count];
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                objects.as_mut_ptr().cast(),
            )
        };
        if status != 0 {
            bail!("process object list query failed (status {status})");
        }
        objects.truncate(size as usize / std::mem::size_of::<u32>());

        let own_pid = std::process::id() as i32;
        let mut captors = Vec::new();
        for object in objects {
            if get_u32(object, PROP_PROCESS_IS_RUNNING_INPUT) != Some(1) {
                continue;
            }
            let pid = get_pid(object).unwrap_or(-1);
            if pid == own_pid {
                continue;
            }
            let basename = pid_basename(pid);
            if let Some(name) = &basename
                && HELPER_BASENAMES.contains(&name.to_lowercase().as_str())
            {
                continue;
            }
            let bundle = get_bundle_id(object);
            if let Some(b) = &bundle
                && (SYSTEM_LISTENER_BUNDLES
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(b))
                    || METERING_ONLY_BUNDLES
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(b)))
            {
                continue;
            }
            let label = if let Some(b) = &bundle {
                bundle_display_name(b)
            } else {
                basename.unwrap_or_else(|| format!("pid {pid}"))
            };
            if !captors.contains(&label) {
                captors.push(label);
            }
        }
        Ok(captors)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn fourcc_matches_sdk_constants() {
            assert_eq!(PROP_PROCESS_OBJECT_LIST, 0x7072_7323); // 'prs#'
            assert_eq!(PROP_PROCESS_PID, 0x7070_6964); // 'ppid'
            assert_eq!(PROP_PROCESS_BUNDLE_ID, 0x7062_6964); // 'pbid'
            assert_eq!(PROP_PROCESS_IS_RUNNING_INPUT, 0x7069_7269); // 'piri'
            assert_eq!(SCOPE_GLOBAL, 0x676c_6f62); // 'glob'
        }

        #[test]
        fn probe_runs_and_excludes_system_listeners() {
            // Live query against coreaudiod. Must not error on any
            // macOS >= 14.4, and always-on Siri listeners must never
            // surface as captors regardless of machine state.
            let captors = other_input_captors().expect("process-object probe failed");
            for label in &captors {
                assert!(
                    !SYSTEM_LISTENER_BUNDLES
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(label)),
                    "system listener leaked into captors: {label}"
                );
                assert!(
                    !METERING_ONLY_BUNDLES
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(label)),
                    "metering-only bundle leaked into captors: {label}"
                );
            }
        }
    }
}

// Platform-specific implementations
#[cfg(target_os = "macos")]
impl MicrophoneMonitor {
    /// CoreAudio process-object probe, with the legacy lsof scan as a
    /// fallback for macOS versions without process objects (< 14.4).
    fn check_microphone_usage_macos(&self) -> Result<bool> {
        match coreaudio_probe::other_input_captors() {
            Ok(captors) => Ok(!captors.is_empty()),
            Err(e) => {
                tracing::debug!(error = %e, "coreaudio process probe unavailable — falling back to lsof");
                let output_str = self.run_mic_lsof()?;
                Ok(!output_str.is_empty())
            }
        }
    }

    fn get_microphone_users_macos(&self) -> Result<Vec<String>> {
        if let Ok(captors) = coreaudio_probe::other_input_captors()
            && !captors.is_empty()
        {
            return Ok(captors);
        }
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
    ///
    /// Parsing is done in Rust (no `grep`/`head` pipeline): we look at the
    /// COMMAND and PID columns only, exclude our own PID and the exact helper
    /// command names (always, rec, sox, coreaudiod), and filter BEFORE bounding
    /// the count. This avoids substring false-negatives on the whole lsof line
    /// (e.g. a third-party app whose path contains "always"/"sox") and avoids
    /// `head -5` dropping a real device holder that sorts after the first lines.
    fn run_mic_lsof(&self) -> Result<String> {
        use std::process::Command;

        // Single subprocess: lsof for CoreAudio file descriptors. No shell
        // pipeline — all filtering happens in Rust below.
        let output = Command::new("lsof").args(["-c", "/coreaudio/i"]).output();

        let result = match output {
            Ok(result) => result,
            Err(_) => return Ok(String::new()),
        };

        let stdout = String::from_utf8_lossy(&result.stdout);

        // Our own pid and the helper command names we never count as "other apps".
        let own_pid = std::process::id().to_string();
        const HELPER_COMMANDS: &[&str] = &["always", "rec", "sox", "coreaudiod"];

        // lsof header: COMMAND  PID  USER  FD  TYPE  DEVICE ... — column 0 is the
        // (possibly truncated) command, column 1 is the pid. Match on those
        // columns only, by exact command name and exact pid.
        let mut matched = Vec::new();
        for line in stdout.lines() {
            let mut cols = line.split_whitespace();
            let Some(command) = cols.next() else {
                continue;
            };
            // Skip the lsof header row.
            if command == "COMMAND" {
                continue;
            }
            let Some(pid) = cols.next() else {
                continue;
            };

            // Exclude our own process and the exact helper command names.
            if pid == own_pid {
                continue;
            }
            let command_lower = command.to_lowercase();
            if HELPER_COMMANDS.contains(&command_lower.as_str()) {
                continue;
            }

            // A real third-party process holds the audio device.
            matched.push(line.trim().to_string());
            // Bound the count AFTER filtering so real holders are never dropped.
            if matched.len() >= 5 {
                break;
            }
        }

        Ok(matched.join("\n"))
    }
}

#[cfg(target_os = "linux")]
use anyhow::Context;

#[cfg(target_os = "linux")]
impl MicrophoneMonitor {
    fn check_microphone_usage_linux(&self) -> Result<bool> {
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
                // Ignore our own persistent SoX recorder; it is the
                // capture client that feeds Always.
                return Ok(output_str
                    .split("Source Output #")
                    .skip(1)
                    .any(|entry| !Self::is_own_recorder_source(entry)));
            }
            _ => {}
        }

        Ok(false)
    }

    fn check_alsa_devices(&self) -> Result<bool> {
        // Device metadata under /proc/asound proves capture hardware exists,
        // not that another client is actively recording from it.
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
                            if !Self::is_own_recorder_app(app_name) {
                                users.push(app_name.to_string());
                            }
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

    fn is_own_recorder_source(entry: &str) -> bool {
        entry.lines().any(|line| {
            line.trim()
                .strip_prefix("application.name = ")
                .and_then(|value| value.split('"').nth(1))
                .is_some_and(Self::is_own_recorder_app)
        })
    }

    fn is_own_recorder_app(app_name: &str) -> bool {
        matches!(app_name.to_ascii_lowercase().as_str(), "sox" | "rec")
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
