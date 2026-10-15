//! Vocabulary import plugins.
//!
//! Each plugin describes one external speech-to-text product and how to
//! pull *real user data* out of it. We deliberately do **not** ship
//! hardcoded "starter" word lists any more — they polluted the glossary
//! and reduced Whisper's bias signal. If a plugin can't find anything
//! useful for the current user it returns an empty `Vec`, period.
//!
//! Currently shipped:
//!
//! | Plugin | Source |
//! |--------|--------|
//! | `MacosTextReplacementsPlugin` | `defaults read NSGlobalDomain NSUserDictionaryReplacementItems` |
//! | `SuperwhisperPlugin` | Vocabulary + Replacements in `<SuperWhisper folder>/superwhisper/settings/settings.json`, plus terms mined from the recordings DB at `~/Library/Application Support/superwhisper/database/superwhisper.sqlite` |
//! | `MacWhisperPlugin` | Sandboxed preferences plist + Replacements file |
//! | `WisprflowPlugin` | Local `Dictionary` table in `~/Library/Application Support/Wispr Flow/flow.sqlite` |

use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Configuration for a vocabulary plugin
#[derive(Default)]
pub struct PluginConfig {
    pub install_path: Option<PathBuf>,
    pub config_paths: Vec<PathBuf>,
    pub data_paths: Vec<PathBuf>,
    pub api_endpoints: Vec<String>,
    pub metadata: HashMap<String, String>,
}

/// Trait for vocabulary import plugins.
pub trait VocabPlugin {
    fn name(&self) -> &str;

