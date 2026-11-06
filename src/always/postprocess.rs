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

    #[cfg(test)]
    pub fn insert_cached_for_test(&self, key: &str, value: &str) {
        self.insert_cached(key.to_string(), value.to_string());
    }

    /// Non-blocking cache lookup for a prepared request: `Some` only when
    /// an earlier (speculative) LLM pass for exactly this request has
    /// already finished. The paste path uses this and nothing else — it
    /// never waits for the LLM.
    pub fn peek_cached(
        &self,
        req: &crate::always::correction_request::CorrectionRequest,
    ) -> Option<String> {
        if !self.config.grammar_correction_enabled {
            return None;
        }
        self.cache.lock().get(&req.user_message).cloned()
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
        // A warm already finished (cached) OR is still in flight (the call
        // below joins its single-flight cell) — both mean the paste reused
        // the warm instead of paying a cold LLM call.
        let cache_hit = self.cache.lock().contains_key(user_message)
            || self.inflight.lock().contains_key(user_message);
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

        let raw_corrected =
            result.map_err(|e| anyhow::anyhow!("Apple Intelligence call failed: {e}"))?;
        let corrected = sanitize_corrected_text(transcript, &raw_corrected)
            .filter(|cleaned| {
                let echoed = context.is_some_and(|ctx| echoes_context(ctx, cleaned));
                if echoed {
                    tracing::warn!(
                        stage = "grammar_correction",
                        "rejected context echo (apple)"
                    );
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
            .json(&grammar_request_body(
                &self.config.groq_model,
                &crate::glossary::postprocess_system_prompt(),
                user_message,
            ))
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

    if drops_spoken_content(input, cleaned) {
        tracing::warn!(
            stage = "grammar_correction",
            "rejected correction that drops spoken words"
        );
        return None;
    }

    Some(cleaned.to_string())
}

/// Hesitation sounds the correction may always remove.
const DISFLUENCY_WORDS: &[&str] = &["um", "uh", "ah", "eh", "er", "erm", "hmm", "mm", "uhm"];
/// Longest run of consecutive spoken words a correction may delete — a
/// false start ("go to the, go to the store") or a duplicate.
const MAX_DROPPED_RUN: usize = 4;

/// The correction deleted words the user actually said, beyond removing
/// hesitations and a short false start. The word-count guard above only
/// bounds the ratio, so a long chunk could lose a whole clause and pass:
/// 2026-10-14 14:48:18Z the Apple pass turned "Well, for anything I say,
/// uh, I mean, anything I explicitly say, uh, she should not ask at all"
/// into "Well, for anything I say, she should not ask at all" (23 of 31
/// words, above the 2/3 floor) and the clause the user meant was gone.
///
/// Aligns the spoken (non-hesitation) input words against the output by
/// longest common subsequence and rejects when more than `MAX_DROPPED_RUN`
/// consecutive spoken words are unmatched, or when the total unmatched
/// exceeds max(`MAX_DROPPED_RUN`, 10 %). A word the model *replaced*
/// (acoustic fix, glossary rewrite) costs one unmatched word, so legitimate
/// edits stay well under both bounds. Rejection falls back to the raw
/// transcript — never to a guess.
fn drops_spoken_content(input: &str, output: &str) -> bool {
    let spoken: Vec<String> = normalized_words(input)
        .into_iter()
        .filter(|w| !DISFLUENCY_WORDS.contains(&w.as_str()))
        .collect();
    let out = normalized_words(output);
    let n = spoken.len();
    let m = out.len();
    if n == 0 {
        return false;
    }
    // GRAMMAR_MAX_CHARS bounds this at a few hundred words per side.
    let mut lcs = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[at(i, j)] = if spoken[i] == out[j] {
                lcs[at(i + 1, j + 1)] + 1
            } else {
                lcs[at(i + 1, j)].max(lcs[at(i, j + 1)])
            };
        }
    }
    let (mut i, mut j) = (0usize, 0usize);
    let (mut run, mut longest, mut dropped) = (0usize, 0usize, 0usize);
    while i < n {
        if j < m && spoken[i] == out[j] && lcs[at(i, j)] == lcs[at(i + 1, j + 1)] + 1 {
            i += 1;
            j += 1;
            run = 0;
        } else if j < m && lcs[at(i, j + 1)] >= lcs[at(i + 1, j)] {
            // Output-only word (an insertion or a replacement's new form).
            j += 1;
        } else {
            dropped += 1;
            run += 1;
            longest = longest.max(run);
            i += 1;
        }
    }
    longest > MAX_DROPPED_RUN || dropped > MAX_DROPPED_RUN.max(n / 10)
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

/// Chat-completions body for the Groq grammar call.
///
/// gpt-oss models generate hidden reasoning tokens before the first
/// answer token, and this call sits on the paste path, so its length is
/// paste latency. Grammar cleanup of one dictated utterance needs little
/// deliberation: ask for the low reasoning effort and do not send the
/// reasoning text back. Both parameters are gpt-oss-only on Groq, so they
/// are omitted for any other configured model.
fn grammar_request_body(model: &str, system_prompt: &str, user_message: &str) -> Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_message }
        ],
        "temperature": 0.1,
        "max_tokens": 500
    });
    if model.contains("gpt-oss") {
        body["reasoning_effort"] = Value::from("low");
        body["include_reasoning"] = Value::from(false);
    }
    body
}

