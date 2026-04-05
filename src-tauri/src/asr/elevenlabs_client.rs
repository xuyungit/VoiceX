use std::collections::HashSet;
use std::path::Path;

use reqwest::multipart::{Form, Part};
use serde::Deserialize;

use super::{AsrConfig, AsrError};

const ELEVENLABS_TRANSCRIPTIONS_URL: &str = "https://api.elevenlabs.io/v1/speech-to-text";
const ELEVENLABS_TIMESTAMPS_GRANULARITY: &str = "word";
const ELEVENLABS_MAX_KEYTERMS: usize = 100;
const ELEVENLABS_MAX_KEYTERM_CHARS: usize = 49;
const ELEVENLABS_MAX_KEYTERM_WORDS: usize = 5;

pub struct ElevenLabsTranscriptionClient {
    config: AsrConfig,
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct ElevenLabsTranscriptionResponse {
    text: String,
}

#[derive(Debug, Deserialize)]
struct ElevenLabsErrorEnvelope {
    detail: Option<ElevenLabsErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct ElevenLabsErrorDetail {
    #[serde(rename = "type")]
    error_type: Option<String>,
    code: Option<String>,
    message: Option<String>,
    param: Option<String>,
    request_id: Option<String>,
}

impl ElevenLabsTranscriptionClient {
    pub fn new(config: AsrConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    pub async fn transcribe_file(&self, path: &Path) -> Result<String, AsrError> {
        if !self.config.is_valid() {
            return Err(AsrError::ConnectionFailed(
                "Invalid ElevenLabs ASR configuration".to_string(),
            ));
        }

        let filename = multipart_filename(path);
        let mime = mime_type_for_filename(&filename);
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| AsrError::ConnectionFailed(format!("Failed to read audio file: {}", e)))?;

        let keyterms = cleaned_elevenlabs_keyterms(
            &self.config.hotwords,
            self.config.elevenlabs_enable_keyterms,
        );

        log::info!(
            "Starting ElevenLabs batch transcription (model={}, language={}, file={}, bytes={}, keyterms={})",
            self.config.elevenlabs_batch_model,
            if self.config.elevenlabs_language.trim().is_empty() {
                "auto"
            } else {
                self.config.elevenlabs_language.as_str()
            },
            path.display(),
            bytes.len(),
            keyterms.len()
        );

        let file_part = Part::bytes(bytes)
            .file_name(filename)
            .mime_str(mime)
            .map_err(|e| AsrError::ProtocolError(format!("Invalid MIME type: {}", e)))?;

        let mut form = Form::new()
            .text("model_id", self.config.elevenlabs_batch_model.clone())
            .text(
                "timestamps_granularity",
                ELEVENLABS_TIMESTAMPS_GRANULARITY.to_string(),
            )
            .part("file", file_part);

        if !self.config.elevenlabs_language.trim().is_empty() {
            form = form.text(
                "language_code",
                self.config.elevenlabs_language.trim().to_string(),
            );
        }

        for keyterm in keyterms {
            form = form.text("keyterms", keyterm);
        }

        let response = self
            .http
            .post(ELEVENLABS_TRANSCRIPTIONS_URL)
            .header("xi-api-key", &self.config.elevenlabs_api_key)
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
            return Err(AsrError::ServerError(format_elevenlabs_error(
                status.as_u16(),
                &body,
            )));
        }

        let parsed: ElevenLabsTranscriptionResponse = serde_json::from_slice(&body)
            .map_err(|e| AsrError::ProtocolError(format!("Invalid response JSON: {}", e)))?;

        let text = parsed.text.trim().to_string();
        if text.is_empty() {
            return Err(AsrError::ServerError(
                "ElevenLabs returned an empty transcription result".to_string(),
            ));
        }

