use std::path::Path;

use reqwest::multipart::{Form, Part};
use serde::Deserialize;

use super::{AsrConfig, AsrError};

const COHERE_TRANSCRIPTIONS_URL: &str = "https://api.cohere.com/v2/audio/transcriptions";

pub struct CohereTranscriptionClient {
    config: AsrConfig,
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct CohereTranscriptionResponse {
    text: String,
}

impl CohereTranscriptionClient {
    pub fn new(config: AsrConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    pub async fn transcribe_file(&self, path: &Path) -> Result<String, AsrError> {
        if !self.config.is_valid() {
            return Err(AsrError::ConnectionFailed(
                "Invalid Cohere ASR configuration".to_string(),
            ));
        }

        let filename = multipart_filename(path);
        let mime = mime_type_for_filename(&filename);
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| AsrError::ConnectionFailed(format!("Failed to read audio file: {}", e)))?;

        log::info!(
            "Starting Cohere transcription (model={}, language={}, file={}, bytes={})",
            self.config.cohere_model,
            self.config.cohere_language,
            path.display(),
            bytes.len()
        );

        let file_part = Part::bytes(bytes)
            .file_name(filename)
            .mime_str(mime)
            .map_err(|e| AsrError::ProtocolError(format!("Invalid MIME type: {}", e)))?;
        let form = Form::new()
            .text("model", self.config.cohere_model.clone())
            .text("language", self.config.cohere_language.clone())
            .part("file", file_part);

        let response = self
            .http
            .post(COHERE_TRANSCRIPTIONS_URL)
            .bearer_auth(&self.config.cohere_api_key)
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
                "Cohere HTTP {}: {}",
                status.as_u16(),
                body_text
            )));
        }

        let parsed: CohereTranscriptionResponse = serde_json::from_slice(&body)
            .map_err(|e| AsrError::ProtocolError(format!("Invalid response JSON: {}", e)))?;

        Ok(parsed.text.trim().to_string())
    }
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
        "ogg" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}