/// The single user-facing postprocess setting (Settings → Postprocessing).
///
/// - `Fast` — the deterministic corrector (`det_correct.rs`) inline before
///   every paste, ≤ 2 ms. No model, no network.
/// - `Apple` / `Groq` — `Fast` first (so the glossary is always honoured),
///   plus the LLM pass run SPECULATIVELY during speech; its result is used
///   only if it is already cached when the paste happens. The paste never
///   waits for it.
/// - `Off` — raw transcript + the minimal `local_cleanup` (fillers, repeats).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostprocessMode {
    Fast,
    Apple,
    Groq,
    Off,
}

impl PostprocessMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Apple => "apple",
            Self::Groq => "groq",
            Self::Off => "off",
        }
    }
    pub fn uses_llm(self) -> bool {
        matches!(self, Self::Apple | Self::Groq)
    }
}

impl std::fmt::Display for PostprocessMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for PostprocessMode {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fast" => Ok(Self::Fast),
            "apple" => Ok(Self::Apple),
            "groq" => Ok(Self::Groq),
            "off" => Ok(Self::Off),
            other => Err(format!("unknown postprocess mode: {other}")),
        }
    }
}

/// Resolve the effective mode. An explicit `postprocess_mode` wins;
/// otherwise migrate the legacy pair WITHOUT a silent behaviour change in
/// kind: Smart Postprocess on → that LLM provider's mode (Apple / Groq,
/// default provider Groq as before); Smart Postprocess off → Fast (the
/// deterministic corrector replaces the old bare local cleanup).
pub fn resolve_mode(
    mode: Option<&str>,
    legacy_enabled: Option<bool>,
    legacy_provider: Option<&str>,
) -> PostprocessMode {
    if let Some(m) = mode.and_then(|m| m.parse().ok()) {
        return m;
    }
    if !legacy_enabled.unwrap_or(true) {
        return PostprocessMode::Fast;
    }
    match legacy_provider.map(|p| p.trim().to_ascii_lowercase()) {
        Some(p) if p == "apple" => PostprocessMode::Apple,
        _ => PostprocessMode::Groq,
    }
}

static MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Publish the resolved mode (called when the config is loaded).
pub fn set_mode(mode: PostprocessMode) {
    let v = match mode {
        PostprocessMode::Fast => 0,
        PostprocessMode::Apple => 1,
        PostprocessMode::Groq => 2,
        PostprocessMode::Off => 3,
    };
    MODE.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// The mode the paste path applies. Defaults to `Fast`.
pub fn mode() -> PostprocessMode {
    match MODE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => PostprocessMode::Apple,
        2 => PostprocessMode::Groq,
        3 => PostprocessMode::Off,
        _ => PostprocessMode::Fast,
    }
}

