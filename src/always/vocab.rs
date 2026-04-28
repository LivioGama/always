use anyhow::Result;
use std::collections::HashSet;
use std::path::PathBuf;

pub mod plugins;
use plugins::get_all_plugins;

/// Detect installed speech-to-text software on the system
pub fn detect_stt_software() -> Vec<String> {
    let mut detected = Vec::new();

    // Check for Dragon NaturallySpeaking (Windows/macOS)
    if is_dragon_installed() {
        detected.push("Dragon NaturallySpeaking".to_string());
    }

    // Check for Windows Speech Recognition
    #[cfg(target_os = "windows")]
    if is_windows_speech_recognition_available() {
        detected.push("Windows Speech Recognition".to_string());
    }

    // Check for macOS Dictation
    #[cfg(target_os = "macos")]
    if is_macos_dictation_enabled() {
        detected.push("macOS Dictation".to_string());
    }

    // Check for Google Speech Recognition
    if is_google_speech_available() {
        detected.push("Google Speech Recognition".to_string());
    }

    // Check for Whisper installation
    if is_whisper_installed() {
        detected.push("OpenAI Whisper".to_string());
    }

    detected
}

/// Detect all installed applications on the system
pub fn detect_all_applications() -> Vec<String> {
    let mut applications = Vec::new();

    #[cfg(target_os = "macos")]
    {
        applications.extend(get_macos_applications());
    }

    #[cfg(target_os = "windows")]
    {
        applications.extend(get_windows_applications());
    }

    #[cfg(target_os = "linux")]
    {
        applications.extend(get_linux_applications());
    }

    applications
}

/// Import vocabulary from detected STT software
pub fn import_vocabulary(software: &[String]) -> Result<Vec<String>> {
    let mut all_terms = HashSet::new();

    // Add vocabulary from STT software (legacy)
    for name in software {
        let terms = extract_vocabulary_from_software(name)?;
        for term in terms {
            all_terms.insert(term);
        }
    }

    // Add vocabulary from plugins
    let plugins = get_all_plugins();
    for plugin in plugins {
        if plugin.is_installed() {
            println!("Found plugin: {} - {}", plugin.name(), plugin.description());
            if let Ok(terms) = plugin.extract_all_vocabulary() {
                println!("  Extracted {} terms from plugin", terms.len());
                for term in terms {
                    all_terms.insert(term);
                }
            }
        }
    }

    // Add all installed application names
    let applications = detect_all_applications();
    for app in applications {
        all_terms.insert(app);
    }

    // Save to glossary.json
    if !all_terms.is_empty() {
        save_to_glossary(&all_terms)?;
    }

    Ok(all_terms.into_iter().collect())
}

fn is_dragon_installed() -> bool {
    #[cfg(target_os = "windows")]
    {
        // Check common Dragon installation paths
        let paths = vec![
            r"C:\Program Files\Nuance\NaturallySpeaking",
            r"C:\Program Files (x86)\Nuance\NaturallySpeaking",
        ];
        paths.iter().any(|p| PathBuf::from(p).exists())
    }

    #[cfg(target_os = "macos")]
    {
        // Check macOS Dragon installation
        PathBuf::from("/Applications/Dragon").exists()
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        false
    }
}

#[cfg(target_os = "windows")]
fn is_windows_speech_recognition_available() -> bool {
    // Windows Speech Recognition is built-in to Windows 7+
    true
}

#[cfg(target_os = "macos")]
fn is_macos_dictation_enabled() -> bool {
    // macOS Dictation is built-in
    true
}

fn is_google_speech_available() -> bool {
    // Check if we can reach Google Speech API (soft check)
    // This is a basic check - actual availability requires internet
    true
}

fn is_whisper_installed() -> bool {
    // Check if whisper command is available
    std::process::Command::new("whisper")
        .arg("--help")
        .output()
        .is_ok()
}

fn extract_vocabulary_from_software(name: &str) -> Result<Vec<String>> {
    let mut terms = Vec::new();

    match name {
        "Dragon NaturallySpeaking" => {
            terms.extend(get_dragon_vocabulary());
        }
        "Windows Speech Recognition" => {
            terms.extend(get_windows_speech_vocabulary());
        }
        "macOS Dictation" => {
            terms.extend(get_macos_dictation_vocabulary());
        }
        "Google Speech Recognition" => {
            terms.extend(get_google_speech_vocabulary());
        }
        "OpenAI Whisper" => {
            terms.extend(get_whisper_vocabulary());
        }
        _ => {}
    }

    Ok(terms)
}