        Ok(text)
    }
}

pub(crate) fn cleaned_elevenlabs_keyterms(hotwords: &[String], enabled: bool) -> Vec<String> {
    if !enabled {
        return Vec::new();
    }

    let mut seen = HashSet::new();
    let mut keyterms = Vec::new();

    for hotword in hotwords {
        if keyterms.len() >= ELEVENLABS_MAX_KEYTERMS {
            break;
        }

        let normalized = normalize_keyterm(hotword);
        if normalized.is_empty() {
            continue;
        }
        if normalized.chars().count() > ELEVENLABS_MAX_KEYTERM_CHARS {
            continue;
        }
        if normalized.split(' ').count() > ELEVENLABS_MAX_KEYTERM_WORDS {
            continue;
        }
        if !seen.insert(normalized.clone()) {
            continue;
        }

        keyterms.push(normalized);
    }

    keyterms
}

fn normalize_keyterm(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn format_elevenlabs_error(status: u16, body: &[u8]) -> String {
    let body_text = String::from_utf8_lossy(body).trim().to_string();
    let parsed = serde_json::from_slice::<ElevenLabsErrorEnvelope>(body).ok();

    if let Some(detail) = parsed.and_then(|envelope| envelope.detail) {
        let error_type = detail
            .error_type
            .unwrap_or_else(|| "unknown_error".to_string());
        let code = detail.code.unwrap_or_else(|| "unknown_code".to_string());
        let message = detail.message.unwrap_or_else(|| body_text.clone());
        let param_suffix = detail
            .param
            .filter(|param| !param.trim().is_empty())
            .map(|param| format!(" param={}", param))
            .unwrap_or_default();
        let request_id_suffix = detail
            .request_id
            .filter(|request_id| !request_id.trim().is_empty())
            .map(|request_id| format!(" request_id={}", request_id))
            .unwrap_or_default();

        let prefix = match status {
            401 => "authentication_error",
            402 => "quota exceeded",
            429 => "rate limit exceeded",
            503 => "service unavailable",
            _ => "ElevenLabs",
        };

        return format!(
            "{} HTTP {} [{}:{}] {}{}{}",
            prefix, status, error_type, code, message, param_suffix, request_id_suffix
        );
    }

    format!("ElevenLabs HTTP {}: {}", status, body_text)
}

fn multipart_filename(path: &Path) -> String {
    let raw_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("audio.ogg");

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

#[cfg(test)]
mod tests {
    use super::{
        cleaned_elevenlabs_keyterms, format_elevenlabs_error, multipart_filename,
        ELEVENLABS_MAX_KEYTERMS,
    };
    use std::path::Path;

    #[test]
    fn keyterms_are_trimmed_deduped_and_filtered() {
        let long = "a".repeat(60);
        let input = vec![
            " ElevenLabs ".to_string(),
            "ElevenLabs".to_string(),
            "voice x".to_string(),
            "too many words in this phrase here".to_string(),
            long,
            "".to_string(),
            "foo   bar".to_string(),
        ];

        let result = cleaned_elevenlabs_keyterms(&input, true);

        assert_eq!(
            result,
            vec![
                "ElevenLabs".to_string(),
                "voice x".to_string(),
                "foo bar".to_string()
            ]
        );
    }

    #[test]
    fn keyterms_respect_disable_and_limit() {
        let disabled = cleaned_elevenlabs_keyterms(&["ElevenLabs".to_string()], false);
        assert!(disabled.is_empty());

        let input = (0..(ELEVENLABS_MAX_KEYTERMS + 20))
            .map(|idx| format!("term-{}", idx))
            .collect::<Vec<_>>();
        let result = cleaned_elevenlabs_keyterms(&input, true);
        assert_eq!(result.len(), ELEVENLABS_MAX_KEYTERMS);
        assert_eq!(result.first().map(String::as_str), Some("term-0"));
        assert_eq!(
            result.last().map(String::as_str),
            Some(format!("term-{}", ELEVENLABS_MAX_KEYTERMS - 1).as_str())
        );
    }

    #[test]
    fn multipart_filename_converts_opus_to_ogg() {
        assert_eq!(
            multipart_filename(Path::new("/tmp/audio.opus")),
            "audio.ogg"
        );
        assert_eq!(multipart_filename(Path::new("/tmp/audio.ogg")), "audio.ogg");
    }

    #[test]
    fn elevenlabs_error_format_surfaces_classification_hints() {
        let body = br#"{
          "detail": {
            "type": "authentication_error",
            "code": "invalid_api_key",
            "message": "API key is invalid",
            "param": "xi-api-key",
            "request_id": "req_123"
          }
        }"#;

        let formatted = format_elevenlabs_error(401, body);

        assert!(formatted.contains("authentication_error HTTP 401"));
        assert!(formatted.contains("invalid_api_key"));
        assert!(formatted.contains("xi-api-key"));
        assert!(formatted.contains("req_123"));
    }
}