/// Strip everything except alphanumeric characters and lowercase the result.
/// Used to compare word cores so "Hello," and "hello" match.
fn alnum_core(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Fast, deterministic cleanup for transcripts. Runs synchronously on every
/// utterance: removes adjacent repeated words ("the the", "ok ok") and
/// conservative filler words ("um", "uh", "ah") without inventing content.
/// Used as the sole cleanup pass when LLM grammar is disabled, and as a
/// pre-pass before the LLM when it is enabled.
pub fn local_cleanup(text: &str) -> String {
    const FILLERS: &[&str] = &["um", "uh", "ah", "eh"];
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return text.trim().to_string();
    }

    let mut out: Vec<&str> = Vec::with_capacity(words.len());
    for w in words {
        let core = alnum_core(w);
        if !core.is_empty() && FILLERS.contains(&core.as_str()) {
            continue;
        }
        if let Some(&prev) = out.last()
            && alnum_core(prev) == core
            && !core.is_empty()
        {
            continue;
        }
        out.push(w);
    }

    out.join(" ")
}

#[cfg(test)]
mod grammar_request_body_tests {
    use super::grammar_request_body;

    #[test]
    fn gpt_oss_asks_for_low_reasoning_without_reasoning_text() {
        let body =
            grammar_request_body("openai/gpt-oss-120b", "sys", "<transcript>hi</transcript>");
        assert_eq!(body["reasoning_effort"], "low");
        assert_eq!(body["include_reasoning"], false);
        assert_eq!(
            body["messages"][1]["content"],
            "<transcript>hi</transcript>"
        );
    }

    #[test]
    fn other_models_do_not_get_gpt_oss_parameters() {
        let body = grammar_request_body("llama-3.1-8b-instant", "sys", "x");
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("include_reasoning").is_none());
    }
}

#[cfg(test)]
mod local_cleanup_tests {
    use super::local_cleanup;

    #[test]
    fn removes_adjacent_repeats() {
        assert_eq!(local_cleanup("the the cat"), "the cat");
        assert_eq!(local_cleanup("ok ok ok done"), "ok done");
    }

    #[test]
    fn removes_fillers() {
        assert_eq!(local_cleanup("um hello uh world"), "hello world");
    }

    #[test]
    fn preserves_punctuation_and_case() {
        assert_eq!(local_cleanup("Hello, hello world."), "Hello, world.");
    }

    #[test]
    fn leaves_different_words_alone() {
        assert_eq!(local_cleanup("hello world hello"), "hello world hello");
    }

