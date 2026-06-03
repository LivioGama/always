use anyhow::Result;
use std::collections::HashSet;
use std::path::PathBuf;

pub mod plugins;
use plugins::get_all_plugins;

/// Detect installed speech-to-text software on the system.
///
/// Only legacy plugins live here; the modern import path goes through
/// `vocab/plugins.rs::get_all_plugins()` which has typed, real per-source
/// extractors. Dragon stays because the user explicitly asked for it
/// even though we don't yet parse `.dvc` files (we only seed a small
/// list of common technical IT vocabulary).
pub fn detect_stt_software() -> Vec<String> {
    let mut detected = Vec::new();

    if is_dragon_installed() {
        detected.push("Dragon NaturallySpeaking".to_string());
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
            tracing::debug!(
                plugin_name = %plugin.name(),
                plugin_description = %plugin.description(),
                "found plugin"
            );
            if let Ok(terms) = plugin.extract_all_vocabulary() {
                tracing::debug!(
                    plugin_name = %plugin.name(),
                    term_count = terms.len(),
                    "extracted terms from plugin"
                );
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

fn extract_vocabulary_from_software(name: &str) -> Result<Vec<String>> {
    match name {
        "Dragon NaturallySpeaking" => Ok(get_dragon_vocabulary()),
        _ => Ok(Vec::new()),
    }
}

fn get_dragon_vocabulary() -> Vec<String> {
    // Common technical terms from Dragon's vocabulary
    vec![
        "algorithm".to_string(),
        "application".to_string(),
        "architecture".to_string(),
        "authentication".to_string(),
        "backup".to_string(),
        "bandwidth".to_string(),
        "browser".to_string(),
        "cache".to_string(),
        "cloud".to_string(),
        "compile".to_string(),
        "configure".to_string(),
        "database".to_string(),
        "debug".to_string(),
        "deploy".to_string(),
        "encryption".to_string(),
        "firewall".to_string(),
        "framework".to_string(),
        "hardware".to_string(),
        "interface".to_string(),
        "javascript".to_string(),
        "kernel".to_string(),
        "library".to_string(),
        "middleware".to_string(),
        "network".to_string(),
        "operating system".to_string(),
        "platform".to_string(),
        "protocol".to_string(),
        "repository".to_string(),
        "server".to_string(),
        "software".to_string(),
        "terminal".to_string(),
        "user interface".to_string(),
        "virtual machine".to_string(),
        "webhook".to_string(),
        "xml".to_string(),
        "yaml".to_string(),
        "zip".to_string(),
        "authentication".to_string(),
        "authorization".to_string(),
        "container".to_string(),
        "docker".to_string(),
        "kubernetes".to_string(),
        "microservices".to_string(),
        "api".to_string(),
        "rest".to_string(),
        "graphql".to_string(),
        "websocket".to_string(),
        "middleware".to_string(),
        "frontend".to_string(),
        "backend".to_string(),
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

/// Merge newly-imported `terms` into the on-disk glossary at
/// `~/.always/glossary.json`. Existing entries are preserved untouched
/// so a user's hand-tuned `mistranscriptions` / `frequency` values
/// survive every subsequent `always vocab import` run. Only brand-new
/// terms are appended.
fn save_to_glossary(terms: &HashSet<String>) -> Result<()> {
    use crate::glossary::user_glossary_path;
    use serde_json::Value;
    use serde_json::to_string_pretty;
    use std::fs::{File, create_dir_all};
    use std::io::Write;

    let path = user_glossary_path().unwrap_or_else(|| std::path::PathBuf::from("glossary.json"));
    if let Some(parent) = path.parent() {
        create_dir_all(parent).ok();
    }

    // Load existing entries (if any) so we don't clobber user edits.
    let mut existing: Vec<Value> = if path.exists() {
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| Vec::new()),
            Err(_) => Vec::new(),
        }
    } else {
        Vec::new()
    };

    let existing_terms: std::collections::HashSet<String> = existing
        .iter()
        .filter_map(|e| e.get("term").and_then(|t| t.as_str()).map(String::from))
        .collect();

    let mut added = 0usize;
    for term in terms {
        if existing_terms.contains(term) {
            continue;
        }
        existing.push(serde_json::json!({
            "term": term,
            "mistranscriptions": [],
            "frequency": 100
        }));
        added += 1;
    }

    let json = to_string_pretty(&existing)?;
    let mut file = File::create(&path)?;
    file.write_all(json.as_bytes())?;

    tracing::info!(
        path = %path.display(),
        added,
        total = existing.len(),
        "glossary_merged"
    );
    Ok(())
}
