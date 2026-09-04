use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use serde_json::Value;

use super::config::{PostprocessConfig, PostprocessProvider};

/// Maximum number of cached (input → corrected) entries before we
/// evict the oldest. Each entry is bounded by the LLM's max_tokens
/// response cap (~500 chars) so 1 000 entries ~= 1 MB upper bound.
const CACHE_MAX_ENTRIES: usize = 1_000;

/// Optional LLM cleanup pass for the transcript.
///
/// Single transformation: send the transcript to the Groq LLM with a
/// strict system prompt (see `glossary::build_postprocess_prompt`) and
/// return the cleaned text. Falls through to the raw input when
/// `grammar_correction_enabled` is false or no API key is configured.
#[derive(Debug, Clone)]
pub struct PostProcessor {
    groq_api_key: Option<String>,
    /// Memoization of `(transcript → corrected)` pairs. Bounded by
    /// `CACHE_MAX_ENTRIES`: the previous implementation never evicted
    /// and grew unboundedly across long daemon sessions.
    cache: Arc<Mutex<HashMap<String, String>>>,
    /// Insertion order used for LRU-style eviction. The pair
    /// `(cache, cache_order)` is treated as a single unit; both locks
    /// are taken together in `insert_cached`.
    cache_order: Arc<Mutex<VecDeque<String>>>,
    /// In-flight requests keyed by exact input text. The speculative
    /// grammar warm (kicked off from the VAD silence window) and the
    /// blocking paste path frequently ask for the same text concurrently;
    /// the second caller joins the first request instead of issuing a
    /// duplicate. A failed call leaves the cell uninitialized so the next
    /// caller retries — matching the old retry-on-next-call behavior.
    inflight: Arc<Mutex<HashMap<String, Arc<tokio::sync::OnceCell<String>>>>>,
    config: PostprocessConfig,
}

impl PostProcessor {
    pub fn new(groq_api_key: Option<String>) -> Self {
        Self::new_with_config(PostprocessConfig::default(), groq_api_key)
    }