    fn description(&self) -> &str {
        "No description provided"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn get_config(&self) -> PluginConfig {
        PluginConfig::default()
    }

    fn is_installed(&self) -> bool;

    fn is_installed_at(&self, path: &Path) -> bool {
        path.exists()
    }

    fn get_installation_paths(&self) -> Vec<PathBuf>;

    /// Hardcoded "starter pack" terms. Default: empty. Plugins should
    /// only override this for terms the user provably uses (e.g. the
    /// product's own name); never for generic IT vocabulary.
    fn extract_vocabulary(&self) -> Vec<String> {
        Vec::new()
    }

    fn extract_vocabulary_from_path(&self, _path: &Path) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn extract_vocabulary_from_config(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn extract_vocabulary_from_data(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn extract_vocabulary_from_api(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Combine every source. Sorts by frequency, takes the top 200.
    fn extract_all_vocabulary(&self) -> Result<Vec<String>> {
        let mut all_terms = Vec::new();

        all_terms.extend(self.extract_vocabulary());
        if let Ok(t) = self.extract_vocabulary_from_config() {
            all_terms.extend(t);
        }
        if let Ok(t) = self.extract_vocabulary_from_data() {
            all_terms.extend(t);
        }
        if let Ok(t) = self.extract_vocabulary_from_api() {
            all_terms.extend(t);
        }

        let mut counts: HashMap<String, usize> = HashMap::new();
        for term in &all_terms {
            *counts.entry(term.clone()).or_insert(0) += 1;
        }
        let mut sorted: Vec<(String, usize)> = counts.into_iter().collect();
        sorted.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        Ok(sorted.into_iter().take(200).map(|(t, _)| t).collect())
    }
}

/// Registry of every shipped plugin. Order matters only insofar as
/// later plugins' duplicate terms are deduped by frequency tally.
pub fn get_all_plugins() -> Vec<Box<dyn VocabPlugin>> {
    vec![
        Box::new(MacosTextReplacementsPlugin),
        Box::new(SuperwhisperPlugin),
        Box::new(MacWhisperPlugin),
        Box::new(WisprflowPlugin),
    ]
}

// ============================================================================
// macOS Text Replacements (System-wide UserDictionary)
// ============================================================================

/// Reads macOS-wide text replacements (System Settings → Keyboard → Text
/// Replacements). The user's `replace`/`with` pairs end up in
/// `NSGlobalDomain` as `NSUserDictionaryReplacementItems`. The
/// **`with`** field is the actual word/phrase the user wants typed —
/// that's what we add to the glossary so Whisper can transcribe it
/// correctly.
pub struct MacosTextReplacementsPlugin;

impl VocabPlugin for MacosTextReplacementsPlugin {
    fn name(&self) -> &str {
        "macOS Text Replacements"
    }

    fn description(&self) -> &str {
        "User-defined text replacements from System Settings → Keyboard → Text Replacements"
    }

    fn is_installed(&self) -> bool {
        cfg!(target_os = "macos")
    }

    fn get_installation_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    fn extract_vocabulary_from_config(&self) -> Result<Vec<String>> {
        if !cfg!(target_os = "macos") {
            return Ok(Vec::new());
        }
        // Pipe `defaults export` into `plutil -extract … json` so we get
        // proper UTF-8 (with curly quotes, emojis, accents intact).
        // The textual `defaults read` output double-escapes Unicode and
        // we'd have to round-trip through a brittle plist-string parser
        // to recover the user's actual phrases.
        let exported = match Command::new("defaults")
            .args(["export", "NSGlobalDomain", "-"])
            .output()
        {
            Ok(o) if o.status.success() => o.stdout,
            _ => return Ok(Vec::new()),
        };
        let mut child = match Command::new("plutil")
            .args([
                "-extract",
                "NSUserDictionaryReplacementItems",
                "json",
                "-o",
                "-",
                "-",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return Ok(Vec::new()),
        };
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write as _;
            let _ = stdin.write_all(&exported);
        }
        let output = match child.wait_with_output() {
            Ok(o) if o.status.success() => o.stdout,
            _ => return Ok(Vec::new()),
        };
        let json: serde_json::Value = match serde_json::from_slice(&output) {
            Ok(j) => j,
            Err(_) => return Ok(Vec::new()),
        };
        let mut out = Vec::new();
        if let Some(arr) = json.as_array() {
            for item in arr {
                if let Some(w) = item.get("with").and_then(|v| v.as_str())
                    && !w.trim().is_empty()
                {
                    out.push(w.to_string());
                }
            }
        }
        Ok(out)
    }
}

/// Parse the `defaults read` output for `NSUserDictionaryReplacementItems`.
/// Format is the old-NeXT plist textual form, e.g.:
///
/// ```text
/// (
///         {
///         on = 1;
///         replace = omw;
///         with = "On my way!";
///     },
/// )
/// ```
///
/// We extract the `with` value of every entry — the canonical phrase
/// the user wants Whisper to transcribe to.
fn parse_user_dictionary_items(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("with = ") else {
            continue;
        };
        let raw = rest.trim_end_matches(';').trim();
        // Strip surrounding quotes if present.
        let cleaned = if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
            &raw[1..raw.len() - 1]
        } else {
            raw
        };
        // Resolve common plist escape sequences (`\\Uxxxx` → unicode char,
        // `\\n` → newline, `\\"` → `"`). Anything we don't recognize, pass through.
        let resolved = resolve_plist_escapes(cleaned);
        if !resolved.is_empty() {
            out.push(resolved);
        }
    }
    out
}

fn resolve_plist_escapes(s: &str) -> String {
    fn read_uxxxx(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<u32> {
        let mut hex = String::new();
        for _ in 0..4 {
            if let Some(h) = chars.peek().copied()
                && h.is_ascii_hexdigit()
            {
                hex.push(h);
                chars.next();
            } else {
                break;
            }
        }
        if hex.is_empty() {
            None
        } else {
            u32::from_str_radix(&hex, 16).ok()
        }
    }

    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('U') => {
                let Some(code) = read_uxxxx(&mut chars) else {
                    out.push('\\');
                    out.push('U');
                    continue;
                };
                // UTF-16 surrogate pair: high surrogate must be followed
                // by `\Uxxxx` low surrogate. The plist textual format
                // emits emojis (codepoints > U+FFFF) this way.
                if (0xD800..=0xDBFF).contains(&code) {
                    // Need a following \Uxxxx low surrogate.
                    if chars.peek() == Some(&'\\') {
                        let mut tmp = chars.clone();
                        tmp.next(); // consume `\`
                        if tmp.peek() == Some(&'U') {
                            tmp.next();
                            if let Some(low) = read_uxxxx(&mut tmp)
                                && (0xDC00..=0xDFFF).contains(&low)
                            {
                                let codepoint =
                                    0x10000 + (((code - 0xD800) << 10) | (low - 0xDC00));
                                if let Some(ch) = char::from_u32(codepoint) {
                                    chars = tmp;
                                    out.push(ch);
                                    continue;
                                }
                            }
                        }
                    }
                    // Lone high surrogate — drop it (invalid UTF-16).
                    continue;
                }
                if let Some(decoded) = char::from_u32(code) {
                    out.push(decoded);
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

// ============================================================================
// SuperWhisper
// ============================================================================

/// SuperWhisper contributes two kinds of terms:
///
/// 1. **Explicit** — what the user typed into SuperWhisper on purpose, from
///    `settings/settings.json` in SuperWhisper's data folder: the
///    `vocabulary` list (strings) and, for each entry of `replacements`
///    (`{id, original, with}`), the `with` value, mirroring how macOS Text
///    Replacements use the "with" side. These are always imported, uncapped.
/// 2. **Mined** — statistical outlier words from the most recent
///    transcripts in the recordings database (see `extract_vocabulary_from_data`).
pub struct SuperwhisperPlugin;

/// Defaults domain of the SuperWhisper app; its `appFolderDirectory` key
/// records the folder that contains the `superwhisper/` data directory.
const SUPERWHISPER_DEFAULTS_DOMAIN: &str = "com.superduper.superwhisper";

/// Cap on the mined (statistical) terms; explicit terms are never capped.
const SUPERWHISPER_MAX_MINED_TERMS: usize = 200;

impl SuperwhisperPlugin {
    fn db_path() -> Option<PathBuf> {
        std::env::var("HOME").ok().map(|home| {
            PathBuf::from(home)
                .join("Library/Application Support/superwhisper/database/superwhisper.sqlite")
        })
    }

    /// The folder the user chose for SuperWhisper's data in its settings
    /// (`defaults read com.superduper.superwhisper appFolderDirectory`),
    /// if any. SuperWhisper keeps its `superwhisper/` directory inside it.
    fn custom_app_folder() -> Option<PathBuf> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let output = Command::new("defaults")
            .args(["read", SUPERWHISPER_DEFAULTS_DOMAIN, "appFolderDirectory"])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if raw.is_empty() {
            return None;
        }
        match raw.strip_prefix("~/") {
            Some(rest) => dirs::home_dir().map(|home| home.join(rest)),
            None => Some(PathBuf::from(raw)),
        }
    }

    /// Candidate `settings.json` locations, most authoritative first: the
    /// folder recorded in SuperWhisper's preferences, then its default
    /// (`~/Documents`).
    fn settings_paths() -> Vec<PathBuf> {
        let mut roots = Vec::new();
        roots.extend(Self::custom_app_folder());
        roots.extend(dirs::home_dir().map(|home| home.join("Documents")));
        let mut paths: Vec<PathBuf> = roots
            .into_iter()
            .map(|root| root.join("superwhisper/settings/settings.json"))
            .collect();
        paths.dedup();
        paths
    }

    /// [`Self::parse_settings`] applied to the file at `path`; an unreadable
    /// file yields nothing.
    fn read_settings_file(path: &Path) -> Vec<String> {
        match std::fs::read_to_string(path) {
            Ok(json) => Self::parse_settings(&json),
            Err(e) => {
                tracing::warn!(error = %e, "superwhisper settings read failed");
                Vec::new()
            }
        }
    }

    /// Words the user explicitly gave SuperWhisper, from the text of its
    /// `settings.json`: every string in `vocabulary` plus the `with` of each
    /// entry in `replacements`. Trimmed, de-duplicated, order preserved.
    /// Malformed JSON or unexpected shapes yield whatever is usable (or
    /// nothing) rather than an error.
    fn parse_settings(json: &str) -> Vec<String> {
        let Ok(settings) = serde_json::from_str::<serde_json::Value>(json) else {
            return Vec::new();
        };
        let vocabulary = settings
            .get("vocabulary")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|item| item.as_str());
        let replacements = settings
            .get("replacements")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("with").and_then(|w| w.as_str()));

        let mut seen = std::collections::HashSet::new();
        vocabulary
            .chain(replacements)
            .map(str::trim)
            .filter(|term| !term.is_empty())
            .filter(|term| seen.insert(*term))
            .map(str::to_string)
            .collect()
    }
}

impl VocabPlugin for SuperwhisperPlugin {
    fn name(&self) -> &str {
        "Superwhisper"
    }

    fn description(&self) -> &str {
        "Reads SuperWhisper's own Vocabulary list and Replacements, and mines the user's recent transcripts for the terms they actually say"
    }

    /// The app bundle OR SuperWhisper's `settings.json`: the explicit
    /// vocabulary stays readable if the app lives somewhere unusual.
    fn get_installation_paths(&self) -> Vec<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            let mut paths = vec![PathBuf::from("/Applications/superwhisper.app")];
            paths.extend(Self::settings_paths());
            paths
        }
        #[cfg(not(target_os = "macos"))]
        {
            vec![]
        }
    }

    fn is_installed(&self) -> bool {
        self.get_installation_paths().iter().any(|p| p.exists())
    }

    /// The user's explicit SuperWhisper vocabulary and replacements (see
    /// [`Self::parse_settings`]), from the first `settings.json` that
    /// exists. Read-only; a missing or unreadable file yields nothing.
    fn extract_vocabulary_from_config(&self) -> Result<Vec<String>> {
        let Some(path) = Self::settings_paths().into_iter().find(|p| p.is_file()) else {
            return Ok(Vec::new());
        };
        Ok(Self::read_settings_file(&path))
    }

    /// Explicit terms first and uncapped, then the mined terms, capped at
    /// [`SUPERWHISPER_MAX_MINED_TERMS`] as before. (The trait default would
    /// cap everything together, letting mined words push explicit ones out.)
    fn extract_all_vocabulary(&self) -> Result<Vec<String>> {
        let mut seen = std::collections::HashSet::new();
        let mut terms = self.extract_vocabulary_from_config().unwrap_or_default();
        terms.retain(|t| seen.insert(t.clone()));

        let mut mined = self.extract_vocabulary_from_data().unwrap_or_default();
        mined.retain(|t| seen.insert(t.clone()));
        mined.truncate(SUPERWHISPER_MAX_MINED_TERMS);
        terms.extend(mined);
        Ok(terms)
    }

    /// Pull every transcript out of SuperWhisper's local SQLite DB and
    /// run statistical outlier extraction on the corpus. This covers
    /// the user's *actual* spoken vocabulary, not a hardcoded guess.
    fn extract_vocabulary_from_data(&self) -> Result<Vec<String>> {
        let Some(db) = Self::db_path() else {
            return Ok(Vec::new());
        };
        if !db.exists() {
            return Ok(Vec::new());
        }
        let conn = match rusqlite::Connection::open_with_flags(
            &db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "superwhisper sqlite open failed");
                return Ok(Vec::new());
            }
        };

