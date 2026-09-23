use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};

use super::funasr_client::{build_instant_vocabulary, language_hints, qwen_workspace_host};
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

        let uses_qwen_audio_api = qwen_audio_flash_batch_model(&self.config.qwen_batch_model);
        let endpoint = if uses_qwen_audio_api {
            qwen_audio_multimodal_endpoint(
                &self.config.qwen_ws_url,
                &self.config.qwen_workspace_id,
            )?
        } else {
            format!(
                "{}/chat/completions",
                qwen_compatible_base_url(&self.config.qwen_ws_url)?
            )
        };
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

        let body = if uses_qwen_audio_api {
            build_qwen_audio_flash_body(&self.config, &filename, data_uri, &bias_text)
        } else {
            build_legacy_qwen_body(&self.config, data_uri, &bias_text)
        };

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

        let mut request = self
            .http
            .post(endpoint)
            .bearer_auth(&self.config.qwen_api_key)
            .json(&body);
        if uses_qwen_audio_api {
            request = request.header("X-DashScope-SSE", "disable");
        }
        let response = request
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
        let text = if uses_qwen_audio_api {
            payload
                .get("output")
                .and_then(|output| output.get("text"))
                .and_then(Value::as_str)
        } else {
            payload
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("message"))
                .and_then(|message| message.get("content"))
                .and_then(Value::as_str)
        }
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

/// The short-audio Qwen-Audio models on the DashScope multimodal endpoint.
/// Their streaming, file-transcription and message siblings share the prefix
/// but not the endpoint.
fn qwen_audio_flash_batch_model(model: &str) -> bool {
    let model = model.trim();
    ["qwen-audio-3.1-asr-flash", "qwen-audio-3.0-asr-flash"]
        .iter()
        .any(|id| model.starts_with(id))
        && !model.contains("streaming")
        && !model.contains("filetrans")
        && !model.contains("message")
}

fn build_legacy_qwen_body(config: &AsrConfig, data_uri: String, bias_text: &str) -> Value {
    let mut messages = Vec::new();
    if !bias_text.is_empty() {
        messages.push(json!({
            "role": "system",
            "content": [{
                "type": "text",
                "text": format!(
                    "Reference vocabulary for spelling only. Use it only when the audio clearly refers to these terms. Never output this list by itself.\n{}",
                    bias_text
                )
            }]
        }));
    }
    messages.push(audio_message(data_uri));

    let mut body = json!({
        "model": config.qwen_batch_model,
        "messages": messages,
        "stream": false,
        "asr_options": { "enable_itn": false }
    });
    if !config.qwen_language.trim().is_empty() {
        body["asr_options"]["language"] = json!(config.qwen_language.trim());
    }
    body
}

fn build_qwen_audio_flash_body(
    config: &AsrConfig,
    filename: &str,
    data_uri: String,
    bias_text: &str,
) -> Value {
    let mut messages = Vec::new();
    if !bias_text.is_empty() {
        let context: String = bias_text.chars().take(400).collect();
        messages.push(json!({
            "role": "user",
            "content": [{ "type": "input_text", "text": context }]
        }));
    }
    messages.push(audio_message(data_uri));

    // Recorded input is a self-describing container (normally WAV). The API accepts any
    // sample rate for these formats, so do not claim 16 kHz when the capture device used
    // another rate. Raw PCM is not produced by this file-based path.
    let mut parameters = json!({
        "format": audio_format_for_filename(filename)
    });
    let hints = language_hints(&config.qwen_language, true);
    if !hints.is_empty() {
        parameters["language_hints"] = json!(hints);
    }
    let vocabulary = build_instant_vocabulary(&config.hotwords, config.qwen_hotword_weight);
    if !vocabulary.is_empty() {
        parameters["vocabulary"] = Value::Object(vocabulary);
        if !config.qwen_vocabulary_id.trim().is_empty() {
            log::warn!(
                "Qwen-Audio batch ASR: inline dictionary hotwords override vocabulary_id={} for this request",
                config.qwen_vocabulary_id.trim()
            );
        }
    } else if !config.qwen_vocabulary_id.trim().is_empty() {
        parameters["vocabulary_id"] = json!(config.qwen_vocabulary_id.trim());
    }

    json!({
        "model": config.qwen_batch_model,
        "input": { "messages": messages },
        "parameters": parameters
    })
}

