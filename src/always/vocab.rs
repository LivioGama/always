use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub mod plugins;
use plugins::{VocabPlugin, get_all_plugins};

/// Provenance recorded on every glossary entry this module creates
/// (SPEC §9b `provenance: "import"`; mirrors `correction::Provenance::Import`).
const IMPORT_PROVENANCE: &str = "import";

/// Starting `frequency` of an imported glossary entry.
const IMPORT_FREQUENCY: i64 = 100;

/// File (in the per-instance config dir) whose existence means the
/// first-launch import has already run. See [`spawn_first_launch_import`].
const AUTO_IMPORT_MARKER: &str = "vocab_auto_import.json";

/// What one import run found, as **counts only**. Term text is deliberately
/// absent: Wispr Flow / SuperWhisper words are the user's own data and must
/// never reach logs.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// Installed `.app` bundle names.
    pub app_names: usize,
    /// Words read from Wispr Flow's local dictionary.
    pub wispr_flow: usize,
    /// Words mined from SuperWhisper's recent transcripts.
    pub superwhisper: usize,
    /// macOS Text Replacements (the `with` side).
    pub text_replacements: usize,
    /// MacWhisper Replacements.
    pub macwhisper: usize,
    /// Anything else (legacy Dragon seed list).
    pub other: usize,
    /// Distinct terms gathered across every source.
    pub unique_terms: usize,
    /// Brand-new glossary entries written (terms already present, in any
    /// letter case, are left untouched and not counted).
    pub added: usize,
}

/// The result of gathering terms from every source.
struct Gathered {
    /// Sorted, trimmed, exact-deduplicated.
    terms: Vec<String>,
    report: ImportReport,
}

/// Which counter a plugin's terms belong to (keyed on `VocabPlugin::name`).
fn count_for_plugin<'a>(report: &'a mut ImportReport, plugin_name: &str) -> &'a mut usize {
    match plugin_name {
        "Wispr Flow" => &mut report.wispr_flow,
        "Superwhisper" => &mut report.superwhisper,
        "macOS Text Replacements" => &mut report.text_replacements,
        "MacWhisper" => &mut report.macwhisper,
        _ => &mut report.other,
    }
}