    pub fn new_with_config(config: PostprocessConfig, groq_api_key: Option<String>) -> Self {
        Self {
            groq_api_key,
            cache: Arc::new(Mutex::new(HashMap::new())),
            cache_order: Arc::new(Mutex::new(VecDeque::new())),
            inflight: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
    }

    /// True when a grammar call will actually reach the LLM (feature on
    /// AND a provider is available). Drives glossary tiering: fuzzy
    /// matches defer to the LLM only when the LLM will really run.
    pub fn can_correct(&self) -> bool {
        if !self.config.grammar_correction_enabled {
            return false;
        }
        match self.config.provider {
            PostprocessProvider::Groq => self.groq_api_key.is_some(),
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            PostprocessProvider::Apple => super::apple_intelligence::check_availability(),
            #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
            PostprocessProvider::Apple => false,
        }
    }

    /// Single optional cleanup pass.
    ///
    /// Just the LLM call when `grammar_correction_enabled` is true;
    /// otherwise raw passthrough. Strict prompt rules in
    /// `glossary::build_postprocess_prompt` keep the LLM from inventing
    /// substitutions. Voice-to-text delivers what was said by default.
    pub async fn process(&self, text: &str, context: Option<&str>) -> Result<String> {
        Ok(self
            .process_traced(text, context)
            .await
            .map(|(corrected, _cache_hit)| corrected)
            .unwrap_or_else(|_| text.to_string()))
    }

    /// Same as [`process`](Self::process) but surfaces errors and reports
    /// whether the result came from the memoization cache, so callers can
    /// log cache effectiveness (see `latency_breakdown` in `event_loop`).
    pub async fn process_traced(
        &self,
        text: &str,
        context: Option<&str>,
    ) -> Result<(String, bool)> {
        let user_message = match context {
            Some(ctx) => {
                format!("<context_before>{ctx}</context_before>\n<transcript>{text}</transcript>")
            }
            None => format!("<transcript>{text}</transcript>"),
        };
        self.process_keyed(&user_message, text, context).await
    }

    /// Request-based entry point — the warm path and the paste path both
    /// arrive here through `correction_request::build`, so the cache /
    /// single-flight key (the user message itself) is identical by
    /// construction.
    pub async fn process_request(
        &self,
        req: &crate::always::correction_request::CorrectionRequest,
    ) -> Result<(String, bool)> {
        self.process_keyed(
            &req.user_message,
            &req.acoustic_text,
            req.context_before.as_deref(),
        )
        .await
    }

    async fn process_keyed(
        &self,
        user_message: &str,
        transcript: &str,
        context: Option<&str>,
    ) -> Result<(String, bool)> {
        if !self.config.grammar_correction_enabled {
            return Ok((transcript.to_string(), false));
        }
        let cache_hit = self.cache.lock().contains_key(user_message);
        let corrected = match self.config.provider {
            PostprocessProvider::Groq => {
                let Some(ref api_key) = self.groq_api_key else {
                    return Ok((transcript.to_string(), false));
                };
                self.correct_grammar_groq(user_message, transcript, api_key, context)
                    .await?
            }
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            PostprocessProvider::Apple => {
                self.correct_grammar_apple(user_message, transcript, context)
                    .await?
            }
            #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
            PostprocessProvider::Apple => {
                return Ok((transcript.to_string(), false));
            }
        };
        Ok((corrected, cache_hit))
    }

    async fn correct_grammar_groq(
        &self,
        user_message: &str,
        transcript: &str,
        api_key: &str,
        context: Option<&str>,
    ) -> Result<String> {
        // Check cache first — keyed on the FULL user message (context +
        // candidates + transcript). The same transcript under different
        // context must not collide: the correction legitimately differs.
        if let Some(cached) = self.cache.lock().get(user_message) {
            tracing::debug!(stage = "grammar_correction", "grammar cache hit");
            return Ok(cached.clone());
        }

        // Single-flight: join an identical in-flight request instead of
        // duplicating it. Lock is only held to fetch/insert the cell —
        // never across the await below.
        let cell = {
            let mut inflight = self.inflight.lock();
            Arc::clone(
                inflight
                    .entry(user_message.to_string())
                    .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new())),
            )
        };
        let outcome = cell
            .get_or_try_init(|| self.fetch_correction(user_message, transcript, api_key, context))
            .await
            .cloned();
        self.inflight.lock().remove(user_message);
        outcome
    }

    /// Apple Intelligence correction path — same caching/single-flight
    /// pattern as Groq, but the actual LLM call goes through the
    /// Swift bridge (`apple_intelligence::process_text_with_system_prompt`).
    /// The Swift call is blocking, so we wrap it in
    /// `spawn_blocking` to avoid stalling the async runtime.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    async fn correct_grammar_apple(
        &self,
        user_message: &str,
        transcript: &str,
        context: Option<&str>,
    ) -> Result<String> {
        // Check cache first.
        if let Some(cached) = self.cache.lock().get(user_message) {
            tracing::debug!(stage = "grammar_correction", "apple grammar cache hit");
            return Ok(cached.clone());
        }

        // Single-flight.
        let cell = {
            let mut inflight = self.inflight.lock();
            Arc::clone(
                inflight
                    .entry(user_message.to_string())
                    .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new())),
            )
        };
        let outcome = cell
            .get_or_try_init(|| self.fetch_correction_apple(user_message, transcript, context))
            .await
            .cloned();
        self.inflight.lock().remove(user_message);
        outcome
    }

    /// The actual Apple Intelligence round-trip + response sanitation + cache insert.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    async fn fetch_correction_apple(
        &self,
        user_message: &str,
        transcript: &str,
        context: Option<&str>,
    ) -> Result<String> {
        let started = std::time::Instant::now();
        let system_prompt = crate::glossary::postprocess_system_prompt();
        let user_msg = user_message.to_string();

        // The Swift bridge is blocking — run on a blocking thread.
        let result = tokio::task::spawn_blocking(move || {
            super::apple_intelligence::process_text_with_system_prompt(
                &system_prompt,
                &user_msg,
                500,
            )
        })
        .await
        .context("Apple Intelligence task panicked")?;

        let raw_corrected = result
            .map_err(|e| anyhow::anyhow!("Apple Intelligence call failed: {e}"))?;
        let corrected = sanitize_corrected_text(transcript, &raw_corrected)
            .filter(|cleaned| {
                let echoed = context.is_some_and(|ctx| echoes_context(ctx, cleaned));
                if echoed {
                    tracing::warn!(stage = "grammar_correction", "rejected context echo (apple)");
                }
                !echoed
            })
            .unwrap_or_else(|| transcript.trim().to_string());

        tracing::info!(
            grammar_api_ms = started.elapsed().as_millis() as u64,
            provider = "apple",
            "grammar_api_call"
        );
        self.insert_cached(user_message.to_string(), corrected.clone());
        Ok(corrected)
    }

    /// The actual Groq round-trip + response sanitation + cache insert.
    /// Only ever reached via the single-flight cell in `correct_grammar_groq`.
    async fn fetch_correction(
        &self,
        user_message: &str,
        transcript: &str,
        api_key: &str,
        context: Option<&str>,
    ) -> Result<String> {
        let started = std::time::Instant::now();
        // Pooled client: a fresh `reqwest::Client::new()` here paid the
        // full DNS+TCP+TLS handshake (~100-300ms) on every utterance.
        let client = crate::http_client::async_client();
        let response = client
            .post("https://api.groq.com/openai/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "model": self.config.groq_model,
                "messages": [
                    {
                        "role": "system",
                        "content": crate::glossary::postprocess_system_prompt()
                    },
                    {
                        "role": "user",
                        "content": user_message
                    }
                ],
                "temperature": 0.1,
                "max_tokens": 500
            }))
            .send()
            .await
            .context("Failed to call Groq API")?;

        if !response.status().is_success() {
            let error = response.text().await.unwrap_or_default();
            anyhow::bail!("Groq API error: {}", error);
        }

        let json: Value = response
            .json()
            .await
            .context("Failed to parse Groq response")?;
        let raw_corrected = json["choices"][0]["message"]["content"]
            .as_str()
            .context("Invalid Groq response format")?
            .trim()
            .to_string();
        let corrected = sanitize_corrected_text(transcript, &raw_corrected)
            .filter(|cleaned| {
                // Context-echo guard: a model that prepends the supplied
                // context would double the user's text on paste. Reject
                // and fall back to the acoustic transcript.
                let echoed = context.is_some_and(|ctx| echoes_context(ctx, cleaned));
                if echoed {
                    tracing::warn!(stage = "grammar_correction", "rejected context echo");
                }
                !echoed
            })
            .unwrap_or_else(|| transcript.trim().to_string());

        tracing::info!(
            grammar_api_ms = started.elapsed().as_millis() as u64,
            model = %self.config.groq_model,
            "grammar_api_call"
        );
        self.insert_cached(user_message.to_string(), corrected.clone());
        Ok(corrected)
    }

    /// Insert a `(input, corrected)` pair into the cache with LRU
    /// eviction. The previous implementation only used the HashMap
    /// half and the unbounded VecDeque was reserved-but-never-used.
    /// Long daemon sessions accumulated entries indefinitely.
    fn insert_cached(&self, text: String, corrected: String) {
        let mut cache = self.cache.lock();
        let mut order = self.cache_order.lock();
        // Replace via `insert`: returns the previous value if the key was
        // present, in which case we already had this entry and just need
        // to bump its position in the LRU queue. Otherwise it's a fresh
        // insert and we may need to evict.
        if cache.insert(text.clone(), corrected).is_some() {
            order.retain(|k| k != &text);
            order.push_back(text);
            return;
        }
        order.push_back(text);
        while order.len() > CACHE_MAX_ENTRIES {
            if let Some(victim) = order.pop_front() {
                cache.remove(&victim);
            } else {
                break;
            }
        }
    }
}