    #[test]
    fn handles_empty() {
        assert_eq!(local_cleanup(""), "");
        assert_eq!(local_cleanup("   "), "");
    }
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

/// Offline latency harness for the Apple Intelligence grammar call: the
/// REAL bridge, the REAL glossary system prompt and the REAL request
/// builder, over the user's most recent transcripts. Run with
/// `cargo test --features local-stt --lib apple_grammar_latency_harness -- --ignored --nocapture`.
/// `ALWAYS_HARNESS_N` sets the sample size (default 24).
#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
mod apple_latency_harness {
    /// A/B variants interleaved per sentence so machine-load drift hits
    /// both arms equally. A = old (fresh session + guided), B = new (pooled
    /// prewarmed session + plain), C = B with the compact prompt (no
    /// "Domain vocabulary" section).
    /// `cargo test --features local-stt --lib apple_grammar_ab -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn apple_grammar_ab() {
        if !crate::always::apple_intelligence::check_availability() {
            return;
        }
        let n: usize = std::env::var("ALWAYS_HARNESS_N")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(12);
        let lines =
            std::fs::read_to_string(dirs::home_dir().unwrap().join(".always/transcripts.jsonl"))
                .unwrap();
        let texts: Vec<String> = lines
            .lines()
            .rev()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_string))
            .filter(|t| !crate::always::event_loop::is_short_utterance(t) && t.len() < 400)
            .take(n)
            .collect();
        let full = crate::glossary::postprocess_system_prompt();
        let compact = match (
            full.find("# Domain vocabulary"),
            full.find("# Domain vocabulary")
                .and_then(|i| full[i + 2..].find("\n# ").map(|j| i + 2 + j + 1)),
        ) {
            (Some(a), Some(b)) => format!("{}{}", &full[..a], &full[b..]),
            _ => full.clone(),
        };
        println!(
            "prompt chars full={} compact={}",
            full.chars().count(),
            compact.chars().count()
        );
        let pct = |v: &mut Vec<u128>, p: f64| {
            v.sort_unstable();
            v[((v.len() as f64 - 1.0) * p).round() as usize]
        };
        let run = |prompt: &str, label: &str| {
            crate::always::apple_intelligence::prewarm(prompt);
            std::thread::sleep(std::time::Duration::from_millis(2000));
            let (mut a, mut b) = (Vec::new(), Vec::new());
            for t in &texts {
                let req = crate::always::correction_request::build(&super::local_cleanup(t), true);
                let s = std::time::Instant::now();
                let _ = crate::always::apple_intelligence::process_with_mode(
                    &full,
                    &req.user_message,
                    2,
                );
                a.push(s.elapsed().as_millis());
                let s = std::time::Instant::now();
                let _ = crate::always::apple_intelligence::process_with_mode(
                    prompt,
                    &req.user_message,
                    1,
                );
                b.push(s.elapsed().as_millis());
            }
            println!(
                "AB {label}: old p50={} p95={} | new p50={} p95={} (n={})",
                pct(&mut a.clone(), 0.5),
                pct(&mut a, 0.95),
                pct(&mut b.clone(), 0.5),
                pct(&mut b, 0.95),
                texts.len()
            );
        };
        run(&full, "B pool+plain, full prompt");
        run(&compact, "C pool+plain, compact prompt");
    }

    #[test]
    #[ignore]
    fn apple_grammar_latency_harness() {
        if !crate::always::apple_intelligence::check_availability() {
            println!("apple intelligence unavailable — skipped");
            return;
        }
        let n: usize = std::env::var("ALWAYS_HARNESS_N")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(24);
        let path = dirs::home_dir().unwrap().join(".always/transcripts.jsonl");
        let lines = std::fs::read_to_string(path).expect("transcripts.jsonl");
        let mut texts: Vec<String> = lines
            .lines()
            .rev()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_string))
            .filter(|t| !crate::always::event_loop::is_short_utterance(t) && t.len() < 400)
            .take(n)
            .collect();
        texts.reverse();
        let system_prompt = crate::glossary::postprocess_system_prompt();
        println!(
            "system_prompt chars={} words={}",
            system_prompt.chars().count(),
            system_prompt.split_whitespace().count()
        );
        let warm = std::time::Instant::now();
        crate::always::apple_intelligence::prewarm(&system_prompt);
        println!("prewarm_ms={}", warm.elapsed().as_millis());
        // Prewarm is asynchronous (daemon start in production): let it land.
        std::thread::sleep(std::time::Duration::from_millis(2000));
        let mut ms: Vec<u128> = Vec::new();
        let mut dump = std::env::var("ALWAYS_HARNESS_OUT")
            .ok()
            .and_then(|p| std::fs::File::create(p).ok());
        for (i, t) in texts.iter().enumerate() {
            // Same shape as the paste path: local cleanup first, then the
            // request builder; local cleanup again on the LLM output.
            let t = &super::local_cleanup(t);
            let req = crate::always::correction_request::build(t, true);
            let started = std::time::Instant::now();
            let out = crate::always::apple_intelligence::process_text_with_system_prompt(
                &system_prompt,
                &req.user_message,
                500,
            );
            let e = started.elapsed().as_millis();
            ms.push(e);
            if let (Some(f), Ok(out)) = (dump.as_mut(), out.as_ref()) {
                use std::io::Write;
                let cleaned = super::sanitize_corrected_text(&req.acoustic_text, out)
                    .map(|c| super::local_cleanup(&c))
                    .unwrap_or_else(|| req.acoustic_text.clone());
                let _ = writeln!(
                    f,
                    "{}",
                    serde_json::json!({"input": req.acoustic_text, "llm": cleaned, "ms": e as u64})
                );
            }
            println!(
                "#{i:02} {e:>6} ms in_chars={} ok={} changed={}",
                req.user_message.chars().count(),
                out.is_ok(),
                out.as_ref().map(|o| o.trim() != t.trim()).unwrap_or(false)
            );
        }
        ms.sort_unstable();
        let pct = |p: f64| ms[((ms.len() as f64 - 1.0) * p).round() as usize];
        println!(
            "APPLE_GRAMMAR n={} p50={} ms p95={} ms max={} ms",
            ms.len(),
            pct(0.5),
            pct(0.95),
            ms.last().unwrap()
        );
    }
}

