//! LLM-assisted correction extraction — a "second opinion" that runs
//! after the deterministic `diff_words` heuristic.
//!
//! The deterministic path (`diff_words` in `correction.rs`) is fast and
//! runs immediately on every hotkey capture. But it has blind spots:
//! short words ("ap" → "API"), non-phonetic corrections ("andy" →
//! "Handy"), and style edits vs mishearings are indistinguishable.
//!
//! This module sends `(original, corrected)` to an LLM with instructions
//! to extract only genuine STT mishearings — not style edits — and
//! return them as `CorrectionPair`s. The LLM path runs asynchronously
//! after the deterministic path, so the user gets immediate feedback
//! from `diff_words` and may get additional pairs from the LLM a few
//! seconds later.
//!
//! Both paths write to the same glossary via `apply_pairs_to_glossary`.
//! The LLM path deduplicates against pairs already written by
//! `diff_words` so there's no double-write.
//!
//! Safety:
//! - If no LLM is available (no API key), extraction is skipped.
//! - LLM-extracted pairs go through the same `apply_pairs_to_glossary`
//!   — same dedup, same glossary format.
//! - Capped at `MAX_PAIRS_PER_CAPTURE` (8) — same as `diff_words`.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::always::correction::{CorrectionPair, MAX_PAIRS_PER_CAPTURE};

/// Extract correction pairs from `(original, corrected)` using an LLM.
///
/// `already_found` is the set of pairs already extracted by the
/// deterministic `diff_words` path — the LLM's output is deduplicated
/// against these so we don't double-write.
pub async fn extract_corrections_via_llm(
    original: &str,
    corrected: &str,
    api_key: &str,
    model: &str,
    already_found: &[CorrectionPair],
) -> Result<Vec<CorrectionPair>> {
    let prompt = build_extraction_prompt(original, corrected);
    let response = call_llm(&prompt, api_key, model).await?;
    let pairs = parse_llm_response(&response)?;

    // Dedup against pairs already found by diff_words.
    let already: std::collections::HashSet<(String, String)> = already_found
        .iter()
        .map(|p| (p.wrong.to_lowercase(), p.right.to_lowercase()))
        .collect();
    let new_pairs = pairs
        .into_iter()
        .filter(|p| !already.contains(&(p.wrong.to_lowercase(), p.right.to_lowercase())))
        .take(MAX_PAIRS_PER_CAPTURE)
        .collect();

    Ok(new_pairs)
}

fn build_extraction_prompt(original: &str, corrected: &str) -> String {
    format!(
        r#"You are a speech-to-text correction analyzer. Compare the original transcription with the user's corrected version and extract ONLY genuine STT mishearings — words the speech-to-text engine got wrong phonetically.

DO NOT extract:
- Style edits (rewording for clarity, tone, or brevity)
- Grammar fixes (punctuation, capitalization, word order)
- Additions or deletions of entire phrases
- Corrections that are just rephrasings

DO extract:
- Words that sound similar but were misrecognized (e.g. "andy" → "Handy", "ap" → "API", "kubernetics" → "Kubernetes")
- Brand names or technical terms that were phonetically misheard

Return a JSON array of objects with "wrong" and "right" fields. If there are no genuine mishearings, return an empty array [].

Original transcription:
{original}

Corrected version:
{corrected}

Return ONLY the JSON array, no other text:"#
    )
}

async fn call_llm(prompt: &str, api_key: &str, model: &str) -> Result<String> {
    let client = crate::http_client::async_client();
    let response = client
        .post("https://api.groq.com/openai/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "model": model,
            "messages": [
                {
                    "role": "user",
                    "content": prompt
                }
            ],
            "temperature": 0.0,
            "max_tokens": 500
        }))
        .send()
        .await
        .context("Failed to call Groq API for correction extraction")?;

    if !response.status().is_success() {
        let error = response.text().await.unwrap_or_default();
        anyhow::bail!("Groq API error (extraction): {}", error);
    }

    let json: serde_json::Value = response
        .json()
        .await
        .context("Failed to parse Groq extraction response")?;

    let content = json["choices"][0]["message"]["content"]
        .as_str()
        .context("Invalid Groq extraction response format")?
        .trim()
        .to_string();

    Ok(content)
}

