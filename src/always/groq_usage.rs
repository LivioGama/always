//! Today's Groq usage, for the Settings → Models usage bar (SPEC §9,
//! owner request 2026-11-25).
//!
//! Groq has no usage or spend API, so the daemon keeps its own tally per
//! UTC day: billed audio seconds for Whisper, tokens for the LLM, and the
//! requests-per-day headers Groq returns. From that it derives
//!
//! * the plan: Groq's free plan has a requests-per-day limit in the low
//!   thousands, the paid Developer plan in the hundreds of thousands;
//! * how much of the free daily allowance is used (the free plan answers
//!   429 past it, it never bills);
//! * on the paid plan, an estimate of today's spend from Groq's published
//!   prices (every request is billed there, there is no free allowance).
//!
//! The tally lives in `config_dir()/groq_usage.json`, so a daemon restart
//! (every model or Postprocess mode change) does not zero it.

use std::path::PathBuf;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::event::{DaemonEvent, global_broadcaster};

/// Free plan, Whisper models: audio seconds per day (8 h).
pub const FREE_AUDIO_SECONDS_PER_DAY: f64 = 28_800.0;
/// Free plan, gpt-oss models: tokens per day.
pub const FREE_LLM_TOKENS_PER_DAY: u64 = 200_000;
/// Groq bills every transcription request at least this long.
const MIN_BILLED_AUDIO_SECONDS: f64 = 10.0;
/// A requests-per-day limit above this is a paid plan (free: 1K–14.4K,
/// Developer: 200K+).
const PAID_RPD_THRESHOLD: u64 = 50_000;

/// USD per hour of audio.
fn whisper_price_per_hour(model: &str) -> f64 {
    if model.contains("turbo") { 0.04 } else { 0.111 }
}

