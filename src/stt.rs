//! Speech-to-text with Groq Whisper API.

use anyhow::{Context, Result};
use reqwest::blocking::multipart as blocking_multipart;
use std::sync::OnceLock;

use crate::glossary;

const GROQ_TRANSCRIPTIONS_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const WHISPER_MODEL: &str = "whisper-large-v3-turbo";

static AUTH_HEADER: OnceLock<String> = OnceLock::new();

fn auth_header(api_key: &str) -> &'static str {
    AUTH_HEADER.get_or_init(|| format!("Bearer {}", api_key))
}

pub fn transcribe_from_bytes(audio_data: Vec<u8>, api_key: &str) -> Result<String> {
    let client = crate::http_client::blocking();

    let mut form = blocking_multipart::Form::new()
        .part(
            "file",
            blocking_multipart::Part::bytes(audio_data)
                .file_name("audio.wav")
                .mime_str("audio/wav")?,
        )
        .text("model", WHISPER_MODEL)
        .text("response_format", "text")
        .text("language", "en");

    if let Some(prompt) = glossary::whisper_bias_prompt() {
        form = form.text("prompt", prompt.clone());
    }

    let response = client
        .post(GROQ_TRANSCRIPTIONS_URL)
        .header("Authorization", auth_header(api_key))
        .multipart(form)
        .send()
        .context("failed to send Groq Whisper API request")?
        .error_for_status()
        .context("Groq Whisper API returned an error")?;

    let text = response
        .text()
        .context("failed to read Groq Whisper response")?;

    Ok(text.trim().to_string())
}