        let corpus = Self::recent_transcripts_corpus(&conn, SUPERWHISPER_MAX_RECORDINGS);
        Ok(extract_interesting_words(&corpus))
    }
}

/// How many of SuperWhisper's most recent recordings are mined. Caps a
/// multi-year history so the import can't OOM the daemon.
const SUPERWHISPER_MAX_RECORDINGS: usize = 5000;

impl SuperwhisperPlugin {
    /// Concatenate the final transcripts (`result`) of the `limit` most
    /// recent recordings.
    ///
    /// The `recording_fts` virtual table mirrors `recording` and carries
    /// the transcript, but has no timestamp of its own, so recency comes
    /// from `recording.datetime` via the `recordingId` link. The `IN`
    /// subquery is materialised once, so this stays a single FTS scan
    /// (~25 ms for 8k recordings). If the schema doesn't have that shape
    /// (older/newer SuperWhisper), fall back to the FTS rowid, which is
    /// assigned in insertion order, so a larger rowid is a newer row.
    fn recent_transcripts_corpus(conn: &rusqlite::Connection, limit: usize) -> String {
        let by_datetime = "SELECT result FROM recording_fts \
             WHERE result IS NOT NULL AND recordingId IN \
             (SELECT id FROM recording ORDER BY datetime DESC LIMIT ?1)";
        let by_rowid = "SELECT result FROM recording_fts \
             WHERE result IS NOT NULL ORDER BY rowid DESC LIMIT ?1";
        let limit = limit as i64;
        for sql in [by_datetime, by_rowid] {
            let mut stmt = match conn.prepare(sql) {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!(error = %e, "superwhisper sqlite prepare failed, trying next query");
                    continue;
                }
            };
            let Ok(rows) = stmt.query_map([limit], |row| row.get::<_, Option<String>>(0)) else {
                continue;
            };
            let mut corpus = String::new();
            for row in rows.flatten().flatten() {
                corpus.push_str(&row);
                corpus.push(' ');
            }
            return corpus;
        }
        tracing::warn!("superwhisper sqlite query failed");
        String::new()
    }
}

