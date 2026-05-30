# Audit: text-proc scope

Files audited (read in full):
- src/always/postprocess.rs
- src/always/filter.rs
- src/always/text_match.rs
- src/always/hallucination.rs
- src/always/filter_config.rs

Callers traced: event_loop.rs:322 / :582-595 (filter + apply_custom_words), speech_action.rs:153/160 (filter + is_hallucination), config.rs:353 (PostProcessor), event_loop.rs:623/688 (pp.process). Confirmed `regex = "1"`, `strsim = "0.11"` in Cargo.toml.

---

### [MEDIUM] User-supplied filter regex compiled with no size limit + no transcript-length cap
- file: src/always/filter_config.rs:119-121 (compile site); src/always/filter.rs:80-89 (match site)
- category: performance
- confidence: medium
- impact: Anything that can write `~/.config/always/filter_config.json` can supply a regex that expands to a large compiled program (default crate limit is 10 MiB), consuming memory at daemon startup. Verified `regex = "1"` — the Rust `regex` crate guarantees linear-time matching, so classic exponential ReDoS does NOT apply, but there is no explicit cap and no transcript-length bound on the matched input.
- problem: Patterns are compiled with bare `Regex::new` (no size_limit) and matched on every transcript with no length cap (verified: no input-length cap at the call sites before these run):
  ```rust
  // filter_config.rs:119
  pub fn compile_regex_patterns(patterns: &[String]) -> Vec<Regex> {
      patterns.iter().filter_map(|p| Regex::new(p).ok()).collect()
  }
  ```
  ```rust
  // filter.rs:80
  for (i, regex) in COMPILED_REGEX.iter().enumerate() {
      if regex.is_match(&substituted) { ... }
  }
  ```
  The default regexes (filter_config.rs:82-93) use unanchored `.*` wildcards (`.*amara\.org.*`, etc.); linear but repeatedly scan the full string.
- fix: Compile with an explicit smaller program-size cap and bound the haystack length before scanning (slice on a char boundary):
  ```rust
  regex::RegexBuilder::new(p).size_limit(1 << 20).build().ok()
  ```

### [MEDIUM] Hallucination per-segment `compression_ratio` gate rejects whole transcript on one bad segment (data loss)
- file: src/always/hallucination.rs:166-184
- category: data-loss
- confidence: medium
- impact: Legitimate dictation is silently dropped (nothing pasted) whenever ANY single Whisper segment reports `compression_ratio > 2.4` — which happens for normal-but-repetitive valid speech (lists, "very very important", code with repeated tokens). For an always-on dictation tool this is real, recurring data loss.
- problem:
  ```rust
  for seg in &result.segments {
      ...
      let repetitive = seg.compression_ratio > 2.4;
      ...
      if repetitive {
          return Some("repetitive (compression ratio)");
      }
  }
  ```
  A single bad segment in a multi-segment utterance rejects the entire transcript, with only a log line. The `repetitive` check has no corroborating confidence signal (unlike the `no_speech && low_conf` and `fallback && very_low_conf` checks beside it).
- fix: Require a corroborating signal, e.g. `repetitive && (seg.no_speech_prob > 0.5 || seg.avg_logprob < -0.5)`, or only reject when a strong majority of segments are repetitive rather than any single one.

### [MEDIUM] `find_best_match` length heuristic uses byte length but `strsim::levenshtein` uses char count — inconsistent scoring on non-ASCII glossary
- file: src/always/text_match.rs:143, 154-163
- category: correctness
- confidence: medium
- impact: Multi-byte custom vocabulary terms (accented brand names, non-Latin words) are wrongly fast-rejected or get a distorted match score, because the normalization denominator (byte length) is on a different scale than the Levenshtein distance numerator (char count).
- problem:
  ```rust
  if candidate.is_empty() || candidate.len() > MAX_CANDIDATE_LEN { return None; } // bytes
  ...
  let len_diff =
      (candidate.len() as i32 - custom_word_nospace.len() as i32).unsigned_abs() as f64; // bytes
  let max_len = candidate.len().max(custom_word_nospace.len()) as f64;                   // bytes
  let dist = levenshtein(candidate, custom_word_nospace) as f64;                         // chars
  let lev_score = if max_len > 0.0 { dist / max_len } else { 1.0 };
  ```
  `.len()` is UTF-8 bytes; `strsim::levenshtein` counts chars. For a 5-char accented word that is 8 bytes, `lev_score = chars/bytes` is scaled wrong, distorting the `< threshold` decision. `MAX_CANDIDATE_LEN = 50` bytes also truncates legitimate non-ASCII candidates earlier than intended.