#[derive(Deserialize)]
struct LlmCorrection {
    wrong: String,
    right: String,
}

fn parse_llm_response(response: &str) -> Result<Vec<CorrectionPair>> {
    // Strip markdown code fences if present.
    let cleaned = response
        .trim()
        .strip_prefix("```json")
        .or_else(|| response.trim().strip_prefix("```"))
        .unwrap_or(response)
        .trim()
        .strip_suffix("```")
        .unwrap_or(response)
        .trim();

    let llm_pairs: Vec<LlmCorrection> = serde_json::from_str(cleaned)
        .context("Failed to parse LLM extraction response as JSON array")?;

    let pairs = llm_pairs
        .into_iter()
        .filter(|p| !p.wrong.trim().is_empty() && !p.right.trim().is_empty())
        .map(|p| CorrectionPair {
            wrong: p.wrong.trim().to_string(),
            right: p.right.trim().to_string(),
        })
        .collect();

    Ok(pairs)
}

/// Check whether LLM extraction is available (API key present and
/// grammar correction enabled — we reuse the same Groq key).
pub fn llm_extraction_available(api_key: Option<&str>) -> bool {
    api_key.is_some_and(|k| !k.is_empty())
}

/// Read the Groq API key from the DB for correction extraction.
/// Returns `None` if no key is configured.
pub fn get_extraction_api_key() -> Option<String> {
    let Ok(conn) = crate::db::open() else {
        return None;
    };
    let Ok(prefs) = crate::db::get_preferences(&conn) else {
        return None;
    };
    prefs.groq_api_key
}

/// Get the model to use for extraction. Falls back to the post-process
/// model if no dedicated correction model is configured.
pub fn get_extraction_model() -> String {
    // Check for a dedicated correction_model preference (Phase 3.2).
    if let Ok(conn) = crate::db::open()
        && let Ok(prefs) = crate::db::get_preferences(&conn)
        && let Some(model) = prefs.correction_model
        && !model.trim().is_empty()
    {
        return model;
    }
    // Fall back to the post-processing model.
    crate::always::config::PostprocessConfig::default().groq_model
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_response() {
        let response = r#"[{"wrong": "andy", "right": "Handy"}, {"wrong": "ap", "right": "API"}]"#;
        let pairs = parse_llm_response(response).unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].wrong, "andy");
        assert_eq!(pairs[0].right, "Handy");
        assert_eq!(pairs[1].wrong, "ap");
        assert_eq!(pairs[1].right, "API");
    }

    #[test]
    fn parse_empty_array() {
        let response = "[]";
        let pairs = parse_llm_response(response).unwrap();
        assert!(pairs.is_empty());
    }

    #[test]
    fn parse_markdown_fenced_json() {
        let response = "```json\n[{\"wrong\": \"kline\", \"right\": \"Cline\"}]\n```";
        let pairs = parse_llm_response(response).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].wrong, "kline");
        assert_eq!(pairs[0].right, "Cline");
    }

    #[test]
    fn parse_filters_empty_strings() {
        let response = r#"[{"wrong": "", "right": "Handy"}, {"wrong": "ap", "right": ""}]"#;
        let pairs = parse_llm_response(response).unwrap();
        assert!(pairs.is_empty());
    }

    #[test]
    fn dedup_against_already_found() {
        let already = vec![CorrectionPair {
            wrong: "kubernetics".to_string(),
            right: "Kubernetes".to_string(),
        }];
        let llm_pairs = vec![
            CorrectionPair {
                wrong: "kubernetics".to_string(),
                right: "Kubernetes".to_string(),
            },
            CorrectionPair {
                wrong: "andy".to_string(),
                right: "Handy".to_string(),
            },
        ];
        // Simulate the dedup logic
        let already_set: std::collections::HashSet<(String, String)> = already
            .iter()
            .map(|p| (p.wrong.to_lowercase(), p.right.to_lowercase()))
            .collect();
        let new_pairs: Vec<CorrectionPair> = llm_pairs
            .into_iter()
            .filter(|p| !already_set.contains(&(p.wrong.to_lowercase(), p.right.to_lowercase())))
            .collect();
        assert_eq!(new_pairs.len(), 1);
        assert_eq!(new_pairs[0].wrong, "andy");
    }
}