/// True when the model's output starts by repeating the supplied
/// context (≥20 chars of it) — the failure mode where "continue from
/// context" is misread as "output context + continuation", which would
/// paste the user's earlier text twice.
fn echoes_context(context: &str, cleaned: &str) -> bool {
    let ctx = context.trim();
    let out = cleaned.trim();
    let prefix: String = ctx.chars().take(20).collect();
    prefix.chars().count() >= 20 && out.to_lowercase().starts_with(&prefix.to_lowercase())
}

fn sanitize_corrected_text(input: &str, output: &str) -> Option<String> {
    let mut cleaned = output.trim();
    if cleaned.is_empty() {
        return None;
    }

    if let Some(inner) = extract_tagged(cleaned, "transcript") {
        cleaned = inner.trim();
    }

    cleaned = strip_known_prefix(cleaned);
    cleaned = strip_wrapping_quotes(cleaned).trim();
    if cleaned.is_empty() {
        return None;
    }

    // Correction models sometimes append conversational filler to an otherwise
    // valid transcript. Treat that as a failed correction so the caller falls
    // back to the words the user actually spoke. Do not strip it from the
    // acoustic input: "thanks" can be intentional dictation.
    if has_appended_gratitude(input, cleaned) {
        return None;
    }

    let input_words = word_count(input);
    let output_words = word_count(cleaned);
    if input_words > 0 {
        if output_words > input_words + 4 {
            return None;
        }
        if input_words >= 4 && output_words * 3 < input_words * 2 {
            return None;
        }
    }

    Some(cleaned.to_string())
}

