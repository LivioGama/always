//! Single builder for everything sent to the grammar LLM.
//!
//! Both the speculative grammar warm (kicked off from the VAD silence
//! window) and the blocking paste path construct their LLM input here.
//! That is the load-bearing invariant: the two paths must produce a
//! byte-identical user message, because the PostProcessor's cache and
//! single-flight dedupe are keyed on it — any divergence silently turns
//! every paste into a cold ~600ms LLM call.
//!
//! The user message itself doubles as the cache key (no separate hash),
//! so "key equality" and "prompt equality" cannot drift apart.

use crate::always::text_match::{
    DEFAULT_THRESHOLD, Substitution, SubstitutionTier, apply_glossary_tiered,
};

/// One fully-prepared grammar-correction request.
#[derive(Debug, Clone)]
pub struct CorrectionRequest {
    /// Transcript after tier-1 (exact mistranscription) rewrites. What
    /// the paste falls back to when the LLM is unavailable.
    pub acoustic_text: String,
    /// Unapplied fuzzy glossary matches for the LLM to arbitrate with
    /// sentence context: `(heard, canonical)`.
    pub deferred_candidates: Vec<(String, String)>,
    /// Tail of the live dictation session (recently pasted text in the
    /// same app), if any.
    pub context_before: Option<String>,
    /// The exact LLM user message — also the cache / single-flight key.
    pub user_message: String,
}

/// Build the request for `text`. `llm_available` toggles fuzzy-match
/// arbitration: when the grammar LLM is enabled, fuzzy matches are NOT
/// applied to the text and instead ride along as candidates; when it's
/// disabled, the legacy rewrite-in-place behavior applies (deterministic
/// but context-blind — there is nobody downstream to decide).
pub fn build(text: &str, llm_available: bool) -> CorrectionRequest {
    let entries = crate::glossary::glossary_match_entries();
    let (acoustic_text, substitutions) =
        apply_glossary_tiered(text, &entries, DEFAULT_THRESHOLD, !llm_available);
    log_substitutions(text, &acoustic_text, &substitutions);

    let deferred_candidates: Vec<(String, String)> = substitutions
        .iter()
        .filter(|s| s.tier == SubstitutionTier::Fuzzy && !s.applied)
        .map(|s| (s.original.clone(), s.replacement.clone()))
        .collect();

    let context_before = crate::always::pause::dictation_buffer_text()
        .or_else(crate::always::dictation::session_context);

    let user_message = render_user_message(
        &acoustic_text,
        &deferred_candidates,
        context_before.as_deref(),
    );

    CorrectionRequest {
        acoustic_text,
        deferred_candidates,
        context_before,
        user_message,
    }
}

/// The exact user-message layout the system prompt documents. Sections
/// are emitted in a fixed order so identical inputs always serialize
/// identically (cache-key stability).
fn render_user_message(
    transcript: &str,
    candidates: &[(String, String)],
    context: Option<&str>,
) -> String {
    let mut out = String::new();
    if let Some(ctx) = context {
        out.push_str("<context_before>");
        out.push_str(ctx);
        out.push_str("</context_before>\n");
    }
    if !candidates.is_empty() {
        out.push_str("<glossary_candidates>\n");
        for (heard, canonical) in candidates {
            out.push_str(&format!("{heard} → {canonical}\n"));
        }
        out.push_str("</glossary_candidates>\n");
    }
    out.push_str("<transcript>");
    out.push_str(transcript);
    out.push_str("</transcript>");
    out
}

fn log_substitutions(before: &str, after: &str, subs: &[Substitution]) {
    if subs.is_empty() {
        tracing::debug!(stage = "acoustic_match", "no glossary matches");
        return;
    }
    let applied: Vec<String> = subs
        .iter()
        .filter(|s| s.applied)
        .map(|s| format!("{}->{}", s.original, s.replacement))
        .collect();
    let deferred: Vec<String> = subs
        .iter()
        .filter(|s| !s.applied)
        .map(|s| format!("{}->{}", s.original, s.replacement))
        .collect();
    tracing::info!(
        stage = "acoustic_match",
        before = %before,
        after = %after,
        applied = %applied.join(", "),
        deferred = %deferred.join(", "),
        "glossary matching"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_layout_is_stable() {
        let msg = render_user_message(
            "ask cloud to fix it",
            &[("cloud".into(), "Claude".into())],
            Some("Earlier text."),
        );
        assert_eq!(
            msg,
            "<context_before>Earlier text.</context_before>\n\
             <glossary_candidates>\ncloud → Claude\n</glossary_candidates>\n\
             <transcript>ask cloud to fix it</transcript>"
        );
    }

    #[test]
    fn user_message_without_extras_is_bare_transcript() {
        let msg = render_user_message("hello world", &[], None);
        assert_eq!(msg, "<transcript>hello world</transcript>");
    }

    /// THE invariant: two builds of the same text under the same session
    /// state must produce identical user messages — the warm and paste
    /// paths share the cache through this equality.
    #[test]
    fn warm_and_paste_paths_produce_identical_keys() {
        // Serialize against the dictation tests — they mutate the same
        // process-global SESSION this invariant reads via session_text().
        let _guard = crate::always::dictation::SESSION_TEST_LOCK.lock();
        crate::always::dictation::clear();
        crate::always::pause::dictation_buffer_clear();
        let warm = build("testing one two three", true);
        let paste = build("testing one two three", true);
        assert_eq!(warm.user_message, paste.user_message);
        assert_eq!(warm.acoustic_text, paste.acoustic_text);
    }
}