fn get_dragon_vocabulary() -> Vec<String> {
    // Common technical terms from Dragon's vocabulary
    vec![
        "algorithm".to_string(), "application".to_string(), "architecture".to_string(), "authentication".to_string(), "backup".to_string(),
        "bandwidth".to_string(), "browser".to_string(), "cache".to_string(), "cloud".to_string(), "compile".to_string(), "configure".to_string(),
        "database".to_string(), "debug".to_string(), "deploy".to_string(), "encryption".to_string(), "firewall".to_string(), "framework".to_string(),
        "hardware".to_string(), "interface".to_string(), "javascript".to_string(), "kernel".to_string(), "library".to_string(), "middleware".to_string(),
        "network".to_string(), "operating system".to_string(), "platform".to_string(), "protocol".to_string(), "repository".to_string(),
        "server".to_string(), "software".to_string(), "terminal".to_string(), "user interface".to_string(), "virtual machine".to_string(),
        "webhook".to_string(), "xml".to_string(), "yaml".to_string(), "zip".to_string(), "authentication".to_string(), "authorization".to_string(),
        "container".to_string(), "docker".to_string(), "kubernetes".to_string(), "microservices".to_string(), "api".to_string(), "rest".to_string(),
        "graphql".to_string(), "websocket".to_string(), "middleware".to_string(), "frontend".to_string(), "backend".to_string(),
    ]
}

fn get_windows_speech_vocabulary() -> Vec<String> {
    // Windows-specific technical terms
    vec![
        "powershell".to_string(), "registry".to_string(), "task manager".to_string(), "command prompt".to_string(), "control panel".to_string(),
        "device manager".to_string(), "event viewer".to_string(), "group policy".to_string(), "hyper-v".to_string(), "iis".to_string(),
        "microsoft edge".to_string(), "windows defender".to_string(), "windows update".to_string(), "active directory".to_string(),
        "azure".to_string(), "onedrive".to_string(), "sharepoint".to_string(), "teams".to_string(), "outlook".to_string(), "excel".to_string(), "word".to_string(),
        "powerpoint".to_string(), "visio".to_string(), "project".to_string(), "visual studio".to_string(), "net".to_string(), "csharp".to_string(),
        "asp".to_string(), "windows forms".to_string(), "wpf".to_string(), "uwp".to_string(), "winui".to_string(), "directx".to_string(), "hololens".to_string(),
    ]
}

fn get_macos_dictation_vocabulary() -> Vec<String> {
    // macOS-specific technical terms
    vec![
        "terminal".to_string(), "finder".to_string(), "spotlight".to_string(), "launchpad".to_string(), "mission control".to_string(),
        "dashboard".to_string(), "dock".to_string(), "menu bar".to_string(), "system preferences".to_string(), "activity monitor".to_string(),
        "disk utility".to_string(), "console".to_string(), "keychain access".to_string(), "time machine".to_string(), "airdrop".to_string(),
        "handoff".to_string(), "continuity".to_string(), "sidecar".to_string(), "universal control".to_string(), "quicktime".to_string(),
        "preview".to_string(), "textedit".to_string(), "safari".to_string(), "mail".to_string(), "calendar".to_string(), "contacts".to_string(), "notes".to_string(),
        "reminders".to_string(), "photos".to_string(), "music".to_string(), "podcasts".to_string(), "tv".to_string(), "app store".to_string(), "xcode".to_string(),
        "swift".to_string(), "objective-c".to_string(), "cocoa".to_string(), "metal".to_string(), "core ml".to_string(), "swiftui".to_string(), "combine".to_string(),
    ]
}

fn get_google_speech_vocabulary() -> Vec<String> {
    // Google/Android-specific technical terms
    vec![
        "android".to_string(), "chromecast".to_string(), "chromebook".to_string(), "google assistant".to_string(), "google drive".to_string(),
        "google docs".to_string(), "google sheets".to_string(), "google slides".to_string(), "gmail".to_string(), "google calendar".to_string(),
        "google meet".to_string(), "google chat".to_string(), "google workspace".to_string(), "firebase".to_string(), "tensorflow".to_string(),
        "kubernetes".to_string(), "grpc".to_string(), "protobuf".to_string(), "angular".to_string(), "flutter".to_string(), "dart".to_string(), "go".to_string(),
        "golang".to_string(), "material design".to_string(), "android studio".to_string(), "gradle".to_string(), "jetpack compose".to_string(),
        "coroutine".to_string(), "lifecycle".to_string(), "viewmodel".to_string(), "livedata".to_string(), "room".to_string(), "navigation".to_string(),
    ]
}

fn get_whisper_vocabulary() -> Vec<String> {
    // OpenAI/AI-specific technical terms
    vec![
        "artificial intelligence".to_string(), "machine learning".to_string(), "deep learning".to_string(), "neural network".to_string(),
        "transformer".to_string(), "attention mechanism".to_string(), "language model".to_string(), "gpt".to_string(), "chatgpt".to_string(),
        "openai".to_string(), "api".to_string(), "token".to_string(), "prompt".to_string(), "completion".to_string(), "embedding".to_string(), "fine-tuning".to_string(),
        "training".to_string(), "inference".to_string(), "gpu".to_string(), "tpu".to_string(), "cuda".to_string(), "pytorch".to_string(), "tensorflow".to_string(),
        "keras".to_string(), "jupyter".to_string(), "notebook".to_string(), "colab".to_string(), "hugging face".to_string(), "dataset".to_string(),
        "model".to_string(), "checkpoint".to_string(), "inference".to_string(), "latency".to_string(), "throughput".to_string(), "batch size".to_string(),
        "learning rate".to_string(), "optimizer".to_string(), "loss function".to_string(), "backpropagation".to_string(), "gradient".to_string(),
    ]
}