- fix: Use `candidate.chars().count()` consistently for the length checks and the `max_len` denominator so the numerator/denominator scales match.

### [MEDIUM] `soundex` recomputed up to 3× per candidate inside the hot inner loop
- file: src/always/text_match.rs:165-166
- category: performance
- confidence: high
- impact: For each transcript word-window and each custom word, `soundex()` (which allocates a `String`) is computed twice on the candidate and once on the custom word; `soundex(candidate)` is independent of the loop variable yet recomputed for every glossary entry. With a large glossary this is a hot, allocation-heavy path on the transcription-latency budget.
- problem:
  ```rust
  let phonetic =
      soundex(candidate) == soundex(custom_word_nospace) && !soundex(candidate).is_empty();
  ```
  `soundex(candidate)` appears twice and is loop-invariant; `soundex(custom_word_nospace)` recomputes the same value on every window.
- fix: Hoist `let cand_sx = soundex(candidate);` above the loop and precompute `custom_words_soundex` once at function entry (alongside `custom_words_nospace`). Then `let phonetic = !cand_sx.is_empty() && cand_sx == custom_sx[i];`.

### [MEDIUM] LLM-corrected text from Groq is pasted/cached verbatim with no plausibility check
- file: src/always/postprocess.rs:103-114
- category: correctness
- confidence: medium
- impact: Whatever Groq returns is trimmed and pasted into the focused app (and cached). If the model echoes the `<transcript>` wrapper, returns a refusal ("I cannot..."), or returns something much longer than the input, that surprising text lands in the user's document.
- problem:
  ```rust
  let corrected = json["choices"][0]["message"]["content"]
      .as_str()
      .context("Invalid Groq response format")?
      .trim()
      .to_string();
  self.insert_cached(text.to_string(), corrected.clone());
  Ok(corrected)
  ```
  No sanity check that the correction is a plausible cleanup of the input (length ratio, no added meta-commentary, no leftover `<transcript>` tags).
- fix: Fall back to the raw input when the response is empty, contains `<transcript>`, or is implausibly long, e.g. `if corrected.is_empty() || corrected.contains("<transcript>") || corrected.chars().count() > text.chars().count() * 3 + 50 { return Ok(text.to_string()); }`.

### [LOW] STT-correction substitutions in the filter are unanchored substring replaces (false rejects)
- file: src/always/filter.rs:50-59
- category: correctness
- confidence: high
- impact: A legitimate transcript can be mangled into a blocked phrase before the filter comparison and then wrongly rejected (the spoken words are dropped). Note: this only affects the filter's internal comparison copy, not the pasted text, so it causes false rejects rather than corrupting output.
- problem:
  ```rust
  let substitutions = [
      (" u ", " you "),
      ("thank u", "thank you"),
      ("thankyou", "thank you"),
      ("ur ", "your "),
  ];
  for (from, to) in &substitutions {
      substituted = substituted.replace(from, to);
  }
  ```
  `"ur "` is unanchored, so `"blur report"` → `"blyour report"`; `" u "` won't catch a leading `"u "` at string start (no leading space). After mangling, a sentence may `starts_with`/`==` a blocked phrase and be dropped.
- fix: Tokenize on whitespace and remap whole tokens (`"u"→"you"`, `"ur"→"your"`) instead of raw `str::replace`; anchor `thank u`/`thankyou` to the start.

### [LOW] Cache key is the unbounded raw transcript — stated "~1 MB" memory bound is not enforced
- file: src/always/postprocess.rs:13, 70, 113, 121-141
- category: resource-leak
- confidence: medium
- impact: The doc comment claims "1 000 entries ~= 1 MB upper bound" assuming each entry ≤ ~500 chars, but the cache KEY is the unbounded input `text` (no input-length cap anywhere in this scope). Long transcripts make individual entries far larger than assumed, so 1 000 entries can exceed the stated bound substantially.
- problem:
  ```rust
  const CACHE_MAX_ENTRIES: usize = 1_000; // "1 000 entries ~= 1 MB upper bound"
  ...
  self.insert_cached(text.to_string(), corrected.clone()); // key = full input
  ```
  Eviction bounds entry COUNT, not total bytes; the memory claim only holds for small inputs, which is not enforced.