// ============================================================================
// MacWhisper
// ============================================================================

pub struct MacWhisperPlugin;

impl MacWhisperPlugin {
    /// Search paths for MacWhisper's "Replacements" data, in priority
    /// order. MacWhisper is sandboxed (`com.dipped.MacWhisper`).
    fn replacement_paths() -> Vec<PathBuf> {
        let Ok(home) = std::env::var("HOME") else {
            return Vec::new();
        };
        let home = PathBuf::from(home);
        let container = home.join("Library/Containers/com.dipped.MacWhisper/Data");
        vec![
            container.join("Library/Application Support/MacWhisper/replacements.json"),
            container.join("Library/Application Support/MacWhisper/Replacements.json"),
            container.join("Library/Preferences/com.dipped.MacWhisper.plist"),
            // Non-sandboxed fallback, in case MacWhisper migrates off the App Store.
            home.join("Library/Application Support/MacWhisper/replacements.json"),
        ]
    }
}

impl VocabPlugin for MacWhisperPlugin {
    fn name(&self) -> &str {
        "MacWhisper"
    }

    fn description(&self) -> &str {
        "Reads MacWhisper's user-defined Replacements (find/replace pairs)"
    }

    fn get_installation_paths(&self) -> Vec<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            vec![PathBuf::from("/Applications/MacWhisper.app")]
        }
        #[cfg(not(target_os = "macos"))]
        {
            Vec::new()
        }
    }

    fn is_installed(&self) -> bool {
        self.get_installation_paths().iter().any(|p| p.exists())
    }

    fn extract_vocabulary_from_config(&self) -> Result<Vec<String>> {
        for path in Self::replacement_paths() {
            if !path.exists() {
                continue;
            }
            // JSON path: `[{"find":"...","replace":"..."}]` or
            // `[{"from":"...","to":"..."}]` depending on app version.
            // The "to/replace" value is what the user actually wants
            // typed — that's our glossary candidate.
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
                    && let Some(arr) = json.as_array()
                {
                    let mut out = Vec::new();
                    for item in arr {
                        for key in ["replace", "to", "with", "replacement"] {
                            if let Some(v) = item.get(key).and_then(|v| v.as_str())
                                && !v.is_empty()
                            {
                                out.push(v.to_string());
                            }
                        }
                    }
                    if !out.is_empty() {
                        return Ok(out);
                    }
                }
            }
            // plist path: read with `defaults`. We grep the same
            // `with`-style fields used by macOS replacements.
            if path.extension().and_then(|s| s.to_str()) == Some("plist") {
                let domain = path.with_extension("");
                if let Some(domain_str) = domain.to_str() {
                    let output = Command::new("defaults")
                        .args(["read", domain_str, "Replacements"])
                        .output();
                    if let Ok(output) = output
                        && output.status.success()
                    {
                        return Ok(parse_user_dictionary_items(&String::from_utf8_lossy(
                            &output.stdout,
                        )));
                    }
                }
            }
        }
        Ok(Vec::new())
    }
}

// ============================================================================
// Wispr Flow
// ============================================================================

/// Wispr Flow is an Electron app that syncs the user's dictionary with
/// its cloud but keeps a full local copy in `flow.sqlite`, in the
/// `Dictionary` table. We read that local copy, read-only; nothing is ever
/// sent to or fetched from Wispr's servers, and no credentials are touched.
///
/// Relevant columns (verified against a real `flow.sqlite`):
///
/// | Column | Meaning |
/// |--------|---------|
/// | `phrase` | the word as spoken / added by the user |
/// | `replacement` | optional: what Wispr types instead of `phrase` |
/// | `isDeleted` | soft delete; 1 = the user removed the word |
/// | `isSnippet` | 1 = a text-expansion snippet, not a vocabulary word |
///
/// The glossary term is `replacement` when set (the spelling the user wants
/// typed), otherwise `phrase`. Deleted entries and snippets are skipped.
pub struct WisprflowPlugin;

impl WisprflowPlugin {
    fn db_path() -> Option<PathBuf> {
        dirs::home_dir().map(|home| home.join("Library/Application Support/Wispr Flow/flow.sqlite"))
    }

