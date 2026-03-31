use std::path::Path;

use reqwest::multipart::{Form, Part};
use serde::Deserialize;

use super::{AsrConfig, AsrError};

pub struct OpenAITranscriptionClient {
    config: AsrConfig,
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct OpenAITranscriptionResponse {
    text: String,
}

impl OpenAITranscriptionClient {
    pub fn new(config: AsrConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    pub async fn transcribe_file(&self, path: &Path) -> Result<String, AsrError> {
        if !self.config.is_valid() {
            return Err(AsrError::ConnectionFailed(
                "Invalid OpenAI ASR configuration".to_string(),
            ));
        }

        let filename = multipart_filename(path);
        let mime = mime_type_for_filename(&filename);
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| AsrError::ConnectionFailed(format!("Failed to read audio file: {}", e)))?;

        let endpoint = format!(
            "{}/audio/transcriptions",
            self.config.openai_asr_base_url.trim_end_matches('/')
        );

        log::info!(
            "Starting OpenAI transcription (model={}, language={}, file={}, bytes={})",
            self.config.openai_asr_model,
            if self.config.openai_asr_language.trim().is_empty() {
                "auto"
            } else {
                self.config.openai_asr_language.as_str()
            },
            path.display(),
            bytes.len()
        );

        let file_part = Part::bytes(bytes)
            .file_name(filename)
            .mime_str(mime)
            .map_err(|e| AsrError::ProtocolError(format!("Invalid MIME type: {}", e)))?;

        let mut form = Form::new()
            .text("model", self.config.openai_asr_model.clone())
            .text("response_format", "json")
            .part("file", file_part);

        if !self.config.openai_asr_language.trim().is_empty() {
            form = form.text(
                "language",
                self.config.openai_asr_language.trim().to_string(),
            );
        }
        let prompt =
            build_transcription_prompt(self.config.openai_asr_prompt.trim(), &self.config.hotwords);
        if !prompt.is_empty() {
            form = form.text("prompt", prompt);
        }

        let response = self
            .http
            .post(endpoint)
            .bearer_auth(&self.config.openai_asr_api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;

        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| AsrError::ConnectionFailed(format!("Failed to read response: {}", e)))?;

        if !status.is_success() {
            let body_text = String::from_utf8_lossy(&body).to_string();
            return Err(AsrError::ServerError(format!(
                "OpenAI HTTP {}: {}",
                status.as_u16(),
                body_text
            )));
        }

        let parsed: OpenAITranscriptionResponse = serde_json::from_slice(&body)
            .map_err(|e| AsrError::ProtocolError(format!("Invalid response JSON: {}", e)))?;

        Ok(parsed.text.trim().to_string())
    }
}

pub(crate) fn build_transcription_prompt(base_prompt: &str, hotwords: &[String]) -> String {
    let mut sections = Vec::new();

    if !base_prompt.is_empty() {
        sections.push(base_prompt.to_string());
    }

    let normalized_hotwords: Vec<&str> = hotwords
        .iter()
        .map(|word| word.trim())
        .filter(|word| !word.is_empty())
        .collect();

    if !normalized_hotwords.is_empty() {
        let preview_limit = 200usize;
        let clipped = normalized_hotwords.len() > preview_limit;
        let joined = normalized_hotwords
            .iter()
            .take(preview_limit)
            .map(|word| format!("- {}", word))
            .collect::<Vec<_>>()
            .join("\n");

        let hotword_section = if clipped {
            format!(
                "Prefer these exact spellings when the audio plausibly refers to them:\n{}\n- ...",
                joined
            )
        } else {
            format!(
                "Prefer these exact spellings when the audio plausibly refers to them:\n{}",
                joined
            )
        };

        sections.push(hotword_section);
    }

    sections.join("\n\n")
}

fn multipart_filename(path: &Path) -> String {
    let raw_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("audio.wav");

    if raw_name.ends_with(".opus") {
        return raw_name.trim_end_matches(".opus").to_string() + ".ogg";
    }

    raw_name.to_string()
}

fn mime_type_for_filename(filename: &str) -> &'static str {
    match filename
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "flac" => "audio/flac",
        "mp3" | "mpeg" | "mpga" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "ogg" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "webm" => "audio/webm",
        _ => "application/octet-stream",
    }
}