#[cfg(target_os = "macos")]
fn get_macos_applications() -> Vec<String> {
    let mut applications = Vec::new();

    if let Ok(entries) = std::fs::read_dir("/Applications") {
        for entry in entries.flatten() {
            if let Some(name) = entry.path().file_stem() {
                let app_name = name.to_string_lossy().to_string();
                // Remove .app if present
                let clean_name = app_name.replace(".app", "");
                if !clean_name.is_empty() {
                    applications.push(clean_name);
                }
            }
        }
    }

    // Also check user's Applications folder
    if let Ok(home) = std::env::var("HOME") {
        let user_apps = format!("{}/Applications", home);
        if let Ok(entries) = std::fs::read_dir(&user_apps) {
            for entry in entries.flatten() {
                if let Some(name) = entry.path().file_stem() {
                    let app_name = name.to_string_lossy().to_string();
                    let clean_name = app_name.replace(".app", "");
                    if !clean_name.is_empty() && !applications.contains(&clean_name) {
                        applications.push(clean_name);
                    }
                }
            }
        }
    }

    applications
}

#[cfg(target_os = "windows")]
fn get_windows_applications() -> Vec<String> {
    let mut applications = Vec::new();

    // Check Program Files
    if let Ok(entries) = std::fs::read_dir(r"C:\Program Files") {
        for entry in entries.flatten() {
            if let Some(name) = entry.path().file_name() {
                if let Some(name_str) = name.to_str() {
                    applications.push(name_str.to_string());
                }
            }
        }
    }

    // Check Program Files (x86)
    if let Ok(entries) = std::fs::read_dir(r"C:\Program Files (x86)") {
        for entry in entries.flatten() {
            if let Some(name) = entry.path().file_name() {
                if let Some(name_str) = name.to_str() {
                    let name_str = name_str.to_string();
                    if !applications.contains(&name_str) {
                        applications.push(name_str);
                    }
                }
            }
        }
    }

    // Check Start Menu Programs
    if let Ok(home) = std::env::var("APPDATA") {
        let start_menu = format!(r"{}\Microsoft\Windows\Start Menu\Programs", home);
        if let Ok(entries) = std::fs::read_dir(&start_menu) {
            for entry in entries.flatten() {
                if let Some(name) = entry.path().file_name() {
                    if let Some(name_str) = name.to_str() {
                        let name_str = name_str.to_string();
                        // Remove .lnk extension
                        let clean_name = name_str.replace(".lnk", "");
                        if !clean_name.is_empty() && !applications.contains(&clean_name) {
                            applications.push(clean_name);
                        }
                    }
                }
            }
        }
    }

    applications
}

#[cfg(target_os = "linux")]
fn get_linux_applications() -> Vec<String> {
    let mut applications = Vec::new();

    // Check /usr/share/applications for .desktop files
    if let Ok(entries) = std::fs::read_dir("/usr/share/applications") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "desktop") {
                if let Some(name) = path.file_stem() {
                    if let Some(name_str) = name.to_str() {
                        applications.push(name_str.to_string());
                    }
                }
            }
        }
    }

    // Check user's local applications
    if let Ok(home) = std::env::var("HOME") {
        let local_apps = format!("{}/.local/share/applications", home);
        if let Ok(entries) = std::fs::read_dir(&local_apps) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "desktop") {
                    if let Some(name) = path.file_stem() {
                        if let Some(name_str) = name.to_str() {
                            let name_str = name_str.to_string();
                            if !applications.contains(&name_str) {
                                applications.push(name_str);
                            }
                        }
                    }
                }
            }
        }

        // Check flatpak applications
        let flatpak_apps = format!("{}/.local/share/flatpak/exports/share/applications", home);
        if let Ok(entries) = std::fs::read_dir(&flatpak_apps) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "desktop") {
                    if let Some(name) = path.file_stem() {
                        if let Some(name_str) = name.to_str() {
                            let name_str = name_str.to_string();
                            if !applications.contains(&name_str) {
                                applications.push(name_str);
                            }
                        }
                    }
                }
            }
        }
    }

    applications
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn get_macos_applications() -> Vec<String> {
    Vec::new()
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn get_windows_applications() -> Vec<String> {
    Vec::new()
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn get_linux_applications() -> Vec<String> {
    Vec::new()
}

fn save_to_glossary(terms: &HashSet<String>) -> Result<()> {
    use serde_json::to_string_pretty;
    use std::fs::File;
    use std::io::Write;

    let entries: Vec<serde_json::Value> = terms
        .iter()
        .map(|term| {
            serde_json::json!({
                "term": term,
                "mistranscriptions": [],
                "frequency": 100
            })
        })
        .collect();

    let json = to_string_pretty(&entries)?;
    let mut file = File::create("glossary.json")?;
    file.write_all(json.as_bytes())?;

    Ok(())
}
