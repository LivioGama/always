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
//! | `SuperwhisperPlugin` | SQLite recordings DB at `~/Library/Application Support/superwhisper/database/superwhisper.sqlite` |
//! | `MacWhisperPlugin` | Sandboxed preferences plist + Replacements file |
//! | `WisprflowPlugin` | Detection only (their dictionary is cloud-hosted) |

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

pub struct SuperwhisperPlugin;

impl SuperwhisperPlugin {
    fn db_path() -> Option<PathBuf> {
        std::env::var("HOME").ok().map(|home| {
            PathBuf::from(home)
                .join("Library/Application Support/superwhisper/database/superwhisper.sqlite")
        })
    }
}

impl VocabPlugin for SuperwhisperPlugin {
    fn name(&self) -> &str {
        "Superwhisper"
    }

    fn description(&self) -> &str {
        "Reads SuperWhisper's local recording database to mine the user's actual transcribed vocabulary"
    }

    fn get_installation_paths(&self) -> Vec<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            vec![PathBuf::from("/Applications/superwhisper.app")]
        }
        #[cfg(not(target_os = "macos"))]
        {
            vec![]
        }
    }

    fn is_installed(&self) -> bool {
        self.get_installation_paths().iter().any(|p| p.exists())
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

        // The recording_fts virtual table mirrors `recording`. Pulling
        // `result` (the final transcript) avoids reading raw audio
        // metadata. Cap at 5000 rows so a multi-year history doesn't
        // OOM the daemon during import.
        let mut stmt = match conn
            .prepare("SELECT result FROM recording_fts WHERE result IS NOT NULL LIMIT 5000")
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "superwhisper sqlite prepare failed");
                return Ok(Vec::new());
            }
        };
        let mut corpus = String::new();
        let rows = stmt.query_map([], |row| row.get::<_, Option<String>>(0));
        if let Ok(iter) = rows {
            for row in iter.flatten().flatten() {
                corpus.push_str(&row);
                corpus.push(' ');
            }
        }

        Ok(extract_interesting_words(&corpus))
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

/// Wispr Flow is an Electron app whose user dictionary lives server-side.
/// We can detect installation but local extraction is not feasible
/// without authenticating against their cloud, which we won't do.
pub struct WisprflowPlugin;

impl VocabPlugin for WisprflowPlugin {
    fn name(&self) -> &str {
        "Wispr Flow"
    }

    fn description(&self) -> &str {
        "Detection only — Wispr Flow stores the user's custom dictionary in their cloud"
    }

    fn get_installation_paths(&self) -> Vec<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            vec![PathBuf::from("/Applications/Wispr Flow.app")]
        }
        #[cfg(not(target_os = "macos"))]
        {
            Vec::new()
        }
    }

    fn is_installed(&self) -> bool {
        self.get_installation_paths().iter().any(|p| p.exists())
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
}
