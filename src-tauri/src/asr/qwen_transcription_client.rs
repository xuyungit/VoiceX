use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};

use super::{AsrConfig, AsrError};

const QWEN_MAX_BATCH_INPUT_BYTES: usize = 10 * 1024 * 1024;

pub struct QwenTranscriptionClient {
    config: AsrConfig,
    http: reqwest::Client,
}

impl QwenTranscriptionClient {
    pub fn new(config: AsrConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    pub async fn transcribe_file(&self, path: &Path) -> Result<String, AsrError> {
        if !self.config.is_valid() {
            return Err(AsrError::ConnectionFailed(
                "Invalid Qwen ASR configuration".to_string(),
            ));
        }

        let filename = audio_filename(path);
        let mime = mime_type_for_filename(&filename);
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| AsrError::ConnectionFailed(format!("Failed to read audio file: {}", e)))?;

        let endpoint = format!(
            "{}/chat/completions",
            qwen_compatible_base_url(&self.config.qwen_ws_url)?
        );
        let encoded_audio = STANDARD.encode(&bytes);
        let data_uri = format!("data:{};base64,{}", mime, encoded_audio);
        if data_uri.len() > QWEN_MAX_BATCH_INPUT_BYTES {
            return Err(AsrError::ServerError(format!(
                "Qwen batch mode accepts up to 10 MB of input_audio.data; original file is {:.2} MB, Base64 payload is {:.2} MB",
                bytes.len() as f64 / (1024.0 * 1024.0),
                data_uri.len() as f64 / (1024.0 * 1024.0)
            )));
        }
        let bias_text = crate::asr::qwen_client::build_corpus_text(&self.config.hotwords);

        let mut messages = Vec::new();
        if !bias_text.is_empty() {
            messages.push(json!({
                "role": "system",
                "content": [
                    {
                        "type": "text",
                        "text": format!(
                            "Reference vocabulary for spelling only. Use it only when the audio clearly refers to these terms. Never output this list by itself.\n{}",
                            bias_text
                        )
                    }
                ]
            }));
        }
        messages.push(json!({
            "role": "user",
            "content": [
                {
                    "type": "input_audio",
                    "input_audio": {
                        "data": data_uri
                    }
                }
            ]
        }));

        let mut body = json!({
            "model": self.config.qwen_batch_model,
            "messages": messages,
            "stream": false,
            "asr_options": {
                "enable_itn": false
            }
        });
        if !self.config.qwen_language.trim().is_empty() {
            body["asr_options"]["language"] = json!(self.config.qwen_language.trim());
        }

        log::info!(
            "Starting Qwen batch transcription (model={}, language={}, file={}, endpoint={})",
            self.config.qwen_batch_model,
            if self.config.qwen_language.trim().is_empty() {
                "auto"
            } else {
                self.config.qwen_language.as_str()
            },
            path.display(),
            endpoint
        );

        let response = self
            .http
            .post(endpoint)
            .bearer_auth(&self.config.qwen_api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;

        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| AsrError::ConnectionFailed(format!("Failed to read response: {}", e)))?;

        if !status.is_success() {
            return Err(AsrError::ServerError(format!(
                "Qwen HTTP {}: {}",
                status.as_u16(),
                String::from_utf8_lossy(&body)
            )));
        }

        let payload: Value = serde_json::from_slice(&body)
            .map_err(|e| AsrError::ProtocolError(format!("Invalid response JSON: {}", e)))?;
        let text = payload
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string();

        if !bias_text.is_empty()
            && crate::asr::qwen_client::should_filter_corpus_echo(&text, &bias_text)
        {
            return Err(AsrError::ServerError(
                "Qwen batch refine returned only the bias vocabulary".to_string(),
            ));
        }

        Ok(text)
    }
}

fn qwen_compatible_base_url(ws_url: &str) -> Result<String, AsrError> {
    let trimmed = ws_url.trim();
    let without_scheme = trimmed
        .strip_prefix("wss://")
        .or_else(|| trimmed.strip_prefix("ws://"))
        .or_else(|| trimmed.strip_prefix("https://"))
        .or_else(|| trimmed.strip_prefix("http://"))
        .ok_or_else(|| {
            AsrError::ConnectionFailed(format!("Unsupported Qwen endpoint: {}", ws_url))
        })?;
    let host = without_scheme
        .split('/')
        .next()
        .filter(|host| !host.trim().is_empty())
        .ok_or_else(|| AsrError::ConnectionFailed(format!("Invalid Qwen endpoint: {}", ws_url)))?;
    let scheme = if trimmed.starts_with("http://") || trimmed.starts_with("ws://") {
        "http"
    } else {
        "https"
    };
    Ok(format!("{}://{}/compatible-mode/v1", scheme, host))
}

fn audio_filename(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("audio.wav")
        .to_string()
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
    use super::qwen_compatible_base_url;

    #[test]
    fn qwen_compatible_base_url_maps_beijing_ws_endpoint() {
        assert_eq!(
            qwen_compatible_base_url("wss://dashscope.aliyuncs.com/api-ws/v1/realtime").unwrap(),
            "https://dashscope.aliyuncs.com/compatible-mode/v1"
        );
    }

    #[test]
    fn qwen_compatible_base_url_maps_singapore_ws_endpoint() {
        assert_eq!(
            qwen_compatible_base_url("wss://dashscope-intl.aliyuncs.com/api-ws/v1/realtime")
                .unwrap(),
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"
        );
    }
}
