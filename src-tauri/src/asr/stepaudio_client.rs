use std::path::Path;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use futures_util::StreamExt;
use serde::Serialize;

use super::{AsrConfig, AsrError};

pub struct StepAudioTranscriptionClient {
    config: AsrConfig,
    http: reqwest::Client,
}

#[derive(Debug, Serialize)]
struct StepAudioAsrRequest {
    audio: StepAudioAudio,
}

#[derive(Debug, Serialize)]
struct StepAudioAudio {
    data: String,
    input: StepAudioInput,
}

#[derive(Debug, Serialize)]
struct StepAudioInput {
    transcription: StepAudioTranscription,
    format: StepAudioFormat,
}

#[derive(Debug, Serialize)]
struct StepAudioTranscription {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    hotwords: Vec<String>,
    enable_itn: bool,
}

#[derive(Debug, Serialize)]
struct StepAudioFormat {
    #[serde(rename = "type")]
    format_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    codec: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bits: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<u16>,
}

impl StepAudioTranscriptionClient {
    pub fn new(config: AsrConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    pub async fn transcribe_file(&self, path: &Path) -> Result<String, AsrError> {
        if !self.config.is_valid() {
            return Err(AsrError::ConnectionFailed(
                "Invalid StepAudio ASR configuration".to_string(),
            ));
        }

        let format = stepaudio_format_for_path(path)?;
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| AsrError::ConnectionFailed(format!("Failed to read audio file: {}", e)))?;
        let data = STANDARD.encode(&bytes);
        let endpoint = format!(
            "{}/audio/asr/sse",
            self.config.stepaudio_base_url.trim_end_matches('/')
        );
        let language = normalize_language_hint(&self.config.stepaudio_language);
        let hotwords = normalize_hotwords(&self.config.hotwords);

        log::info!(
            "Starting StepAudio transcription (model={}, language={}, file={}, bytes={}, format={}, hotwords={})",
            self.config.stepaudio_model,
            language.as_deref().unwrap_or("auto"),
            path.display(),
            bytes.len(),
            format.format_type,
            hotwords.len()
        );

        let request = StepAudioAsrRequest {
            audio: StepAudioAudio {
                data,
                input: StepAudioInput {
                    transcription: StepAudioTranscription {
                        model: self.config.stepaudio_model.clone(),
                        language,
                        hotwords,
                        enable_itn: self.config.enable_itn,
                    },
                    format,
                },
            },
        };

        let response = self
            .http
            .post(endpoint)
            .bearer_auth(&self.config.stepaudio_api_key)
            .header("Accept", "text/event-stream")
            .json(&request)
            .send()
            .await
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.bytes().await.map_err(|e| {
                AsrError::ConnectionFailed(format!("Failed to read response: {}", e))
            })?;
            return Err(AsrError::ServerError(format!(
                "StepAudio HTTP {}: {}",
                status.as_u16(),
                String::from_utf8_lossy(&body)
            )));
        }

        parse_sse_response(response).await
    }
}

async fn parse_sse_response(response: reqwest::Response) -> Result<String, AsrError> {
    let mut stream = response.bytes_stream();
    let mut pending = String::new();
    let mut delta_text = String::new();
    let mut done_text: Option<String> = None;

    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| AsrError::ConnectionFailed(format!("SSE read failed: {}", e)))?;
        pending.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(newline_index) = pending.find('\n') {
            let raw_line: String = pending.drain(..=newline_index).collect();
            let line = raw_line.trim();
            if let Some(data) = line.strip_prefix("data:") {
                handle_sse_data(data.trim(), &mut delta_text, &mut done_text)?;
            }
        }
    }

    if !pending.trim().is_empty() {
        let line = pending.trim();
        if let Some(data) = line.strip_prefix("data:") {
            handle_sse_data(data.trim(), &mut delta_text, &mut done_text)?;
        }
    }

    let text = done_text.unwrap_or(delta_text).trim().to_string();
    if text.is_empty() {
        Err(AsrError::ProtocolError(
            "StepAudio returned an empty transcription".to_string(),
        ))
    } else {
        Ok(text)
    }
}

fn handle_sse_data(
    data: &str,
    delta_text: &mut String,
    done_text: &mut Option<String>,
) -> Result<(), AsrError> {
    if data.is_empty() || data == "[DONE]" {
        return Ok(());
    }

    let value: serde_json::Value = serde_json::from_str(data)
        .map_err(|e| AsrError::ProtocolError(format!("Invalid StepAudio SSE JSON: {}", e)))?;

    match value.get("type").and_then(|v| v.as_str()) {
        Some("transcript.text.delta") => {
            if let Some(delta) = value.get("delta").and_then(|v| v.as_str()) {
                delta_text.push_str(delta);
            }
            Ok(())
        }
        Some("transcript.text.done") => {
            *done_text = value
                .get("text")
                .and_then(|v| v.as_str())
                .map(|text| text.to_string());
            Ok(())
        }
        Some("error") => Err(AsrError::ServerError(format!(
            "StepAudio error: {}",
            value
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error")
        ))),
        Some(_) | None => Ok(()),
    }
}

fn stepaudio_format_for_path(path: &Path) -> Result<StepAudioFormat, AsrError> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match extension.as_str() {
        "ogg" | "opus" => Ok(StepAudioFormat {
            format_type: "ogg",
            codec: None,
            rate: None,
            bits: None,
            channel: None,
        }),
        "mp3" | "mpeg" | "mpga" => Ok(StepAudioFormat {
            format_type: "mp3",
            codec: None,
            rate: None,
            bits: None,
            channel: None,
        }),
        "wav" => Ok(StepAudioFormat {
            format_type: "wav",
            codec: None,
            rate: None,
            bits: None,
            channel: None,
        }),
        "pcm" | "s16le" => Ok(StepAudioFormat {
            format_type: "pcm",
            codec: Some("pcm_s16le"),
            rate: Some(16_000),
            bits: Some(16),
            channel: Some(1),
        }),
        _ => Err(AsrError::ProtocolError(format!(
            "StepAudio ASR only supports ogg, mp3, wav, and pcm audio files; got {}",
            path.display()
        ))),
    }
}

fn normalize_language_hint(language: &str) -> Option<String> {
    let language = language.trim();
    if language.is_empty() {
        None
    } else {
        Some(language.to_string())
    }
}

fn normalize_hotwords(hotwords: &[String]) -> Vec<String> {
    hotwords
        .iter()
        .map(|word| word.trim())
        .filter(|word| !word.is_empty())
        .take(200)
        .map(|word| word.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{handle_sse_data, normalize_language_hint, stepaudio_format_for_path};

    #[test]
    fn parses_delta_and_done_events() {
        let mut delta = String::new();
        let mut done = None;

        handle_sse_data(
            r#"{"type":"transcript.text.delta","delta":"你好"}"#,
            &mut delta,
            &mut done,
        )
        .unwrap();
        handle_sse_data(
            r#"{"type":"transcript.text.done","text":"你好，世界。"}"#,
            &mut delta,
            &mut done,
        )
        .unwrap();

        assert_eq!(delta, "你好");
        assert_eq!(done.as_deref(), Some("你好，世界。"));
    }

    #[test]
    fn omits_blank_language_hint() {
        assert_eq!(normalize_language_hint("  "), None);
        assert_eq!(normalize_language_hint("auto").as_deref(), Some("auto"));
    }

    #[test]
    fn maps_voice_recording_ogg_to_ogg_format() {
        let format = stepaudio_format_for_path(Path::new("recording.ogg")).unwrap();
        assert_eq!(format.format_type, "ogg");
    }

    use std::path::Path;
}