    /// Read the vocabulary words out of a Wispr Flow `flow.sqlite`.
    ///
    /// Tolerant of schema drift: only `phrase` is required; `replacement`,
    /// `isDeleted` and `isSnippet` are used when present. Opened read-only
    /// with a short busy timeout so a running Wispr Flow is never blocked.
    fn read_dictionary(db: &Path) -> Result<Vec<String>> {
        use rusqlite::{Connection, OpenFlags};

        let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;

        let mut columns = std::collections::HashSet::new();
        {
            let mut info = conn.prepare("PRAGMA table_info(Dictionary)")?;
            let names = info.query_map([], |row| row.get::<_, String>(1))?;
            columns.extend(names.flatten());
        }
        if !columns.contains("phrase") {
            return Ok(Vec::new());
        }

        let replacement = if columns.contains("replacement") {
            "replacement"
        } else {
            "NULL"
        };
        let mut sql = format!("SELECT phrase, {replacement} FROM Dictionary");
        let filters: Vec<&str> = [
            ("isDeleted", "COALESCE(isDeleted, 0) = 0"),
            ("isSnippet", "COALESCE(isSnippet, 0) = 0"),
        ]
        .into_iter()
        .filter(|(col, _)| columns.contains(*col))
        .map(|(_, filter)| filter)
        .collect();
        if !filters.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&filters.join(" AND "));
        }

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })?;
        let mut out = Vec::new();
        for (phrase, replacement) in rows.flatten() {
            let pick = |s: &Option<String>| {
                s.as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            if let Some(term) = pick(&replacement).or_else(|| pick(&phrase)) {
                out.push(term);
            }
        }
        Ok(out)
    }
}

impl VocabPlugin for WisprflowPlugin {
    fn name(&self) -> &str {
        "Wispr Flow"
    }

    fn description(&self) -> &str {
        "Reads the user's Wispr Flow dictionary from its local flow.sqlite (read-only; nothing leaves the Mac)"
    }

    /// The app bundle (either Applications folder) OR the local database:
    /// the dictionary is readable whenever `flow.sqlite` exists, even if
    /// the app was installed somewhere unusual or has since been removed.
    fn get_installation_paths(&self) -> Vec<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            let mut paths = vec![PathBuf::from("/Applications/Wispr Flow.app")];
            if let Some(home) = dirs::home_dir() {
                paths.push(home.join("Applications/Wispr Flow.app"));
            }
            paths.extend(Self::db_path());
            paths
        }
        #[cfg(not(target_os = "macos"))]
        {
            Vec::new()
        }
    }

    fn is_installed(&self) -> bool {
        self.get_installation_paths().iter().any(|p| p.exists())
    }

    fn extract_vocabulary_from_data(&self) -> Result<Vec<String>> {
        let Some(db) = Self::db_path() else {
            return Ok(Vec::new());
        };
        if !db.exists() {
            return Ok(Vec::new());
        }
        match Self::read_dictionary(&db) {
            Ok(terms) => Ok(terms),
            Err(e) => {
                tracing::warn!(error = %e, "wispr flow dictionary read failed");
                Ok(Vec::new())
            }
        }
    }

    /// Every word, uncapped. The trait default keeps only the top 200 by
    /// frequency, which for a hand-curated dictionary (all weight 1) would
    /// drop words arbitrarily.
    fn extract_all_vocabulary(&self) -> Result<Vec<String>> {
        let mut seen = std::collections::HashSet::new();
        let mut terms = self.extract_vocabulary_from_data()?;
        terms.retain(|t| seen.insert(t.clone()));
        Ok(terms)
    }
}

// ============================================================================
// Shared corpus extractor
// ============================================================================