fn audio_message(data_uri: String) -> Value {
    json!({
        "role": "user",
        "content": [{
            "type": "input_audio",
            "input_audio": { "data": data_uri }
        }]
    })
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

fn qwen_audio_multimodal_endpoint(ws_url: &str, workspace_id: &str) -> Result<String, AsrError> {
    let trimmed = ws_url.trim();
    let host = qwen_workspace_host(ws_url, workspace_id)?;
    let scheme = if trimmed.starts_with("http://") || trimmed.starts_with("ws://") {
        "http"
    } else {
        "https"
    };
    Ok(format!(
        "{}://{}/api/v1/services/aigc/multimodal-generation/generation",
        scheme, host
    ))
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

fn audio_format_for_filename(filename: &str) -> &'static str {
    match filename
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "aac" => "aac",
        "amr" => "amr",
        "flac" => "flac",
        "m4a" => "m4a",
        "mp3" | "mpeg" | "mpga" => "mp3",
        "ogg" => "ogg",
        "opus" => "opus",
        "webm" => "webm",
        _ => "wav",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_qwen_audio_flash_body, qwen_audio_flash_batch_model, qwen_audio_multimodal_endpoint,
        qwen_compatible_base_url,
    };
    use crate::asr::AsrConfig;

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

    #[test]
    fn qwen_audio_endpoint_preserves_workspace_host() {
        assert_eq!(
            qwen_audio_multimodal_endpoint(
                "wss://workspace-id.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference",
                ""
            )
            .unwrap(),
            "https://workspace-id.cn-beijing.maas.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation"
        );
        assert_eq!(
            qwen_audio_multimodal_endpoint(
                "wss://dashscope-intl.aliyuncs.com/api-ws/v1/inference",
                "workspace-id"
            )
            .unwrap(),
            "https://workspace-id.ap-southeast-1.maas.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation"
        );
    }

    #[test]
    fn qwen_audio_batch_body_includes_context_hotwords_and_languages() {
        let mut config = AsrConfig::default();
        config.qwen_batch_model = "qwen-audio-3.0-asr-flash".to_string();
        config.qwen_language = "zh, en".to_string();
        config.hotwords = vec!["VoiceX".to_string(), "连续刚构桥".to_string()];
        config.qwen_hotword_weight = 5;
        let body = build_qwen_audio_flash_body(
            &config,
            "recording.opus",
            "data:audio/ogg;base64,AAAA".to_string(),
            "VoiceX, 连续刚构桥",
        );
        assert_eq!(body["parameters"]["format"], "opus");
        assert_eq!(
            body["parameters"]["language_hints"],
            serde_json::json!(["zh", "en"])
        );
        assert_eq!(body["parameters"]["vocabulary"]["VoiceX"], 5);
        assert_eq!(
            body["input"]["messages"][0]["content"][0]["type"],
            "input_text"
        );
        assert_eq!(
            body["input"]["messages"][1]["content"][0]["type"],
            "input_audio"
        );
        assert!(qwen_audio_flash_batch_model(&config.qwen_batch_model));
        assert!(qwen_audio_flash_batch_model("qwen-audio-3.1-asr-flash"));
        for other in [
            "qwen-audio-3.1-asr-flash-streaming",
            "qwen-audio-3.1-asr-flash-filetrans",
            "qwen-audio-3.1-asr-flash-message",
            "qwen3-asr-flash",
        ] {
            assert!(!qwen_audio_flash_batch_model(other), "{other}");
        }
    }
}
