//! `always vocab extract`: read-only scan of a project for identifier-like
//! terms worth teaching the dictation engine.
//!
//! Candidates are the matches of `VocabConfig::file_patterns` in the text
//! files under the project root, kept when they pass the length bounds, are
//! not in `VocabConfig::common_words`, and are not language or standard-library
//! words. A term with project evidence (it appears in a doc, a file name, or a
//! string literal in code) ranks above a term that only appears as a code
//! identifier. Nothing is written to the glossary: the caller reviews the list
//! and applies what it wants.

use crate::always::config::VocabConfig;
use anyhow::{Context, Result};
use regex::Regex;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Largest file scanned; bigger files (lockfiles, bundles) are skipped.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Directories never descended into (dependency and build output).
const SKIPPED_DIRS: &[&str] = &["target", "node_modules", "vendor", "dist", "build"];

/// Files whose whole text is prose: every term in them is project evidence.
const DOC_EXTENSIONS: &[&str] = &["md", "txt", "rst", "adoc"];

/// Rust and common JS/TS keywords, prelude items, std types and macros.
/// Matched case-sensitively: these are identifiers, not English words.
const LANGUAGE_WORDS: &[&str] = &[
    // Rust keywords and primitives
    "as",
    "async",
    "await",
    "break",
    "const",
    "continue",
    "crate",
    "dyn",
    "else",
    "enum",
    "extern",
    "false",
    "fn",
    "for",
    "if",
    "impl",
    "in",
    "let",
    "loop",
    "match",
    "mod",
    "move",
    "mut",
    "pub",
    "ref",
    "return",
    "self",
    "Self",
    "static",
    "struct",
    "super",
    "trait",
    "true",
    "type",
    "unsafe",
    "use",
    "where",
    "while",
    "yield",
    "bool",
    "char",
    "str",
    "usize",
    "isize",
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "f32",
    "f64",
    // JS/TS keywords
    "class",
    "const",
    "default",
    "export",
    "function",
    "import",
    "interface",
    "let",
    "new",
    "null",
    "this",
    "throw",
    "try",
    "catch",
    "typeof",
    "undefined",
    "var",
    "void",
    // Prelude and common std types and traits
    "Option",
    "Some",
    "None",
    "Result",
    "Ok",
    "Err",
    "Vec",
    "String",
    "Box",
    "Rc",
    "Arc",
    "Cell",
    "RefCell",
    "Mutex",
    "RwLock",
    "HashMap",
    "HashSet",
    "BTreeMap",
    "BTreeSet",
    "VecDeque",
    "Path",
    "PathBuf",
    "Duration",
    "Instant",
    "Ordering",
    "Cow",
    "Pin",
    "Future",
    "Default",
    "Clone",
    "Copy",
    "Send",
    "Sync",
    "Sized",
    "Iterator",
    "IntoIterator",
    "From",
    "Into",
    "TryFrom",
    "TryInto",
    "AsRef",
    "AsMut",
    "Drop",
    "Fn",
    "FnMut",
    "FnOnce",
    "Eq",
    "Ord",
    "PartialEq",
    "PartialOrd",
    "Debug",
    "Hash",
    "ToString",
    "ToOwned",
    "Display",
    "Error",
    "Write",
    "Read",
    // Std macros
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "println",
    "print",
    "eprintln",
    "eprint",
    "format",
    "vec",
    "panic",
    "write",
    "writeln",
    "matches",
    "todo",
    "unimplemented",
    "unreachable",
];

/// Standard-library method and field names that show up in code all the time.
/// Matched case-insensitively.
const STD_METHOD_WORDS: &[&str] = &[
    "is_empty",
    "is_none",
    "is_some",
    "is_ok",
    "is_err",
    "unwrap",
    "unwrap_or",
    "unwrap_or_else",
    "unwrap_or_default",
    "expect",
    "len",
    "iter",
    "into_iter",
    "map",
    "and_then",
    "ok_or",
    "as_str",
    "as_ref",
    "as_mut",
    "to_string",
    "to_owned",
    "clone",
    "push",
    "pop",
    "insert",
    "remove",
    "get",
    "contains",
    "starts_with",
    "ends_with",
    "trim",
    "split",
    "collect",
    "lock",
    "with_capacity",
    "from_str",
    "find_iter",
    "captures_iter",
    "is_match",
    "display",
    "is_dir",
    "is_file",
    "exists",
    "join",
    "parent",
    "file_name",
    "file_stem",
    "extension",
    "read_to_string",
    "write_all",
    "flush",
    "spawn",
    "send",
    "recv",
    "sleep",
    "elapsed",
];