/// Extract interesting words from an arbitrary corpus. Words are
/// classified as "interesting" if they are statistical outliers
/// (Z-score > 1.5) AND have technical characteristics (digits,
/// underscores, hyphens, multiple non-alphanumeric chars), or if they
/// are rare (Z-score < -1) AND long AND capitalized.
fn extract_interesting_words(content: &str) -> Vec<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut total = 0usize;
    for word in content.split_whitespace() {
        let cleaned = word
            .trim()
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-' && c != '_')
            .to_lowercase();
        if cleaned.len() >= 3 && cleaned.len() <= 50 {
            *counts.entry(cleaned).or_insert(0) += 1;
            total += 1;
        }
    }
    if total == 0 {
        return Vec::new();
    }
    let frequencies: Vec<f64> = counts.values().map(|&c| c as f64 / total as f64).collect();
    let mean = frequencies.iter().sum::<f64>() / frequencies.len() as f64;
    let variance =
        frequencies.iter().map(|&f| (f - mean).powi(2)).sum::<f64>() / frequencies.len() as f64;
    let std_dev = variance.sqrt();

    let mut out = Vec::new();
    for (word, count) in counts {
        let rel = count as f64 / total as f64;
        let z = if std_dev > 0.0 {
            (rel - mean) / std_dev
        } else {
            0.0
        };
        let has_digits = word.chars().any(|c| c.is_numeric());
        let has_underscore = word.contains('_');
        let has_hyphen = word.contains('-');
        let multiple_special = word.chars().filter(|c| !c.is_alphanumeric()).count() > 1;
        let is_long = word.len() > 8;
        let is_capitalized = word
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false);
        let stat_outlier = z.abs() > 1.5;
        let technical = has_digits || has_underscore || has_hyphen || multiple_special;
        if (stat_outlier && technical) || (z < -1.0 && is_long && is_capitalized) {
            out.push(word);
        }
    }
    out
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_user_dictionary_items_extracts_with_field() {
        // `defaults read` encodes a literal newline inside a string as
        // `\n` (single backslash + n). A literal backslash followed by
        // n is encoded as `\\n` (double-backslash + n). The raw-string
        // literal below contains EXACTLY `\n` (2 chars) on purpose so
        // the resolver should emit a real newline.
        let sample = r#"(
        {
        on = 1;
        replace = omw;
        with = "On my way!";
    },
        {
        on = 1;
        replace = jrv;
        with = "J\U2019arrive\U00a0!";
    },
        {
        on = 1;
        replace = newline;
        with = "line1\nline2";
    }
)"#;
        let items = parse_user_dictionary_items(sample);
        assert!(items.iter().any(|w| w == "On my way!"));
        assert!(items.iter().any(|w| w.contains("J\u{2019}arrive")));
        // `\n` in the encoded form resolves to a real newline.
        assert!(
            items.iter().any(|s| s.contains('\n')),
            "expected at least one item with a newline, got {:?}",
            items
        );
    }

    #[test]
    fn parse_user_dictionary_items_skips_unrelated_lines() {
        let sample = r#"(
            { on = 1; replace = omw; with = ""; },
            { hello = world; }
        )"#;
        // Empty `with` value gets dropped.
        let items = parse_user_dictionary_items(sample);
        assert_eq!(items.len(), 0);
    }

    #[test]
    fn resolve_plist_escapes_handles_unicode() {
        assert_eq!(resolve_plist_escapes("J\\U2019arrive"), "J\u{2019}arrive");
        assert_eq!(resolve_plist_escapes("plain"), "plain");
        assert_eq!(resolve_plist_escapes("\\n"), "\n");
        assert_eq!(resolve_plist_escapes("\\\""), "\"");
    }

    #[test]
    fn resolve_plist_escapes_handles_utf16_surrogate_pairs() {
        // 👀 = U+1F440 = high surrogate U+D83D + low surrogate U+DC40.
        // The plist textual form encodes it as `\Ud83d\Udc40`.
        assert_eq!(resolve_plist_escapes("\\Ud83d\\Udc40"), "\u{1F440}");
        // Surrounded by literal text.
        assert_eq!(
            resolve_plist_escapes("see \\Ud83d\\Udc40 here"),
            "see \u{1F440} here"
        );
    }

    #[test]
    fn resolve_plist_escapes_drops_lone_surrogates() {
        // Lone high surrogate without a following low surrogate is
        // invalid UTF-16 — must be dropped, not passed through.
        assert_eq!(resolve_plist_escapes("a\\Ud83d!"), "a!");
    }

    #[test]
    fn macos_text_replacements_plugin_is_macos_only() {
        let p = MacosTextReplacementsPlugin;
        if cfg!(target_os = "macos") {
            assert!(p.is_installed());
        } else {
            assert!(!p.is_installed());
        }
    }

    #[test]
    fn extract_interesting_words_rejects_empty_input() {
        assert!(extract_interesting_words("").is_empty());
        assert!(extract_interesting_words("the the the").is_empty());
    }

    #[test]
    fn extract_interesting_words_picks_technical_outliers() {
        // Statistical outlier extraction needs enough unique words for
        // the Z-score distribution to be meaningful. Build a corpus
        // where a single technical term (`pod-2024`) repeats far more
        // often than any of the surrounding common words.
        let mut corpus = String::new();
        for _ in 0..50 {
            corpus.push_str("pod-2024 ");
        }
        for w in [
            "the", "and", "for", "but", "yet", "not", "any", "all", "one", "two", "way", "see",
            "say", "let", "use", "out", "now", "how", "got", "had",
        ] {
            corpus.push_str(w);
            corpus.push(' ');
        }
        let words = extract_interesting_words(&corpus);
        assert!(
            words.iter().any(|w| w == "pod-2024"),
            "expected pod-2024 in {:?}",
            words
        );
    }

    #[test]
    fn macwhisper_replacement_paths_includes_sandbox_and_fallback() {
        // Doesn't require MacWhisper to be installed — only checks the
        // candidate list shape.
        let paths = MacWhisperPlugin::replacement_paths();
        assert!(paths.iter().any(|p| {
            p.to_string_lossy()
                .contains("Containers/com.dipped.MacWhisper")
        }));
        assert!(
            paths
                .iter()
                .any(|p| p.to_string_lossy().ends_with("replacements.json"))
        );
    }

    #[test]
    fn all_plugins_registered() {
        let plugins = get_all_plugins();
        let names: Vec<&str> = plugins.iter().map(|p| p.name()).collect();
        assert!(names.contains(&"macOS Text Replacements"));
        assert!(names.contains(&"Superwhisper"));
        assert!(names.contains(&"MacWhisper"));
        assert!(names.contains(&"Wispr Flow"));
    }

    // -- Wispr Flow ---------------------------------------------------------

    /// A unique, empty scratch directory (no `tempfile` dependency).
    fn scratch_dir(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "always-plugins-test-{label}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The `Dictionary` table exactly as Wispr Flow's Sequelize migrations
    /// create it (copied from a real `flow.sqlite`).
    const WISPR_DICTIONARY_DDL: &str = r#"CREATE TABLE "Dictionary" (`id` VARCHAR(36) NOT NULL PRIMARY KEY, `phrase` VARCHAR(255) NOT NULL, `replacement` VARCHAR(255), `teamDictionaryId` VARCHAR(36) NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000', `lastUsed` DATETIME, `frequencyUsed` INTEGER NOT NULL DEFAULT 0, `remoteFrequencyUsed` INTEGER NOT NULL DEFAULT 0, `manualEntry` TINYINT(1) NOT NULL DEFAULT 0, `createdAt` DATETIME NOT NULL, `modifiedAt` DATETIME NOT NULL, `isDeleted` TINYINT(1) NOT NULL DEFAULT 0, `source` VARCHAR(255), `isSnippet` TINYINT(1) NOT NULL DEFAULT 0, `observedSource` VARCHAR(255), `isStarred` TINYINT(1) NOT NULL DEFAULT 0, CONSTRAINT `uq_dictionary_phrase_team_dictionary_id` UNIQUE (`phrase`, `teamDictionaryId`));"#;

    fn wispr_row(
        conn: &rusqlite::Connection,
        id: &str,
        phrase: &str,
        replacement: Option<&str>,
        is_deleted: i64,
        is_snippet: i64,
    ) {
        conn.execute(
            "INSERT INTO Dictionary (id, phrase, replacement, createdAt, modifiedAt, isDeleted, isSnippet) \
             VALUES (?1, ?2, ?3, '2026-01-01', '2026-01-01', ?4, ?5)",
            rusqlite::params![id, phrase, replacement, is_deleted, is_snippet],
        )
        .unwrap();
    }

    #[test]
    fn wispr_flow_extracts_words_skipping_deleted_and_snippets() {
        let dir = scratch_dir("wispr");
        let db = dir.join("flow.sqlite");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(WISPR_DICTIONARY_DDL).unwrap();
            wispr_row(&conn, "1", "Zorblax", None, 0, 0);
            wispr_row(&conn, "2", "  Frobnicate  ", Some(""), 0, 0);
            // A replacement pair: the spelling the user wants typed wins.
            wispr_row(&conn, "3", "gee pee tee", Some("GPT"), 0, 0);
            // Removed by the user: must not come back.
            wispr_row(&conn, "4", "Deleted Word", None, 1, 0);
            // A text-expansion snippet is not vocabulary.
            wispr_row(
                &conn,
                "5",
                "my address",
                Some("1 Fake Street, Nowhere"),
                0,
                1,
            );
            // Blank phrase and no replacement: nothing to import.
            wispr_row(&conn, "6", "   ", None, 0, 0);
        }
        let mut terms = WisprflowPlugin::read_dictionary(&db).unwrap();
        terms.sort();
        assert_eq!(terms, vec!["Frobnicate", "GPT", "Zorblax"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wispr_flow_tolerates_a_minimal_or_missing_dictionary_table() {
        let dir = scratch_dir("wispr-min");

        // Older shape: only `phrase`.
        let old = dir.join("old.sqlite");
        {
            let conn = rusqlite::Connection::open(&old).unwrap();
            conn.execute_batch(
                "CREATE TABLE Dictionary (phrase TEXT NOT NULL); \
                 INSERT INTO Dictionary VALUES ('Zorblax');",
            )
            .unwrap();
        }
        assert_eq!(
            WisprflowPlugin::read_dictionary(&old).unwrap(),
            vec!["Zorblax"]
        );

        // No Dictionary table at all: empty, not an error.
        let none = dir.join("none.sqlite");
        {
            let conn = rusqlite::Connection::open(&none).unwrap();
            conn.execute_batch("CREATE TABLE Notes (id TEXT);").unwrap();
        }
        assert!(WisprflowPlugin::read_dictionary(&none).unwrap().is_empty());

        // Not a database: an error the caller turns into "no terms".
        let junk = dir.join("junk.sqlite");
        std::fs::write(&junk, b"definitely not sqlite").unwrap();
        assert!(WisprflowPlugin::read_dictionary(&junk).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wispr_flow_is_read_only() {
        let dir = scratch_dir("wispr-ro");
        let db = dir.join("flow.sqlite");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(WISPR_DICTIONARY_DDL).unwrap();
            wispr_row(&conn, "1", "Zorblax", None, 0, 0);
        }
        let before = std::fs::read(&db).unwrap();
        WisprflowPlugin::read_dictionary(&db).unwrap();
        assert_eq!(std::fs::read(&db).unwrap(), before, "DB must be untouched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wispr_flow_is_detected_by_its_database_not_only_the_app_bundle() {
        let p = WisprflowPlugin;
        assert!(!p.description().to_lowercase().contains("detection only"));
        if cfg!(target_os = "macos") {
            let paths = p.get_installation_paths();
            assert!(
                paths.iter().any(|p| p.ends_with("Wispr Flow/flow.sqlite")),
                "DB path must count as an install signal: {paths:?}"
            );
            assert!(
                paths
                    .iter()
                    .any(|p| p.ends_with("Applications/Wispr Flow.app"))
            );
        } else {
            assert!(p.get_installation_paths().is_empty());
            assert!(!p.is_installed());
        }
    }

    // -- SuperWhisper -------------------------------------------------------

    #[test]
    fn superwhisper_picks_the_most_recent_recordings_by_datetime() {
        // FTS rows are inserted in an order that differs from recency, so a
        // plain `LIMIT` (or a rowid order) would pick the wrong two.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE recording (id TEXT PRIMARY KEY, datetime DATETIME NOT NULL); \
             CREATE VIRTUAL TABLE recording_fts USING fts5(recordingId, llmResult, rawResult, result); \
             INSERT INTO recording VALUES ('a', '2026-01-01'), ('b', '2026-03-01'), ('c', '2026-02-01'); \
             INSERT INTO recording_fts (recordingId, result) VALUES ('b', 'newest'); \
             INSERT INTO recording_fts (recordingId, result) VALUES ('a', 'oldest'); \
             INSERT INTO recording_fts (recordingId, result) VALUES ('c', 'middle');",
        )
        .unwrap();
        let corpus = SuperwhisperPlugin::recent_transcripts_corpus(&conn, 2);
        assert!(corpus.contains("newest"), "{corpus:?}");
        assert!(corpus.contains("middle"), "{corpus:?}");
        assert!(!corpus.contains("oldest"), "{corpus:?}");
    }

    #[test]
    fn superwhisper_falls_back_to_rowid_order_without_a_recording_table() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE recording_fts (recordingId TEXT, result TEXT); \
             INSERT INTO recording_fts VALUES ('1', 'first'); \
             INSERT INTO recording_fts VALUES ('2', 'second'); \
             INSERT INTO recording_fts VALUES ('3', 'third');",
        )
        .unwrap();
        let corpus = SuperwhisperPlugin::recent_transcripts_corpus(&conn, 2);
        assert!(
            corpus.contains("third") && corpus.contains("second"),
            "{corpus:?}"
        );
        assert!(!corpus.contains("first"), "{corpus:?}");
    }

    #[test]
    fn superwhisper_settings_import_vocabulary_and_the_with_side_of_replacements() {
        let json = r#"{
            "favoriteModelIDs": ["a", "b"],
            "modeKeys": ["default"],
            "vocabulary": ["Zorblax", "  Frobnicate  ", "Zorblax", "", "   "],
            "replacements": [
                {"id": "1", "original": "zorb lax", "with": "Zorblax Pro"},
                {"id": "2", "original": "frob", "with": "Frobnicate"},
                {"id": "3", "original": "empty", "with": "  "},
                {"id": "4", "original": "no with side"}
            ]
        }"#;
        let terms = SuperwhisperPlugin::parse_settings(json);
        assert_eq!(
            terms,
            vec!["Zorblax", "Frobnicate", "Zorblax Pro"],
            "vocabulary first, then replacement `with` values; trimmed and de-duplicated"
        );
        // The `original` (what the user says) is never imported.
        assert!(!terms.iter().any(|t| t == "zorb lax" || t == "frob"));
    }

    #[test]
    fn superwhisper_settings_tolerate_malformed_or_unexpected_content() {
        for junk in [
            "",
            "{ this is not json",
            "[]",
            "null",
            r#"{"vocabulary": "not a list", "replacements": 7}"#,
            r#"{"vocabulary": [1, null, {"x": 1}], "replacements": ["s", 3, {"with": 5}]}"#,
            r#"{"other": ["Zorblax"]}"#,
        ] {
            assert!(
                SuperwhisperPlugin::parse_settings(junk).is_empty(),
                "{junk:?} should yield nothing"
            );
        }
        // One bad section does not discard the good one.
        let partial = r#"{"vocabulary": ["Zorblax", 3, null], "replacements": "oops"}"#;
        assert_eq!(SuperwhisperPlugin::parse_settings(partial), vec!["Zorblax"]);
    }

    #[test]
    fn superwhisper_settings_file_missing_or_unreadable_yields_nothing() {
        let dir = scratch_dir("sw-settings");
        let good = dir.join("settings.json");
        std::fs::write(&good, r#"{"vocabulary": ["Zorblax"]}"#).unwrap();
        assert_eq!(
            SuperwhisperPlugin::read_settings_file(&good),
            vec!["Zorblax"]
        );
        assert!(SuperwhisperPlugin::read_settings_file(&dir.join("absent.json")).is_empty());
        let bad = dir.join("bad.json");
        std::fs::write(&bad, b"\xff\xfe not utf8 {").unwrap();
        assert!(SuperwhisperPlugin::read_settings_file(&bad).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn superwhisper_is_detected_by_the_app_or_its_settings_file() {
        let paths = SuperwhisperPlugin::settings_paths();
        assert!(!paths.is_empty());
        assert!(
            paths
                .iter()
                .all(|p| p.ends_with("superwhisper/settings/settings.json"))
        );
        // The default folder (~/Documents) is always a candidate.
        assert!(
            paths
                .iter()
                .any(|p| p.to_string_lossy().contains("Documents"))
        );

        let p = SuperwhisperPlugin;
        assert!(p.description().contains("Vocabulary"));
        if cfg!(target_os = "macos") {
            let install = p.get_installation_paths();
            assert!(install.iter().any(|p| p.ends_with("superwhisper.app")));
            assert!(
                install
                    .iter()
                    .any(|p| p.ends_with("superwhisper/settings/settings.json")),
                "settings.json must count as an install signal: {install:?}"
            );
        } else {
            assert!(!p.is_installed());
        }
    }

    #[test]
    fn superwhisper_query_failure_yields_an_empty_corpus() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        assert!(SuperwhisperPlugin::recent_transcripts_corpus(&conn, 10).is_empty());
    }
}