fn clean_terms(raw: Vec<String>) -> BTreeSet<String> {
    raw.into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Collect terms from legacy STT software, every installed plugin, and the
/// installed-application names, counting each source separately.
fn gather_terms(
    software: &[String],
    plugins: &[Box<dyn VocabPlugin>],
    applications: Vec<String>,
) -> Result<Gathered> {
    let mut all = BTreeSet::new();
    let mut report = ImportReport::default();

    // Legacy STT software (Dragon).
    for name in software {
        let terms = clean_terms(extract_vocabulary_from_software(name)?);
        report.other += terms.len();
        all.extend(terms);
    }

    // Plugins (macOS Text Replacements, SuperWhisper, MacWhisper, Wispr Flow).
    for plugin in plugins {
        if !plugin.is_installed() {
            continue;
        }
        tracing::debug!(
            plugin_name = %plugin.name(),
            plugin_description = %plugin.description(),
            "found plugin"
        );
        if let Ok(terms) = plugin.extract_all_vocabulary() {
            let terms = clean_terms(terms);
            tracing::debug!(
                plugin_name = %plugin.name(),
                term_count = terms.len(),
                "extracted terms from plugin"
            );
            *count_for_plugin(&mut report, plugin.name()) += terms.len();
            all.extend(terms);
        }
    }

    // Installed application names.
    let apps = clean_terms(applications);
    report.app_names = apps.len();
    all.extend(apps);

    report.unique_terms = all.len();
    Ok(Gathered {
        terms: all.into_iter().collect(),
        report,
    })
}

/// Terms plus the per-source report of one import.
pub struct Imported {
    pub terms: Vec<String>,
    pub report: ImportReport,
}

/// The one import code path, shared by `always vocab import` and the
/// first-launch auto-import: gather from every source, then merge the
/// new terms into `~/.always/glossary.json` with `import` provenance.
pub fn run_import(software: &[String]) -> Result<Imported> {
    let Gathered { terms, mut report } =
        gather_terms(software, &get_all_plugins(), detect_all_applications())?;
    if !terms.is_empty() {
        report.added = save_to_glossary(&terms)?;
    }
    Ok(Imported { terms, report })
}

/// Import vocabulary from detected STT software and every plugin.
/// Returns every distinct term gathered (not only the newly added ones).
pub fn import_vocabulary(software: &[String]) -> Result<Vec<String>> {
    Ok(run_import(software)?.terms)
}

// ---------------------------------------------------------------------------
// First-launch auto-import
// ---------------------------------------------------------------------------

/// Where the "already auto-imported" marker lives. Per instance
/// (`config::config_dir()` is), so the dev and prod apps each import once.
pub fn auto_import_marker_path() -> PathBuf {
    crate::config::config_dir().join(AUTO_IMPORT_MARKER)
}

/// Auto-import only knows macOS sources today (installed `.app` bundles,
/// Text Replacements, Wispr Flow, SuperWhisper, MacWhisper). Elsewhere it is
/// a deliberate no-op rather than importing `Program Files` / `.desktop`
/// noise; `always vocab import` still works everywhere on request.
const fn auto_import_supported() -> bool {
    cfg!(target_os = "macos")
}

/// On the first daemon start (marker absent) import the user's installed
/// app names, macOS Text Replacements, Wispr Flow dictionary and SuperWhisper
/// vocabulary into the glossary — once, in a background thread so audio and
/// the UDS server never wait for it. A later start finds the marker and does
/// nothing; `always vocab import` is the manual re-run.
///
/// Imported terms apply immediately: the glossary cache is invalidated after
/// the write, and the next access reloads from disk (see `glossary.rs`).
pub fn spawn_first_launch_import() {
    if !auto_import_supported() {
        return;
    }
    let marker = auto_import_marker_path();
    if marker.exists() {
        tracing::debug!("vocab_auto_import_skipped_already_done");
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("vocab-auto-import".into())
        .spawn(move || {
            let _ = run_first_launch_import(&marker, || {
                run_import(&detect_stt_software()).map(|imported| imported.report)
            });
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "vocab_auto_import_spawn_failed");
    }
}

/// Run `import` unless `marker` exists; on success write the marker and log
/// the `vocab_auto_import` event. Returns the report when an import ran.
///
/// A failed import leaves no marker, so the next launch tries again.
fn run_first_launch_import(
    marker: &Path,
    import: impl FnOnce() -> Result<ImportReport>,
) -> Option<ImportReport> {
    if marker.exists() {
        return None;
    }
    let report = match import() {
        Ok(report) => report,
        Err(e) => {
            tracing::warn!(error = %e, "vocab_auto_import_failed");
            return None;
        }
    };
    // Counts only; term text never reaches the log (user data).
    tracing::info!(
        app_names = report.app_names,
        wispr_flow = report.wispr_flow,
        superwhisper = report.superwhisper,
        text_replacements = report.text_replacements,
        macwhisper = report.macwhisper,
        other = report.other,
        unique_terms = report.unique_terms,
        added = report.added,
        "vocab_auto_import"
    );
    if let Err(e) = write_marker(marker, &report) {
        // Worst case the import repeats next launch; it only adds terms
        // that are missing, so repeating is harmless.
        tracing::warn!(error = %e, "vocab_auto_import_marker_failed");
    }
    Some(report)
}

fn write_marker(marker: &Path, report: &ImportReport) -> Result<()> {
    let completed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let body = serde_json::to_string_pretty(&serde_json::json!({
        "version": 1,
        "completed_at_unix": completed_at,
        "added": report.added,
    }))?;
    write_atomic(marker, body.as_bytes())
}

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

fn is_dragon_installed() -> bool {
    #[cfg(target_os = "windows")]
    {
        // Check common Dragon installation paths
        let paths = vec![
            r"C:\Program Files\Nuance\NaturallySpeaking",
            r"C:\Program Files (x86)\Nuance\NaturallySpeaking",
        ];
        paths.iter().any(|p| std::path::PathBuf::from(p).exists())
    }

    #[cfg(target_os = "macos")]
    {
        // Check macOS Dragon installation
        std::path::PathBuf::from("/Applications/Dragon").exists()
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

/// The name of an application bundle from a directory entry's file name:
/// `"Safari.app"` -> `Some("Safari")`. Anything that is not a `.app` bundle
/// (`.DS_Store`, `.localized`, the `Utilities` folder, `Chrome Apps.localized`)
/// and hidden or empty names give `None`.
#[cfg(any(target_os = "macos", test))]
fn app_bundle_name(file_name: &str) -> Option<String> {
    let stem = file_name.strip_suffix(".app")?.trim();
    if stem.is_empty() || stem.starts_with('.') {
        return None;
    }
    Some(stem.to_string())
}

/// Names of the `.app` bundles directly inside `dir` (missing dir -> none).
#[cfg(any(target_os = "macos", test))]
fn app_bundle_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| app_bundle_name(&entry.file_name().to_string_lossy()))
        .collect()
}

#[cfg(target_os = "macos")]
fn get_macos_applications() -> Vec<String> {
    let mut directories = vec![PathBuf::from("/Applications")];
    if let Some(home) = dirs::home_dir() {
        directories.push(home.join("Applications"));
    }

    let mut applications = Vec::new();
    for directory in directories {
        for name in app_bundle_names(&directory) {
            if !applications.contains(&name) {
                applications.push(name);
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
            if path.extension().is_some_and(|ext| ext == "desktop")
                && let Some(name) = path.file_stem()
                && let Some(name_str) = name.to_str()
            {
                applications.push(name_str.to_string());
            }
        }
    }

    // Check user's local applications
    if let Ok(home) = std::env::var("HOME") {
        let local_apps = format!("{}/.local/share/applications", home);
        if let Ok(entries) = std::fs::read_dir(&local_apps) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "desktop")
                    && let Some(name) = path.file_stem()
                    && let Some(name_str) = name.to_str()
                {
                    let name_str = name_str.to_string();
                    if !applications.contains(&name_str) {
                        applications.push(name_str);
                    }
                }
            }
        }

        // Check flatpak applications
        let flatpak_apps = format!("{}/.local/share/flatpak/exports/share/applications", home);
        if let Ok(entries) = std::fs::read_dir(&flatpak_apps) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "desktop")
                    && let Some(name) = path.file_stem()
                    && let Some(name_str) = name.to_str()
                {
                    let name_str = name_str.to_string();
                    if !applications.contains(&name_str) {
                        applications.push(name_str);
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
/// `~/.always/glossary.json`; returns how many entries were added.
fn save_to_glossary(terms: &[String]) -> Result<usize> {
    let path = crate::glossary::user_glossary_path()
        .unwrap_or_else(|| std::path::PathBuf::from("glossary.json"));
    let added = merge_into_glossary(&path, terms)?;
    if added > 0 {
        // Same process as the daemon's glossary reader: drop the cache so
        // the new terms apply on the next access, no restart needed. (A
        // separate CLI process is picked up by the daemon's mtime check.)
        crate::glossary::invalidate_cache();
    }
    Ok(added)
}

/// Append every term in `terms` that the glossary at `path` does not already
/// contain (compared case-insensitively) as a brand-new entry with `import`
/// provenance. Existing entries are preserved untouched, so a user's
/// hand-tuned `mistranscriptions` / `frequency` / `weight` survive every
/// import. Returns the number of entries added.
///
/// * Nothing to add -> the file is not rewritten at all.
/// * The write is atomic (temp file in the same directory, then rename), so
///   a concurrent reader or a crash never sees a half-written glossary —
///   the daemon runs this while corrections may be reading the same file.
/// * An unreadable or non-array file is an **error**, never silently
///   replaced: overwriting it would destroy the user's entries.
fn merge_into_glossary(path: &Path, terms: &[String]) -> Result<usize> {
    use serde_json::Value;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let mut entries: Vec<Value> = match std::fs::read_to_string(path) {
        Ok(content) if content.trim().is_empty() => Vec::new(),
        Ok(content) => serde_json::from_str(&content).with_context(|| {
            format!(
                "{} is not a valid glossary (expected a JSON array); leaving it untouched",
                path.display()
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };

    let mut known: std::collections::HashSet<String> = entries
        .iter()
        .filter_map(|e| e.get("term").and_then(|t| t.as_str()))
        .map(str::to_lowercase)
        .collect();

    let mut added = 0usize;
    for term in terms {
        if !known.insert(term.to_lowercase()) {
            continue;
        }
        entries.push(serde_json::json!({
            "term": term,
            "mistranscriptions": [],
            "frequency": IMPORT_FREQUENCY,
            "provenance": IMPORT_PROVENANCE,
        }));
        added += 1;
    }

    if added == 0 {
        tracing::info!(path = %path.display(), added, total = entries.len(), "glossary_merged");
        return Ok(0);
    }

    let json = serde_json::to_string_pretty(&entries)?;
    write_atomic(path, json.as_bytes())?;

    tracing::info!(
        path = %path.display(),
        added,
        total = entries.len(),
        "glossary_merged"
    );
    Ok(added)
}

/// Write `bytes` to `path` atomically: a uniquely named temp file in the
/// same directory (so the rename never crosses filesystems), fsynced, then
/// renamed over `path`. A symlinked `path` (e.g. a dotfiles-managed
/// `glossary.json`) is resolved first so the link itself is kept.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "glossary.json".into());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".{file_name}.tmp-{}-{nanos}", std::process::id()));

    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, &target)
            .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::cell::Cell;

    /// A unique, empty scratch directory (no `tempfile` dependency).
    fn scratch_dir(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "always-vocab-test-{label}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read_entries(path: &Path) -> Vec<Value> {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    struct FakePlugin {
        name: &'static str,
        installed: bool,
        terms: Vec<String>,
    }

    impl VocabPlugin for FakePlugin {
        fn name(&self) -> &str {
            self.name
        }
        fn is_installed(&self) -> bool {
            self.installed
        }
        fn get_installation_paths(&self) -> Vec<PathBuf> {
            Vec::new()
        }
        fn extract_all_vocabulary(&self) -> Result<Vec<String>> {
            Ok(self.terms.clone())
        }
    }

    fn fake(name: &'static str, installed: bool, terms: &[&str]) -> Box<dyn VocabPlugin> {
        Box::new(FakePlugin {
            name,
            installed,
            terms: strings(terms),
        })
    }

    // -- application names --------------------------------------------------

    #[test]
    fn app_bundle_name_keeps_only_real_app_bundles() {
        assert_eq!(app_bundle_name("Safari.app").as_deref(), Some("Safari"));
        assert_eq!(
            app_bundle_name("Visual Studio Code.app").as_deref(),
            Some("Visual Studio Code")
        );
        assert_eq!(app_bundle_name("Foo 2.app").as_deref(), Some("Foo 2"));
        // The junk the old importer let through.
        for junk in [
            ".DS_Store",
            ".localized",
            "Utilities",
            "Chrome Apps.localized",
            "OpenVPN Connect",
            ".hidden.app",
            ".app",
            " .app",
            "",
        ] {
            assert_eq!(app_bundle_name(junk), None, "{junk:?} must be dropped");
        }
    }

    #[test]
    fn app_bundle_names_scans_a_directory() {
        let dir = scratch_dir("apps");
        std::fs::create_dir(dir.join("Notion.app")).unwrap();
        std::fs::create_dir(dir.join("IntelliJ IDEA.app")).unwrap();
        std::fs::create_dir(dir.join("Utilities")).unwrap();
        std::fs::create_dir(dir.join("Comet Apps.localized")).unwrap();
        std::fs::write(dir.join(".DS_Store"), b"x").unwrap();
        std::fs::write(dir.join(".localized"), b"").unwrap();

        let mut names = app_bundle_names(&dir);
        names.sort();
        assert_eq!(names, vec!["IntelliJ IDEA", "Notion"]);
        assert!(app_bundle_names(&dir.join("does-not-exist")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- per-source counting ------------------------------------------------

    #[test]
    fn gather_terms_counts_each_source_separately() {
        let plugins = vec![
            fake("Wispr Flow", true, &[" Zorblax ", "Zorblax", "Frobnicate"]),
            fake("Superwhisper", true, &["pod-2024", "Notion"]),
            fake("macOS Text Replacements", true, &["On my way!"]),
            // Not installed: must contribute nothing, even with terms.
            fake("MacWhisper", false, &["a", "b", "c"]),
        ];
        let gathered = gather_terms(
            &strings(&["Dragon NaturallySpeaking"]),
            &plugins,
            strings(&["Safari", "Notion", "  "]),
        )
        .unwrap();

        let r = &gathered.report;
        assert_eq!(r.wispr_flow, 2, "trimmed + deduped");
        assert_eq!(r.superwhisper, 2);
        assert_eq!(r.text_replacements, 1);
        assert_eq!(r.macwhisper, 0, "uninstalled plugin is skipped");
        assert_eq!(r.app_names, 2, "blank app name dropped");
        assert!(r.other > 0, "legacy Dragon seed list is counted as `other`");

        // "Notion" came from two sources: counted in both, stored once.
        assert_eq!(
            gathered
                .terms
                .iter()
                .filter(|t| t.as_str() == "Notion")
                .count(),
            1
        );
        assert_eq!(r.unique_terms, gathered.terms.len());
        assert!(
            gathered.terms.windows(2).all(|w| w[0] < w[1]),
            "sorted+unique"
        );
        assert!(gathered.terms.contains(&"Zorblax".to_string()));
        assert!(!gathered.terms.contains(&"a".to_string()));
    }

    // -- glossary merge: provenance, preservation, atomicity ----------------

    #[test]
    fn merge_adds_new_entries_with_import_provenance() {
        let dir = scratch_dir("prov");
        let path = dir.join(".always").join("glossary.json");

        let added = merge_into_glossary(&path, &strings(&["Frobnicate", "Zorblax"])).unwrap();
        assert_eq!(added, 2);

        let entries = read_entries(&path);
        assert_eq!(entries.len(), 2);
        for entry in &entries {
            assert_eq!(entry["provenance"], "import");
            assert_eq!(entry["frequency"], 100);
            assert_eq!(entry["mistranscriptions"], json!([]));
        }
        let terms: Vec<&str> = entries.iter().filter_map(|e| e["term"].as_str()).collect();
        assert_eq!(terms, vec!["Frobnicate", "Zorblax"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_preserves_existing_entries_and_skips_known_terms_in_any_case() {
        let dir = scratch_dir("preserve");
        let path = dir.join("glossary.json");
        let hand_tuned = json!({
            "term": "Kubernetes",
            "mistranscriptions": ["kubernetics"],
            "frequency": 250,
            "weight": 3,
            "provenance": "hotkey"
        });
        let legacy = json!({"term": "Claude", "mistranscriptions": ["cloud"], "frequency": 100});
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!([hand_tuned, legacy])).unwrap(),
        )
        .unwrap();

        let added = merge_into_glossary(
            &path,
            &strings(&["kubernetes", "CLAUDE", "Zorblax", "Foo", "foo"]),
        )
        .unwrap();
        assert_eq!(added, 2, "Zorblax + the first of Foo/foo");

        let entries = read_entries(&path);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0], hand_tuned, "hand-tuned entry untouched");
        assert_eq!(entries[1], legacy, "entry without provenance stays without");
        assert_eq!(entries[2]["term"], "Zorblax");
        assert_eq!(entries[3]["term"], "Foo");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_with_nothing_new_does_not_rewrite_the_file() {
        let dir = scratch_dir("noop");
        let path = dir.join("glossary.json");
        merge_into_glossary(&path, &strings(&["Zorblax"])).unwrap();
        let before = std::fs::read(&path).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

        assert_eq!(
            merge_into_glossary(&path, &strings(&["zorblax"])).unwrap(),
            0
        );
        assert_eq!(merge_into_glossary(&path, &[]).unwrap(), 0);

        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_refuses_to_overwrite_a_corrupt_glossary() {
        let dir = scratch_dir("corrupt");
        let path = dir.join("glossary.json");

        std::fs::write(&path, b"{ this is not json").unwrap();
        assert!(merge_into_glossary(&path, &strings(&["Zorblax"])).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{ this is not json");

        std::fs::write(&path, br#"{"term": "not an array"}"#).unwrap();
        assert!(merge_into_glossary(&path, &strings(&["Zorblax"])).is_err());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            br#"{"term": "not an array"}"#
        );

        // An empty file is a fresh glossary, not corruption.
        std::fs::write(&path, b"  \n").unwrap();
        assert_eq!(
            merge_into_glossary(&path, &strings(&["Zorblax"])).unwrap(),
            1
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_is_atomic_rename_and_leaves_no_temp_files() {
        let dir = scratch_dir("atomic");
        let path = dir.join("glossary.json");
        merge_into_glossary(&path, &strings(&["First"])).unwrap();

        #[cfg(unix)]
        let inode_before = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&path).unwrap());
        merge_into_glossary(&path, &strings(&["Second"])).unwrap();
        #[cfg(unix)]
        {
            let inode_after =
                std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&path).unwrap());
            assert_ne!(
                inode_before, inode_after,
                "a rename replaces the inode; an in-place truncate+write would not"
            );
        }

        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "glossary.json")
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
        assert_eq!(read_entries(&path).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn write_keeps_a_symlinked_glossary_a_symlink() {
        let dir = scratch_dir("symlink");
        let real = dir.join("dotfiles-glossary.json");
        let link = dir.join("glossary.json");
        std::fs::write(&real, "[]").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert_eq!(
            merge_into_glossary(&link, &strings(&["Zorblax"])).unwrap(),
            1
        );

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link must survive"
        );
        assert_eq!(read_entries(&real).len(), 1, "the real file got the write");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_concurrent_reader_never_sees_a_partial_glossary() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = scratch_dir("concurrent");
        let path = dir.join("glossary.json");
        merge_into_glossary(&path, &strings(&["Seed"])).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let (path, stop) = (path.clone(), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut reads = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    let raw = std::fs::read_to_string(&path).expect("file always present");
                    serde_json::from_str::<Vec<Value>>(&raw)
                        .unwrap_or_else(|e| panic!("partial glossary seen ({e}): {raw:?}"));
                    reads += 1;
                }
                reads
            })
        };
        for i in 0..150 {
            merge_into_glossary(&path, &[format!("Term{i}")]).unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        let reads = reader.join().unwrap();
        assert!(reads > 0);
        assert_eq!(read_entries(&path).len(), 151);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- first-launch marker ------------------------------------------------

    #[test]
    fn first_launch_import_runs_once_then_never_again() {
        let dir = scratch_dir("marker");
        let marker = dir.join(AUTO_IMPORT_MARKER);
        let calls = Cell::new(0usize);
        let report = ImportReport {
            app_names: 3,
            wispr_flow: 2,
            unique_terms: 5,
            added: 4,
            ..ImportReport::default()
        };

        assert!(!marker.exists());
        let first = run_first_launch_import(&marker, || {
            calls.set(calls.get() + 1);
            Ok(report.clone())
        });
        assert_eq!(first, Some(report.clone()));
        assert_eq!(calls.get(), 1);
        assert!(
            marker.exists(),
            "marker is written after a successful import"
        );

        // Second and third launch: marker present -> importer never invoked.
        for _ in 0..2 {
            let again = run_first_launch_import(&marker, || {
                calls.set(calls.get() + 1);
                Ok(report.clone())
            });
            assert_eq!(again, None);
        }
        assert_eq!(calls.get(), 1, "import must not run a second time");

        // The marker records counts only.
        let body: Value = serde_json::from_str(&std::fs::read_to_string(&marker).unwrap()).unwrap();
        assert_eq!(body["version"], 1);
        assert_eq!(body["added"], 4);
        assert!(body["completed_at_unix"].as_u64().unwrap() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_first_launch_import_leaves_no_marker_so_it_retries() {
        let dir = scratch_dir("marker-fail");
        let marker = dir.join("nested").join(AUTO_IMPORT_MARKER);

        let failed = run_first_launch_import(&marker, || Err(anyhow::anyhow!("boom")));
        assert_eq!(failed, None);
        assert!(!marker.exists());

        let ok = run_first_launch_import(&marker, || Ok(ImportReport::default()));
        assert_eq!(ok, Some(ImportReport::default()));
        assert!(marker.exists(), "marker dir is created on demand");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_import_is_macos_only() {
        assert_eq!(auto_import_supported(), cfg!(target_os = "macos"));
        assert!(auto_import_marker_path().ends_with(AUTO_IMPORT_MARKER));
    }
}