/// SQL keywords (string literals in code are scanned, so SQL shows up here).
/// Matched case-insensitively.
const SQL_WORDS: &[&str] = &[
    "select",
    "insert",
    "update",
    "delete",
    "from",
    "where",
    "create",
    "table",
    "index",
    "primary",
    "key",
    "limit",
    "add",
    "alter",
    "drop",
    "into",
    "values",
    "join",
    "order",
    "group",
    "having",
    "union",
    "null",
    "references",
    "foreign",
    "unique",
    "and",
    "or",
    "not",
    "on",
    "set",
];

/// Most common English words, which dictation already spells correctly and
/// which show up in UI strings and prose. Matched case-insensitively.
const ENGLISH_WORDS: &[&str] = &[
    "a", "about", "above", "after", "again", "all", "also", "am", "an", "and", "any", "are", "as",
    "at", "be", "because", "been", "before", "being", "below", "between", "both", "but", "by",
    "can", "could", "did", "do", "does", "done", "down", "during", "each", "either", "else",
    "even", "ever", "every", "few", "first", "for", "from", "get", "got", "had", "has", "have",
    "he", "her", "here", "hers", "him", "his", "how", "i", "if", "in", "into", "is", "it", "its",
    "just", "last", "less", "like", "make", "many", "may", "me", "more", "most", "much", "must",
    "my", "near", "need", "new", "next", "no", "none", "nor", "not", "now", "of", "off", "often",
    "old", "on", "once", "one", "only", "or", "other", "our", "out", "over", "own", "per",
    "please", "same", "she", "should", "since", "so", "some", "still", "such", "take", "than",
    "that", "the", "their", "them", "then", "there", "these", "they", "this", "those", "through",
    "to", "too", "two", "under", "until", "up", "upon", "us", "use", "very", "was", "we", "well",
    "were", "what", "when", "where", "which", "while", "who", "why", "will", "with", "within",
    "without", "would", "yes", "yet", "you", "your", "failed", "failure", "error", "errors",
    "return", "returned", "command", "hello", "thanks", "sorry", "okay", "ok", "try", "tried",
    "trying", "run", "running", "open", "close", "start", "stop", "search", "save", "saved",
    "cancel", "done", "back", "next", "previous", "settings", "help", "name", "value", "values",
    "type", "text", "file", "files", "folder", "page", "view", "add", "edit", "delete", "update",
    "select", "copy", "paste", "item", "items", "list", "title", "label", "message", "messages",
    "status", "true", "false", "null", "default", "none",
];

/// One candidate term with its occurrence count and whether it carries
/// project evidence (appears in a doc, a file name, or a code string literal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTerm {
    pub term: String,
    pub count: usize,
    pub project_specific: bool,
}

#[derive(Default)]
struct Tally {
    count: usize,
    project_specific: bool,
}