/// USD per million (input, output) tokens. Unknown models are priced
/// like gpt-oss-120b, the default and the dearest of the current set.
fn llm_price_per_million(model: &str) -> (f64, f64) {
    if model.contains("gpt-oss-20b") {
        (0.075, 0.30)
    } else {
        (0.15, 0.60)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct Tally {
    /// Days since the Unix epoch (UTC).
    day: u64,
    audio_seconds: f64,
    llm_tokens: u64,
    cost_usd: f64,
    stt_requests_limit: Option<u64>,
    stt_requests_remaining: Option<u64>,
    llm_requests_limit: Option<u64>,
    llm_requests_remaining: Option<u64>,
}

static TALLY: Mutex<Option<Tally>> = Mutex::new(None);

fn path() -> PathBuf {
    crate::config::config_dir().join("groq_usage.json")
}

fn today() -> u64 {
    chrono::Utc::now().timestamp().max(0) as u64 / 86_400
}

/// Run `f` on today's tally (loading it, or starting a new day), persist
/// it, and return the event describing the result.
fn update(f: impl FnOnce(&mut Tally)) -> DaemonEvent {
    let mut guard = TALLY.lock();
    let day = today();
    let tally = guard.get_or_insert_with(|| load().unwrap_or_default());
    if tally.day != day {
        *tally = Tally {
            day,
            ..Tally::default()
        };
    }
    f(tally);
    if let Ok(json) = serde_json::to_vec(tally) {
        let _ = std::fs::write(path(), json);
    }
    event_for(tally)
}

fn load() -> Option<Tally> {
    serde_json::from_slice(&std::fs::read(path()).ok()?).ok()
}

fn plan(t: &Tally) -> &'static str {
    match t.stt_requests_limit.max(t.llm_requests_limit) {
        None => "unknown",
        Some(limit) if limit > PAID_RPD_THRESHOLD => "paid",
        Some(_) => "free",
    }
}

fn event_for(t: &Tally) -> DaemonEvent {
    DaemonEvent::GroqUsage {
        plan: plan(t).to_string(),
        audio_seconds: t.audio_seconds,
        audio_seconds_free: FREE_AUDIO_SECONDS_PER_DAY,
        llm_tokens: t.llm_tokens,
        llm_tokens_free: FREE_LLM_TOKENS_PER_DAY,
        stt_requests_limit: t.stt_requests_limit,
        stt_requests_remaining: t.stt_requests_remaining,
        llm_requests_limit: t.llm_requests_limit,
        llm_requests_remaining: t.llm_requests_remaining,
        cost_usd: t.cost_usd,
    }
}

fn header_u64(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

/// `(limit, remaining)` requests per day from Groq's rate-limit headers.
fn rpd(headers: &reqwest::header::HeaderMap) -> (Option<u64>, Option<u64>) {
    (
        header_u64(headers, "x-ratelimit-limit-requests"),
        header_u64(headers, "x-ratelimit-remaining-requests"),
    )
}

/// Length of a PCM WAV clip in seconds, from its `fmt ` byte rate and its
/// `data` chunk size. `None` when the bytes are not a WAV we can read.
#[must_use]
pub fn wav_seconds(wav: &[u8]) -> Option<f64> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12;
    let mut byte_rate = None;
    while pos + 8 <= wav.len() {
        let id = &wav[pos..pos + 4];
        let size = u32::from_le_bytes(wav[pos + 4..pos + 8].try_into().ok()?) as usize;
        let body = pos + 8;
        if id == b"fmt " && body + 12 <= wav.len() {
            byte_rate = Some(u32::from_le_bytes(
                wav[body + 8..body + 12].try_into().ok()?,
            ));
        } else if id == b"data" {
            let data = size.min(wav.len() - body);
            return byte_rate
                .filter(|r| *r > 0)
                .map(|r| data as f64 / f64::from(r));
        }
        pos = body + size + (size & 1);
    }
    None
}

/// A Whisper request finished (`ok` = Groq accepted and billed it).
pub fn record_transcription(
    headers: &reqwest::header::HeaderMap,
    model: &str,
    audio_seconds: Option<f64>,
    ok: bool,
) {
    let (limit, remaining) = rpd(headers);
    let event = update(|t| {
        if limit.is_some() {
            t.stt_requests_limit = limit;
            t.stt_requests_remaining = remaining;
        }
        if ok {
            let billed = audio_seconds.unwrap_or(0.0).max(MIN_BILLED_AUDIO_SECONDS);
            t.audio_seconds += billed;
            t.cost_usd += billed / 3600.0 * whisper_price_per_hour(model);
        }
    });
    global_broadcaster().send(event);
}

/// A chat-completion request finished; `usage` is the response's `usage`
/// object when it succeeded.
pub fn record_completion(
    headers: &reqwest::header::HeaderMap,
    model: &str,
    usage: Option<&serde_json::Value>,
) {
    let (limit, remaining) = rpd(headers);
    let tokens = |key: &str| usage.and_then(|u| u[key].as_u64()).unwrap_or(0);
    let (input, output) = (tokens("prompt_tokens"), tokens("completion_tokens"));
    let event = update(|t| {
        if limit.is_some() {
            t.llm_requests_limit = limit;
            t.llm_requests_remaining = remaining;
        }
        let (in_price, out_price) = llm_price_per_million(model);
        t.llm_tokens += input + output;
        t.cost_usd += (input as f64 * in_price + output as f64 * out_price) / 1_000_000.0;
    });
    global_broadcaster().send(event);
}

/// Today's usage for a client's initial burst; `None` when Groq has not
/// been used today.
#[must_use]
pub fn initial_burst_event() -> Option<DaemonEvent> {
    let mut guard = TALLY.lock();
    let tally = guard.get_or_insert_with(|| load().unwrap_or_default());
    (tally.day == today()).then(|| event_for(tally))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    fn wav(byte_rate: u32, data_len: usize) -> Vec<u8> {
        let mut v = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&[1, 0, 1, 0]); // PCM, mono
        v.extend_from_slice(&16_000u32.to_le_bytes());
        v.extend_from_slice(&byte_rate.to_le_bytes());
        v.extend_from_slice(&[2, 0, 16, 0]);
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data_len as u32).to_le_bytes());
        v.extend(std::iter::repeat_n(0u8, data_len));
        v
    }

    #[test]
    fn wav_length_from_header() {
        assert_eq!(wav_seconds(&wav(32_000, 64_000)), Some(2.0));
        assert_eq!(wav_seconds(b"not a wav"), None);
        assert_eq!(wav_seconds(&wav(0, 10)), None);
    }

    #[test]
    fn plan_follows_the_requests_per_day_limit() {
        let mut t = Tally::default();
        assert_eq!(plan(&t), "unknown");
        t.stt_requests_limit = Some(2_000);
        assert_eq!(plan(&t), "free");
        t.llm_requests_limit = Some(500_000);
        assert_eq!(plan(&t), "paid");
    }

    #[test]
    fn rpd_headers_parse() {
        let mut h = HeaderMap::new();
        h.insert(
            "x-ratelimit-limit-requests",
            HeaderValue::from_static("2000"),
        );
        h.insert(
            "x-ratelimit-remaining-requests",
            HeaderValue::from_static(" 1840"),
        );
        assert_eq!(rpd(&h), (Some(2000), Some(1840)));
        assert_eq!(rpd(&HeaderMap::new()), (None, None));
    }

    #[test]
    fn prices_match_groq_published_rates() {
        assert_eq!(whisper_price_per_hour("whisper-large-v3-turbo"), 0.04);
        assert_eq!(whisper_price_per_hour("whisper-large-v3"), 0.111);
        assert_eq!(llm_price_per_million("openai/gpt-oss-20b"), (0.075, 0.30));
        assert_eq!(llm_price_per_million("openai/gpt-oss-120b"), (0.15, 0.60));
    }
}
