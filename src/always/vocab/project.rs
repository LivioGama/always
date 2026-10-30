//! `always vocab extract`: read-only scan of a project for identifier-like
//! terms worth teaching the dictation engine.
//!
//! Candidates are the matches of `VocabConfig::file_patterns` in the text
//! files under the project root, kept when they pass the length bounds and
//! are not in `VocabConfig::common_words`. Nothing is written to the
//! glossary: the caller reviews the list and applies what it wants.

use crate::always::config::VocabConfig;
use anyhow::{Context, Result};
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Largest file scanned; bigger files (lockfiles, bundles) are skipped.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Directories never descended into (dependency and build output).
const SKIPPED_DIRS: &[&str] = &["target", "node_modules", "vendor", "dist", "build"];

/// Distinct candidate terms under `root` with their occurrence counts,
/// most frequent first (ties alphabetical). Hidden entries, dependency and
/// build directories, symlinks, binary and oversized files are skipped.
pub fn extract_project_terms(root: &Path, config: &VocabConfig) -> Result<Vec<(String, usize)>> {
    let patterns = config
        .file_patterns
        .iter()
        .map(|p| Regex::new(p).with_context(|| format!("invalid vocab file pattern: {p}")))
        .collect::<Result<Vec<_>>>()?;

    let mut files = Vec::new();
    collect_files(root, &mut files)?;

    let mut counts: HashMap<String, usize> = HashMap::new();
    for file in &files {
        let Some(content) = read_text(file) else {
            continue;
        };
        for pattern in &patterns {
            for m in pattern.find_iter(&content) {
                *counts.entry(m.as_str().to_string()).or_default() += 1;
            }
        }
    }

    let mut terms: Vec<(String, usize)> = counts
        .into_iter()
        .filter(|(term, _)| is_candidate(term, config))
        .collect();
    terms.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(terms)
}

fn is_candidate(term: &str, config: &VocabConfig) -> bool {
    let len = term.chars().count();
    len >= config.min_term_length
        && len <= config.max_term_length
        && !config
            .common_words
            .iter()
            .any(|word| word.eq_ignore_ascii_case(term))
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || SKIPPED_DIRS.contains(&name.as_str()) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(&entry.path(), files)?;
        } else if file_type.is_file() {
            files.push(entry.path());
        }
    }
    Ok(())
}

/// File contents as text, or `None` for oversized, binary (NUL byte) or
/// non-UTF-8 files and unreadable paths.
fn read_text(path: &Path) -> Option<String> {
    if std::fs::metadata(path).ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    (!content.contains('\0')).then_some(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique, empty scratch directory (no `tempfile` dependency).
    fn scratch_dir(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "always-vocab-project-{label}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, rel: &str, bytes: &[u8]) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn terms_of(root: &Path) -> Vec<(String, usize)> {
        extract_project_terms(root, &VocabConfig::default()).unwrap()
    }

    #[test]
    fn counts_identifiers_most_frequent_first() {
        let root = scratch_dir("counts");
        write(
            &root,
            "src/lib.rs",
            b"struct MyType;\nfn parse_config() { my_var; my_var; }\n",
        );
        let terms = terms_of(&root);
        assert_eq!(terms[0], ("my_var".to_string(), 2));
        assert!(terms.contains(&("MyType".to_string(), 1)));
        assert!(terms.contains(&("parse_config".to_string(), 1)));
    }

    #[test]
    fn drops_common_words_and_out_of_bounds_lengths() {
        let root = scratch_dir("filter");
        write(&root, "notes.txt", b"The And A Ab my_name\n");
        let terms: Vec<String> = terms_of(&root).into_iter().map(|(t, _)| t).collect();
        // "The"/"And" are common words, "A" is below min length 2.
        assert_eq!(terms, vec!["Ab".to_string(), "my_name".to_string()]);
    }

    #[test]
    fn skips_hidden_dependency_binary_and_oversized_files() {
        let root = scratch_dir("skips");
        write(&root, "src/real.rs", b"RealTerm\n");
        write(&root, ".git/config", b"HiddenThing\n");
        write(&root, "target/out.rs", b"BuildOutput\n");
        write(&root, "node_modules/dep/index.js", b"DepModule\n");
        write(&root, "bin/blob.dat", b"BinaryTerm\0\x01");
        let mut big = b"BigTerm ".to_vec();
        big.resize(MAX_FILE_BYTES as usize + 1, b' ');
        write(&root, "big.txt", &big);
        let terms: Vec<String> = terms_of(&root).into_iter().map(|(t, _)| t).collect();
        assert_eq!(terms, vec!["RealTerm".to_string()]);
    }

    #[test]
    fn invalid_pattern_is_an_error() {
        let root = scratch_dir("badpattern");
        let config = VocabConfig {
            file_patterns: vec!["(unclosed".to_string()],
            ..VocabConfig::default()
        };
        let err = extract_project_terms(&root, &config).unwrap_err();
        assert!(err.to_string().contains("invalid vocab file pattern"));
    }
}