fn has_appended_gratitude(input: &str, output: &str) -> bool {
    let input_words = normalized_words(input);
    let output_words = normalized_words(output);
    [
        &["thank", "you"][..],
        &["thanks"][..],
        &["thanks", "a", "lot"][..],
        &["thank", "you", "very", "much"][..],
    ]
    .iter()
    .any(|suffix| ends_with_words(&output_words, suffix) && !ends_with_words(&input_words, suffix))
}

fn normalized_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| part.to_ascii_lowercase())
        .collect()
}

fn ends_with_words(words: &[String], suffix: &[&str]) -> bool {
    words.len() >= suffix.len()
        && words[words.len() - suffix.len()..]
            .iter()
            .zip(suffix)
            .all(|(word, expected)| word == expected)
}

fn extract_tagged<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(&text[start..end])
}

fn strip_known_prefix(mut text: &str) -> &str {
    loop {
        let lower = text.to_ascii_lowercase();
        let Some(prefix) = [
            "here is the cleaned transcript:",
            "here's the cleaned transcript:",
            "cleaned transcript:",
            "the cleaned transcript:",
            "the cleaned text:",
            "output:",
            "sure,",
        ]
        .iter()
        .find(|prefix| lower.starts_with(**prefix)) else {
            return text;
        };
        text = text[prefix.len()..].trim_start();
    }
}

fn strip_wrapping_quotes(text: &str) -> &str {
    let Some(first) = text.chars().next() else {
        return text;
    };
    let Some(last) = text.chars().last() else {
        return text;
    };
    if text.len() < 2 {
        return text;
    }
    match (first, last) {
        ('"', '"') | ('\'', '\'') | ('`', '`') => {
            let start = first.len_utf8();
            let end = text.len() - last.len_utf8();
            &text[start..end]
        }
        _ => text,
    }
}

