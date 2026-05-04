//! Speech-to-text with Groq Whisper API.

use anyhow::{Context, Result};
use reqwest::blocking::multipart as blocking_multipart;
use serde::Deserialize;
use std::sync::OnceLock;

use crate::glossary;

const GROQ_TRANSCRIPTIONS_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const WHISPER_MODEL: &str = "whisper-large-v3-turbo";

static AUTH_HEADER: OnceLock<String> = OnceLock::new();

fn auth_header(api_key: &str) -> &'static str {
    AUTH_HEADER.get_or_init(|| format!("Bearer {}", api_key))
}

/// Resolve the Groq transcription endpoint URL.
///
/// Honors the `ALWAYS_GROQ_BASE_URL` environment variable so integration tests
/// can point the client at a wiremock server. The override base URL is appended
/// with `/openai/v1/audio/transcriptions`. Defaults to the production Groq URL
/// when the variable is unset.
fn transcriptions_url() -> String {
    std::env::var("ALWAYS_GROQ_BASE_URL")
        .map(|base| {
            format!(
                "{}/openai/v1/audio/transcriptions",
                base.trim_end_matches('/')
            )
        })
        .unwrap_or_else(|_| GROQ_TRANSCRIPTIONS_URL.to_string())
}

#[derive(Debug, Clone, Deserialize)]
pub struct TranscriptionSegment {
    #[serde(default)]
    pub id: u32,
    #[serde(default)]
    pub start: f64,
    #[serde(default)]
    pub end: f64,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub no_speech_prob: f32,
    #[serde(default)]
    pub avg_logprob: f32,
    #[serde(default)]
    pub compression_ratio: f32,
    #[serde(default)]
    pub temperature: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TranscriptionResult {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub segments: Vec<TranscriptionSegment>,
}

pub fn transcribe_from_bytes(audio_data: Vec<u8>, api_key: &str) -> Result<TranscriptionResult> {
    let client = crate::http_client::blocking();

    let mut form = blocking_multipart::Form::new()
        .part(
            "file",
            blocking_multipart::Part::bytes(audio_data)
                .file_name("audio.wav")
                .mime_str("audio/wav")?,
        )
        .text("model", WHISPER_MODEL)
        .text("response_format", "verbose_json")
        .text("language", "en");

    if let Some(prompt) = glossary::whisper_bias_prompt() {
        form = form.text("prompt", prompt.clone());
    }

    let response = client
        .post(transcriptions_url())
        .header("Authorization", auth_header(api_key))
        .multipart(form)
        .send()
        .context("failed to send Groq Whisper API request")?
        .error_for_status()
        .context("Groq Whisper API returned an error")?;

    let mut result: TranscriptionResult = response
        .json()
        .context("failed to parse Groq Whisper response")?;

    result.text = result.text.trim().to_string();

    Ok(result)
}