/// Candidate terms under `root`, project-specific first, then most frequent
/// (ties alphabetical). Hidden entries, dependency and build directories,
/// symlinks, binary and oversized files are skipped.
pub fn extract_project_terms(root: &Path, config: &VocabConfig) -> Result<Vec<ProjectTerm>> {
    let patterns = config
        .file_patterns
        .iter()
        .map(|p| Regex::new(p).with_context(|| format!("invalid vocab file pattern: {p}")))
        .collect::<Result<Vec<_>>>()?;
    let literal = Regex::new(r#""((?:[^"\\\n]|\\.)*)""#).expect("static regex");

    let mut files = Vec::new();
    collect_files(root, &mut files)?;

    let mut found: HashMap<String, Tally> = HashMap::new();
    for file in &files {
        let Some(content) = read_text(file) else {
            continue;
        };
        // File name tokens are project evidence.
        if let Some(name) = file.file_name().and_then(|n| n.to_str()) {
            tally(name, &patterns, true, &mut found);
        }
        if is_doc(file) {
            tally(&content, &patterns, true, &mut found);
        } else {
            tally(&content, &patterns, false, &mut found);
            // UI strings in code are project evidence.
            for caps in literal.captures_iter(&content) {
                if let Some(text) = caps.get(1) {
                    tally(text.as_str(), &patterns, true, &mut found);
                }
            }
        }
    }

    let mut terms: Vec<ProjectTerm> = found
        .into_iter()
        .filter(|(term, _)| is_candidate(term, config))
        .map(|(term, tally)| ProjectTerm {
            term,
            count: tally.count,
            project_specific: tally.project_specific,
        })
        .collect();
    terms.sort_by(|a, b| {
        b.project_specific
            .cmp(&a.project_specific)
            .then_with(|| b.count.cmp(&a.count))
            .then_with(|| a.term.cmp(&b.term))
    });
    Ok(terms)
}

fn tally(
    text: &str,
    patterns: &[Regex],
    project_specific: bool,
    found: &mut HashMap<String, Tally>,
) {
    // One occurrence per text span: patterns overlap (e.g. `README` matches
    // both the CamelCase and the SCREAMING_CASE pattern) and must not double-count.
    let spans: BTreeSet<(usize, usize)> = patterns
        .iter()
        .flat_map(|pattern| pattern.find_iter(text))
        .map(|m| (m.start(), m.end()))
        .collect();
    for (start, end) in spans {
        let entry = found.entry(text[start..end].to_string()).or_default();
        entry.count += 1;
        entry.project_specific |= project_specific;
    }
}

fn is_candidate(term: &str, config: &VocabConfig) -> bool {
    let len = term.chars().count();
    len >= config.min_term_length
        && len <= config.max_term_length
        && !LANGUAGE_WORDS.contains(&term)
        && !is_noise_word(term)
        && !config
            .common_words
            .iter()
            .any(|word| word.eq_ignore_ascii_case(term))
}

/// Std methods, SQL keywords and English words, matched case-insensitively.
fn is_noise_word(term: &str) -> bool {
    [STD_METHOD_WORDS, SQL_WORDS, ENGLISH_WORDS]
        .iter()
        .any(|list| list.iter().any(|word| word.eq_ignore_ascii_case(term)))
}

fn is_doc(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| DOC_EXTENSIONS.contains(&ext))
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

    fn terms_of(root: &Path) -> Vec<ProjectTerm> {
        extract_project_terms(root, &VocabConfig::default()).unwrap()
    }

    fn names(root: &Path) -> Vec<String> {
        terms_of(root).into_iter().map(|t| t.term).collect()
    }

    #[test]
    fn counts_identifiers_and_flags_code_only_terms() {
        let root = scratch_dir("counts");
        write(
            &root,
            "src/lib.rs",
            b"struct MyType;\nfn parse_config() { my_var; my_var; }\n",
        );
        let terms = terms_of(&root);
        let my_var = terms.iter().find(|t| t.term == "my_var").unwrap();
        assert_eq!(my_var.count, 2);
        assert!(!my_var.project_specific);
        assert!(names(&root).contains(&"MyType".to_string()));
        assert!(names(&root).contains(&"parse_config".to_string()));
    }

    #[test]
    fn drops_common_words_language_words_and_out_of_bounds_lengths() {
        let root = scratch_dir("filter");
        write(&root, "notes.txt", b"The And A Ab my_name\n");
        // "The"/"And" are common words, "A" is below min length 2.
        assert_eq!(names(&root), vec!["Ab".to_string(), "my_name".to_string()]);
    }

    #[test]
    fn excludes_language_and_std_identifiers_from_code() {
        let root = scratch_dir("language");
        write(
            &root,
            "src/main.rs",
            b"fn main() -> Option<String> { let v: Vec<Box<u8>> = Vec::new(); assert_eq!(v, None); Some(Self::Ok) }\n",
        );
        let names = names(&root);
        for word in [
            "Option", "String", "Vec", "Box", "None", "Some", "Self", "Ok",
        ] {
            assert!(!names.contains(&word.to_string()), "{word} leaked");
        }
    }

    #[test]
    fn project_evidence_ranks_above_code_only_identifiers() {
        let root = scratch_dir("rank");
        // CodeOnly appears 5 times in code; DocTerm once, in a README.
        write(
            &root,
            "src/lib.rs",
            b"CodeOnly; CodeOnly; CodeOnly; CodeOnly; CodeOnly;\n",
        );
        write(&root, "README.md", b"Set up DocTerm first.\n");
        let terms = terms_of(&root);
        assert_eq!(terms[0].term, "DocTerm");
        assert!(terms[0].project_specific);
        let code_only = terms.iter().find(|t| t.term == "CodeOnly").unwrap();
        assert_eq!(code_only.count, 5);
        assert!(!code_only.project_specific);
    }

    #[test]
    fn ui_string_literals_and_file_names_are_project_evidence() {
        let root = scratch_dir("evidence");
        write(
            &root,
            "src/ui.rs",
            b"let label = \"ShowOverlay\"; let other = plain_ident;\n",
        );
        write(&root, "src/wave_form.rs", b"\n");
        let terms = terms_of(&root);
        let ui = terms.iter().find(|t| t.term == "ShowOverlay").unwrap();
        assert!(ui.project_specific);
        let file = terms.iter().find(|t| t.term == "wave_form").unwrap();
        assert!(file.project_specific);
        let ident = terms.iter().find(|t| t.term == "plain_ident").unwrap();
        assert!(!ident.project_specific);
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
        assert_eq!(names(&root), vec!["RealTerm".to_string()]);
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
