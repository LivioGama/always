//! Speech-to-text with Groq Whisper API.

use anyhow::{Context, Result};
use reqwest::blocking::multipart as blocking_multipart;
use reqwest::multipart;
use std::path::Path;

use crate::glossary;

/// Transcribe audio file using Groq Whisper API.
pub fn transcribe(audio_path: &str, _lang: Option<&str>, api_key: &str, _model: &str) -> Result<String> {
    if !Path::new(audio_path).exists() {
        anyhow::bail!("audio file not found: {audio_path}");
    }

    let audio_data = std::fs::read(audio_path)
        .with_context(|| format!("failed to read audio file: {audio_path}"))?;

    transcribe_from_bytes(audio_data, _lang, api_key, _model)
}

/// Transcribe audio from raw WAV bytes (eliminates file I/O)
pub fn transcribe_from_bytes(audio_data: Vec<u8>, _lang: Option<&str>, api_key: &str, _model: &str) -> Result<String> {
    // Use faster client configuration
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))  // Reduce from default 30s
        .connect_timeout(std::time::Duration::from_secs(5))
        .pool_max_idle_per_host(5)
        .pool_idle_timeout(std::time::Duration::from_secs(60))
        .build()
        .context("Failed to create optimized HTTP client")?;

    let mut form = blocking_multipart::Form::new()
        .part("file", blocking_multipart::Part::bytes(audio_data).file_name("audio.wav").mime_str("audio/wav")?)
        .text("model", "whisper-large-v3")
        .text("response_format", "text")
        .text("language", "en");

    if let Some(prompt) = glossary::whisper_bias_prompt() {
        form = form.text("prompt", prompt.clone());
    }

    let response = client
        .post("https://api.groq.com/openai/v1/audio/transcriptions")
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .send()
        .context("failed to send Groq Whisper API request")?
        .error_for_status()
        .context("Groq Whisper API returned an error")?;

    let text = response.text().context("failed to read Groq Whisper response")?;

    Ok(text.trim().to_string())
}

/// Async version for better performance
pub async fn transcribe_from_bytes_async(audio_data: Vec<u8>, _lang: Option<&str>, api_key: &str, _model: &str) -> Result<String> {
    let client = reqwest::Client::new();

    let mut form = multipart::Form::new()
        .part("file", multipart::Part::bytes(audio_data).file_name("audio.wav").mime_str("audio/wav")?)
        .text("model", "whisper-large-v3")
        .text("response_format", "text")
        .text("language", "en");

    if let Some(prompt) = glossary::whisper_bias_prompt() {
        form = form.text("prompt", prompt.clone());
    }

    let response = client
        .post("https://api.groq.com/openai/v1/audio/transcriptions")
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .send()
        .await
        .context("failed to send Groq Whisper API request")?
        .error_for_status()
        .context("Groq Whisper API returned an error")?;

    let text = response.text().await.context("failed to read Groq Whisper response")?;

    Ok(text.trim().to_string())
}