- fix: Skip caching oversized inputs (`if text.len() > 2048 { return Ok(corrected); }` before insert) or evict on total byte size rather than count.

### [LOW] `load_filter_config` silently falls back to defaults when an existing config file fails to parse
- file: src/always/filter_config.rs:97-117
- category: error-handling
- confidence: high
- impact: A user who edits `filter_config.json` and makes a JSON typo silently gets the built-in defaults; their custom filters don't apply and the only signal is a `debug!` line that is indistinguishable from "no file present".
- problem:
  ```rust
  if path.exists()
      && let Ok(content) = std::fs::read_to_string(&path)
      && let Ok(config) = serde_json::from_str::<FilterConfig>(&content)
  { ... return Some(config); }
  ...
  tracing::debug!("filter_config_using_defaults"); // even when a present file failed to parse
  ```
- fix: Split the conditions so a present-but-unparseable file emits `tracing::warn!(path, error, "filter_config_parse_failed")` before falling back to defaults.

### [LOW] Hallucination "too short" floor (`chars().count() < 3`) drops valid short dictation
- file: src/always/hallucination.rs:154-155
- category: data-loss
- confidence: medium
- impact: Valid short utterances ("no", "ok", "hi", "AI", single-digit numbers, "C") are always rejected — data loss that directly conflicts with the module's own documented intent (lines 75-82: "the user is the authority on whether they meant to say them").
- problem:
  ```rust
  if text.chars().count() < 3 {
      return Some("too short");
  }
  ```
  The comment block at lines 75-82 explicitly says affirmatives/negatives like `yes/no/ok` should be allowed, yet this floor unconditionally drops everything ≤2 chars. The test at line 490 even concedes "`ok`/`no` still trip the 3-char floor … that gate is intentional" — but for an always-on dictation tool this is a frequent, real data-loss path.
- fix: Lower the floor to 1 char and rely on the segment-metric / `no_speech_prob` layers to catch true silence, or only apply the short gate when segment confidence is also low.

### [LOW] `is_hallucination` lowercases the full text twice per call
- file: src/always/hallucination.rs:203, 210
- category: performance
- confidence: low
- impact: Minor redundant allocation on the hot path: `normalize(text)` at line 203 already lowercases, then line 210 builds another full lowercase copy for the substring scan.
- problem:
  ```rust
  let normalized = normalize(text);        // line 203 — already lowercased
  ...
  let lower = text.to_lowercase();         // line 210 — second full lowercase copy
  for needle in HALLUCINATION_SUBSTRINGS { if lower.contains(needle) { ... } }
  ```
- fix: Reuse a single lowercased copy for both the normalize step and the substring scan (kept the original punctuation intact only where needed for `amara.org`).

### [LOW] `extract_punctuation` treats an all-punctuation leading token as a prefix
- file: src/always/text_match.rs:204-223
- category: correctness
- confidence: low
- impact: In a multi-token matched window whose first token is pure punctuation, the entire punctuation token is concatenated as the replacement's prefix, producing incorrect surrounding punctuation. Requires a punctuation-only leading token in a matched window, hence low severity.
- problem:
  ```rust
  let prefix_end = word.char_indices().find(|(_,c)| c.is_alphanumeric())
      .map(|(i,_)| i).unwrap_or(word.len());   // no alnum -> prefix = whole token
  ```
  When `window[0]` has no alphanumerics, `prefix` becomes the whole token and is glued in front of the canonical replacement.
- fix: Only run prefix/suffix extraction on tokens that contain ≥1 alphanumeric; otherwise treat the punctuation token as a standalone token.

---

## Verified-clean / non-findings
- text_match.rs slicing (`&word[..prefix_end]`, `&word[suffix_start..]`) uses `char_indices`/`len_utf8`, so it is char-boundary safe — no panic risk.
- hallucination.rs `normalize`, `chars().windows(4)`, and all per-char filters operate on `char`s, not byte indices — no boundary panics on multi-byte input.
- postprocess.rs `insert_cached` always takes `cache` then `cache_order` in the same order; no lock-ordering deadlock within this module.
- `soundex` returns empty for empty/no-alpha input and caps output at 4 chars — no panic/overflow; ReDoS is not applicable (Rust `regex` crate is linear-time).