fn word_count(text: &str) -> usize {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `process` returns the input verbatim when grammar correction
    /// is disabled — voice-to-text default is "deliver what was said".
    #[tokio::test]
    async fn process_passthrough_when_disabled() {
        let cfg = PostprocessConfig {
            grammar_correction_enabled: false,
            ..Default::default()
        };
        let processor = PostProcessor::new_with_config(cfg, None);
        let out = processor
            .process("I have an idea about Kubernetes.", None)
            .await
            .unwrap();
        assert_eq!(out, "I have an idea about Kubernetes.");
    }

    /// Even with grammar correction enabled, missing API key falls
    /// through to passthrough (we don't make the daemon hang on a
    /// configuration error).
    #[tokio::test]
    async fn process_passthrough_when_enabled_but_no_api_key() {
        let processor = PostProcessor::new(None);
        let out = processor.process("hello world", None).await.unwrap();
        assert_eq!(out, "hello world");
    }

    /// A cached entry short-circuits before any network call (api_key is
    /// set, so a miss here would attempt a real request and fail) and is
    /// reported as a cache hit to the latency instrumentation.
    #[tokio::test]
    async fn process_traced_reports_cache_hit_without_network() {
        let processor = PostProcessor::new(Some("test-key-never-used".to_string()));
        processor.insert_cached(
            "<transcript>hello world how are you</transcript>".into(),
            "Hello world, how are you?".into(),
        );
        let (out, cache_hit) = processor
            .process_traced("hello world how are you", None)
            .await
            .unwrap();
        assert_eq!(out, "Hello world, how are you?");
        assert!(cache_hit);
    }

    /// Concurrent identical requests share one in-flight cell: the second
    /// caller must observe the same `Arc` rather than creating a duplicate.
    #[tokio::test]
    async fn inflight_cell_is_shared_for_identical_text() {
        let processor = PostProcessor::new(Some("test-key-never-used".to_string()));
        let cell_a = {
            let mut inflight = processor.inflight.lock();
            Arc::clone(
                inflight
                    .entry("same text".to_string())
                    .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new())),
            )
        };
        let cell_b = {
            let mut inflight = processor.inflight.lock();
            Arc::clone(
                inflight
                    .entry("same text".to_string())
                    .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new())),
            )
        };
        assert!(Arc::ptr_eq(&cell_a, &cell_b));
        cell_a.set("Corrected.".to_string()).unwrap();
        assert_eq!(cell_b.get().map(String::as_str), Some("Corrected."));
    }

    #[test]
    fn sanitize_corrected_text_removes_model_wrappers() {
        let out = sanitize_corrected_text(
            "can you check this",
            "Here is the cleaned transcript: \"Can you check this?\"",
        )
        .unwrap();
        assert_eq!(out, "Can you check this?");
    }

    #[test]
    fn sanitize_corrected_text_extracts_transcript_tag() {
        let out =
            sanitize_corrected_text("run the tests", "<transcript>Run the tests.</transcript>")
                .unwrap();
        assert_eq!(out, "Run the tests.");
    }

    #[test]
    fn sanitize_corrected_text_rejects_divergent_answer() {
        let out = sanitize_corrected_text(
            "can you explain the logs",
            "Sure, the issue is probably caused by your microphone, and here are several steps you can try.",
        );
        assert!(out.is_none());
    }

    #[test]
    fn sanitize_corrected_text_rejects_model_appended_gratitude() {
        assert!(
            sanitize_corrected_text("Send the report.", "Send the report. Thank you.").is_none()
        );
        assert!(sanitize_corrected_text("Run the tests", "Run the tests. Thanks a lot.").is_none());
    }

    #[test]
    fn sanitize_corrected_text_preserves_spoken_gratitude() {
        assert_eq!(
            sanitize_corrected_text("Send the report, thank you.", "Send the report, thank you.")
                .as_deref(),
            Some("Send the report, thank you.")
        );
        assert_eq!(
            sanitize_corrected_text("Thanks", "Thanks").as_deref(),
            Some("Thanks")
        );
    }

    #[test]
    fn context_echo_is_detected() {
        let ctx = "I went to the store yesterday and bought";
        assert!(echoes_context(
            ctx,
            "I went to the store yesterday and bought milk and eggs."
        ));
        // A genuine continuation does not echo.
        assert!(!echoes_context(ctx, "milk and eggs for breakfast."));
        // Short context can't reliably signal an echo — never reject.
        assert!(!echoes_context("Hi.", "Hi. How are you?"));
    }

    #[test]
    fn cache_distinguishes_same_text_under_different_context() {
        // Same transcript, different context → different user message →
        // different cache entries. A collision here would paste a
        // correction tuned for the WRONG surrounding text.
        let processor = PostProcessor::new(Some("test-key-never-used".to_string()));
        processor.insert_cached(
            "<context_before>Hello.</context_before>\n<transcript>and more</transcript>".into(),
            "And more.".into(),
        );
        let other_key = "<transcript>and more</transcript>";
        assert!(!processor.cache.lock().contains_key(other_key));
    }
}