#[cfg(test)]
mod mode_tests {
    use super::*;

    #[test]
    fn explicit_mode_wins_and_parses() {
        assert_eq!(
            resolve_mode(Some("off"), Some(true), Some("apple")),
            PostprocessMode::Off
        );
        assert_eq!(
            resolve_mode(Some("FAST"), None, None),
            PostprocessMode::Fast
        );
        assert!("bogus".parse::<PostprocessMode>().is_err());
    }

    #[test]
    fn legacy_prefs_migrate_without_changing_kind() {
        // The user's prefs today: Smart Postprocess on + Apple provider.
        assert_eq!(
            resolve_mode(None, Some(true), Some("apple")),
            PostprocessMode::Apple
        );
        assert_eq!(
            resolve_mode(None, Some(true), Some("groq")),
            PostprocessMode::Groq
        );
        assert_eq!(resolve_mode(None, None, None), PostprocessMode::Groq);
        assert_eq!(
            resolve_mode(None, Some(false), Some("apple")),
            PostprocessMode::Fast
        );
        // An unparseable stored mode falls back to the legacy mapping.
        assert_eq!(
            resolve_mode(Some("x"), Some(false), None),
            PostprocessMode::Fast
        );
    }

    #[test]
    fn mode_global_round_trips() {
        for m in [
            PostprocessMode::Off,
            PostprocessMode::Apple,
            PostprocessMode::Groq,
            PostprocessMode::Fast,
        ] {
            set_mode(m);
            assert_eq!(mode(), m);
            assert_eq!(m.as_str().parse::<PostprocessMode>().unwrap(), m);
        }
    }
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
    fn correction_that_deletes_a_spoken_clause_is_rejected() {
        // Real chunk 0 of the 2026-10-14 14:48:18Z dictation and the
        // correction that was pasted.
        let input = "Well, for anything I say, uh, I mean, anything I explicitly say, uh, \
                     she should not ask at all, right? Generally asking this is not a very \
                     nice approach at all.";
        let lossy = "Well, for anything I say, she should not ask at all, right? Generally \
                     asking this is not a very nice approach at all.";
        assert!(drops_spoken_content(input, lossy));
        assert_eq!(
            sanitize_corrected_text(input, lossy),
            None,
            "falls back to raw"
        );
    }

    #[test]
    fn ordinary_cleanup_is_not_flagged_as_word_loss() {
        // Hesitations, a duplicated word, punctuation, case, one acoustic
        // fix and a short false start are all allowed.
        let input = "okay the incremental thing is not working uh and like uh on on this \
                     I was like speaking she did not even take any action";
        let out = "Okay, the incremental thing is not working, and like, on this, I was \
                   like speaking. She did not even take any action.";
        assert!(!drops_spoken_content(input, out));
        assert!(!drops_spoken_content(
            "you need to check the locks and fix everything",
            "You need to check the logs and fix everything."
        ));
        assert!(!drops_spoken_content(
            "so we go to the go to the store tomorrow",
            "So we go to the store tomorrow."
        ));
        assert!(!drops_spoken_content("", "anything"));
        assert!(sanitize_corrected_text(input, out).is_some());
    }

    #[test]
    fn many_scattered_deletions_are_rejected() {
        let input = "one two three four five six seven eight nine ten eleven twelve";
        // Six words gone, none consecutive beyond 1 — total bound trips.
        let out = "one three five seven nine eleven";
        assert!(drops_spoken_content(input, out));
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
