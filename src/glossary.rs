//! Shared glossary loader. Provides the niche-term list both for Whisper's
//! `prompt` parameter (vocabulary biasing at transcription time) and for the
//! LLM post-processor's system prompt (term-aware cleanup).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Whisper's `prompt` field has a 224-token hard limit; ~120 words leaves a
/// comfortable safety margin.
const WHISPER_WORD_BUDGET: usize = 120;

#[derive(Deserialize)]
struct Entry {
    term: String,
    #[serde(default)]
    mistranscriptions: Vec<String>,
    #[serde(default)]
    frequency: i64,
}

static ENTRIES: OnceLock<Vec<Entry>> = OnceLock::new();
static WHISPER_PROMPT: OnceLock<Option<String>> = OnceLock::new();
static POSTPROCESS_PROMPT: OnceLock<String> = OnceLock::new();
static AI_FILTER_CONTEXT: OnceLock<String> = OnceLock::new();

fn entries() -> &'static [Entry] {
    ENTRIES.get_or_init(|| match load_entries() {
        Ok(mut v) => {
            v.sort_by(|a, b| b.frequency.cmp(&a.frequency));
            v
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to load glossary.json");
            Vec::new()
        }
    })
}

/// Vocabulary-biasing string for Whisper's `prompt` field. Returns `None`
/// when the glossary is empty or unavailable.
pub fn whisper_bias_prompt() -> Option<&'static String> {
    WHISPER_PROMPT
        .get_or_init(|| build_whisper_bias_prompt(entries()))
        .as_ref()
}

/// System prompt for the LLM post-processor. Falls back to grammar-only rules
/// when the glossary is empty.
pub fn postprocess_system_prompt() -> &'static str {
    POSTPROCESS_PROMPT.get_or_init(|| build_postprocess_prompt(entries()))
}

/// Glossary context for JSON-producing AI filters. Unlike the postprocess prompt,
/// this contains only term/correction facts and no output-format instructions.
pub fn ai_filter_vocabulary_context() -> &'static str {
    AI_FILTER_CONTEXT.get_or_init(|| build_ai_filter_context(entries()))
}

fn build_whisper_bias_prompt(entries: &[Entry]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let prefix = "Common tools and products: ";
    let mut word_count = prefix.split_whitespace().count();
    let mut chosen: Vec<&str> = Vec::new();
    for entry in entries {
        let term = entry.term.trim();
        if term.is_empty() {
            continue;
        }
        let term_words = term.split_whitespace().count().max(1);
        if word_count + term_words + 1 > WHISPER_WORD_BUDGET {
            break;
        }
        word_count += term_words + 1;
        chosen.push(term);
    }
    if chosen.is_empty() {
        return None;
    }
    Some(format!("{prefix}{}.", chosen.join(", ")))
}

fn build_postprocess_prompt(entries: &[Entry]) -> String {
    let mut out = String::from(
        "You are post-processing a Whisper speech-to-text transcript from a developer \
talking about their projects. Produce a clean, coherent version.\n\n",
    );

    if !entries.is_empty() {
        out.push_str(
            "# Niche vocabulary used by this speaker\n\n\
These terms come up frequently. Whisper often mistranscribes them — fix any phonetic \
mistranscriptions you spot. Canonical spelling is on the left; common mistranscriptions \
on the right.\n\n",
        );
        for e in entries {
            let term = e.term.trim();
            if term.is_empty() {
                continue;
            }
            if e.mistranscriptions.is_empty() {
                out.push_str(&format!("- {term}\n"));
            } else {
                let miss: Vec<&str> = e
                    .mistranscriptions
                    .iter()
                    .take(5)
                    .map(|s| s.as_str())
                    .collect();
                out.push_str(&format!("- {term} — {}\n", miss.join(", ")));
            }
        }
        out.push('\n');
    }

    out.push_str(
        "# Rules\n\n\
1. Fix mistranscriptions of the niche terms above when context confirms it. Don't \
   change ordinary words like \"cloud\" when cloud computing is meant.\n\
2. Make the result a coherent, well-formed phrase. Restore minimal grammar so the \
   output reads as a real sentence. Add periods and commas at obvious sentence \
   boundaries. Add question marks for questions.\n\
3. Fix capitalization (proper nouns, sentence starts).\n\
4. Do not invent content. If something is unclear, leave it.\n\
5. Do not translate. If the speaker switched languages, leave it as is.\n\
6. Preserve informal style — \"gonna\" stays \"gonna\", profanity stays.\n\
7. Output only the cleaned transcript. No preamble, no explanation, no quotes.\n",
    );
    out
}

fn build_ai_filter_context(entries: &[Entry]) -> String {
    if entries.is_empty() {
        return "No glossary entries available.".to_string();
    }

    let mut out = String::from(
        "Canonical glossary terms and common mistranscriptions. Prefer the canonical term when the input is a phonetic match:\n",
    );

    for e in entries.iter().take(80) {
        let term = e.term.trim();
        if term.is_empty() {
            continue;
        }

        if e.mistranscriptions.is_empty() {
            out.push_str(&format!("- {term}\n"));
        } else {
            let miss: Vec<&str> = e
                .mistranscriptions
                .iter()
                .take(5)
                .map(|s| s.as_str())
                .collect();
            out.push_str(&format!(
                "- canonical: \"{term}\"; mistranscriptions: {}\n",
                miss.iter()
                    .map(|s| format!("\"{s}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    out
}

fn load_entries() -> Result<Vec<Entry>> {
    let Some(path) = locate_glossary() else {
        anyhow::bail!("glossary.json not found");
    };
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading glossary file: {}", path.display()))?;
    let entries: Vec<Entry> = serde_json::from_str(&raw)
        .with_context(|| format!("parsing glossary file: {}", path.display()))?;
    Ok(entries)
}

/// Canonical user-facing location for glossary.json. The daemon writes
/// here on `always vocab import`, the GUI Settings panel reveals here,
/// and `locate_glossary` searches here first.
pub fn user_glossary_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".always").join("glossary.json"))
}

/// Resolve the glossary path. Search order:
///   1. `ALWAYS_GLOSSARY_PATH` env var (escape hatch for tests/CI)
///   2. `~/.always/glossary.json` (canonical user location)
///   3. `$CWD/glossary.json`             (legacy — for users who haven't migrated)
///   4. Adjacent to the daemon binary    (legacy)
fn locate_glossary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ALWAYS_GLOSSARY_PATH") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = user_glossary_path() {
        candidates.push(p);
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("glossary.json"));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("glossary.json"));
        if let Some(parent) = dir.parent().and_then(|p| p.parent()) {
            candidates.push(parent.join("glossary.json"));
        }
    }
    candidates.into_iter().find(|p| p.exists())
}
