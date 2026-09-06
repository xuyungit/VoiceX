use std::path::Path;
use std::time::Duration;

use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{AsrConfig, AsrError};

const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com";
const FILE_STATE_ACTIVE: &str = "ACTIVE";
const FILE_STATE_PROCESSING: &str = "PROCESSING";
const MAX_PROMPT_HOTWORDS: usize = 100;

pub(crate) fn transcription_languages(language: &str) -> Vec<&str> {
    match language {
        "zh" => vec!["cmn-Hans-CN"],
        "en" => vec!["en-US"],
        "zh-en" => vec!["cmn-Hans-CN", "en-US"],
        _ => vec![],
    }
}

pub(crate) fn transcription_vocabulary(config: &AsrConfig) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    config
        .hotwords
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter(|s| seen.insert(s.to_string()))
        .take(MAX_PROMPT_HOTWORDS)
        .map(str::to_string)
        .collect()
}

fn build_transcribe_request(config: &AsrConfig, uri: &str, mime: &str) -> Value {
    // https://ai.google.dev/gemini-api/docs/transcribe
    // Keep verbatim defaults: automatic rewriting belongs to the correction stage.
    json!({
        "model": config.gemini_model.trim(),
        "store": false,
        "input": [{"type": "audio", "uri": uri, "mime_type": mime}],
        "generation_config": {
            "transcription_config": {
                "language_codes": transcription_languages(&config.gemini_language),
                "custom_vocabulary": transcription_vocabulary(config)
            }
        }
    })
}

fn parse_transcribe_response(body: &[u8]) -> Result<String, AsrError> {
    let response: Value = serde_json::from_slice(body)
        .map_err(|e| AsrError::ProtocolError(format!("Invalid Gemini transcription JSON: {e}")))?;
    if let Some(status) = response.get("status").and_then(Value::as_str) {
        if status != "completed" {
            return Err(AsrError::ServerError(format!(
                "Gemini transcription did not complete: {status}"
            )));
        }
    }
    let steps = response
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AsrError::ProtocolError("Gemini transcription response is missing steps".into())
        })?;
    let mut text = String::new();
    for step in steps.iter().filter(|step| step["type"] == "model_output") {
        let content = step
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                AsrError::ProtocolError("Gemini model_output is missing content".into())
            })?;
        for part in content.iter().filter(|part| part["type"] == "text") {
            text.push_str(part.get("text").and_then(Value::as_str).ok_or_else(|| {
                AsrError::ProtocolError("Gemini text output is missing text".into())
            })?);
        }
    }
    if text.trim().is_empty() {
        return Err(AsrError::ServerError(
            "Gemini transcription returned no text".into(),
        ));
    }
    Ok(text.trim().to_string())
}

pub struct GeminiTranscriptionClient {
    config: AsrConfig,
    http: reqwest::Client,
}

#[derive(Debug, Clone, Deserialize)]
struct GeminiFile {
    name: String,
    uri: String,
    #[serde(rename = "mimeType")]
    mime_type: String,
    state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiUploadEnvelope {
    file: GeminiFile,
}

#[derive(Debug, Serialize)]
struct GeminiGenerateContentRequest {
    #[serde(rename = "system_instruction")]
    system_instruction: GeminiSystemInstruction,
    contents: Vec<GeminiContent>,
    #[serde(rename = "generationConfig")]
    generation_config: GeminiGenerationConfig,
}

#[derive(Debug, Serialize)]
struct GeminiSystemInstruction {
    parts: Vec<GeminiTextPart>,
}

#[derive(Debug, Serialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize)]
struct GeminiTextPart {
    text: String,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum GeminiPart {
    Text {
        text: String,
    },
    FileData {
        #[serde(rename = "file_data")]
        file_data: GeminiFileData,
    },
}

#[derive(Debug, Serialize)]
struct GeminiGenerationConfig {
    #[serde(rename = "thinkingConfig")]
    thinking_config: GeminiThinkingConfig,
}

#[derive(Debug, Serialize)]
struct GeminiThinkingConfig {
    #[serde(rename = "thinkingLevel")]
    thinking_level: &'static str,
}

#[derive(Debug, Serialize)]
struct GeminiFileData {
    #[serde(rename = "mime_type")]
    mime_type: String,
    #[serde(rename = "file_uri")]
    file_uri: String,
}

#[derive(Debug, Deserialize)]
struct GeminiGenerateContentResponse {
    candidates: Option<Vec<GeminiCandidate>>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    content: Option<GeminiResponseContent>,
}

#[derive(Debug, Deserialize)]
struct GeminiResponseContent {
    parts: Option<Vec<GeminiResponsePart>>,
}

#[derive(Debug, Deserialize)]
struct GeminiResponsePart {
    text: Option<String>,
}

struct PreparedUpload {
    display_name: String,
    mime_type: &'static str,
    bytes: Vec<u8>,
}

impl GeminiTranscriptionClient {
    pub fn new(config: AsrConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    pub async fn transcribe_file(&self, path: &Path) -> Result<String, AsrError> {
        if !self.config.is_valid() {
            return Err(AsrError::ConnectionFailed(
                "Invalid Gemini ASR configuration".to_string(),
            ));
        }

        let upload = prepare_upload(path)?;
        log::info!(
            "Starting Gemini transcription (model={}, language={}, file={}, bytes={}, mime={})",
            self.config.gemini_model,
            if self.config.gemini_language.trim().is_empty() {
                "auto"
            } else {
                self.config.gemini_language.as_str()
            },
            path.display(),
            upload.bytes.len(),
            upload.mime_type,
        );

        let mut file = self.upload_file(&upload).await?;
        let file_name = file.name.clone();

        let result = async {
            if matches!(file.state.as_deref(), Some(FILE_STATE_PROCESSING)) {
                file = self.wait_until_active(file).await?;
            }
            if self.config.gemini_model.trim() == "gemini-3.5-transcribe"
                || self
                    .config
                    .gemini_model
                    .trim()
                    .starts_with("gemini-3.5-transcribe-")
            {
                self.transcribe_interaction(&file).await
            } else {
                self.generate_transcript(&file).await
            }
        }
        .await;

        if let Err(err) = self.delete_file(&file_name).await {
            log::warn!("Failed to delete Gemini upload {}: {}", file_name, err);
        }

        result
    }

    async fn upload_file(&self, upload: &PreparedUpload) -> Result<GeminiFile, AsrError> {
        let start_response = self
            .http
            .post(format!("{}/upload/v1beta/files", GEMINI_BASE_URL))
            .query(&[("key", self.config.gemini_api_key.as_str())])
            .header("X-Goog-Upload-Protocol", "resumable")
            .header("X-Goog-Upload-Command", "start")
            .header(
                "X-Goog-Upload-Header-Content-Length",
                upload.bytes.len().to_string(),
            )
            .header("X-Goog-Upload-Header-Content-Type", upload.mime_type)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "file": {
                    "display_name": upload.display_name,
                }
            }))
            .send()
            .await
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;

        let status = start_response.status();
        let headers = start_response.headers().clone();
        let body = start_response.bytes().await.map_err(|e| {
            AsrError::ConnectionFailed(format!("Failed to read upload start response: {}", e))
        })?;
        if !status.is_success() {
            return Err(http_error("Gemini upload start", status.as_u16(), &body));
        }

        let upload_url = extract_upload_url(&headers)?;
        let finalize_response = self
            .http
            .post(upload_url)
            .header("Content-Length", upload.bytes.len().to_string())
            .header("X-Goog-Upload-Offset", "0")
            .header("X-Goog-Upload-Command", "upload, finalize")
            .body(upload.bytes.clone())
            .send()
            .await
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;

        let status = finalize_response.status();
        let body = finalize_response.bytes().await.map_err(|e| {
            AsrError::ConnectionFailed(format!("Failed to read upload finalize response: {}", e))
        })?;
        if !status.is_success() {
            return Err(http_error("Gemini upload finalize", status.as_u16(), &body));
        }

        parse_uploaded_file(&body)
    }

    async fn wait_until_active(&self, mut file: GeminiFile) -> Result<GeminiFile, AsrError> {
        for _ in 0..12 {
            if matches!(file.state.as_deref(), Some(FILE_STATE_ACTIVE) | None) {
                return Ok(file);
            }

            tokio::time::sleep(Duration::from_secs(2)).await;

            let response = self
                .http
                .get(format!("{}/v1beta/{}", GEMINI_BASE_URL, file.name))
                .query(&[("key", self.config.gemini_api_key.as_str())])
                .send()
                .await
                .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;
            let status = response.status();
            let body = response.bytes().await.map_err(|e| {
                AsrError::ConnectionFailed(format!("Failed to read file status response: {}", e))
            })?;
            if !status.is_success() {
                return Err(http_error("Gemini file status", status.as_u16(), &body));
            }

            file = parse_file_resource(&body)?;
        }

        Err(AsrError::ServerError(
            "Gemini file stayed in PROCESSING for too long".to_string(),
        ))
    }

    async fn transcribe_interaction(&self, file: &GeminiFile) -> Result<String, AsrError> {
        let response = self
            .http
            .post(format!("{GEMINI_BASE_URL}/v1beta/interactions"))
            .header("x-goog-api-key", &self.config.gemini_api_key)
            .header("Api-Revision", "2026-05-20")
            .timeout(Duration::from_secs(120))
            .json(&build_transcribe_request(
                &self.config,
                &file.uri,
                &file.mime_type,
            ))
            .send()
            .await
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;
        if !status.is_success() {
            return Err(http_error("Gemini transcription", status.as_u16(), &body));
        }
        parse_transcribe_response(&body)
    }

    async fn generate_transcript(&self, file: &GeminiFile) -> Result<String, AsrError> {
        let response = self
            .http
            .post(format!(
                "{}/v1beta/models/{}:generateContent",
                GEMINI_BASE_URL, self.config.gemini_model
            ))
            .query(&[("key", self.config.gemini_api_key.as_str())])
            .json(&GeminiGenerateContentRequest {
                system_instruction: GeminiSystemInstruction {
                    parts: vec![GeminiTextPart {
                        text: build_system_instruction(
                            &self.config.gemini_language,
                            &self.config.hotwords,
                        ),
                    }],
                },
                contents: vec![GeminiContent {
                    parts: vec![
                        GeminiPart::Text {
                            text: build_transcription_prompt(),
                        },
                        GeminiPart::FileData {
                            file_data: GeminiFileData {
                                mime_type: file.mime_type.clone(),
                                file_uri: file.uri.clone(),
                            },
                        },
                    ],
                }],
                generation_config: GeminiGenerationConfig {
                    thinking_config: GeminiThinkingConfig {
                        thinking_level: "MINIMAL",
                    },
                },
            })
            .send()
            .await
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;

        let status = response.status();
        let body = response.bytes().await.map_err(|e| {
            AsrError::ConnectionFailed(format!("Failed to read generateContent response: {}", e))
        })?;
        if !status.is_success() {
            return Err(http_error("Gemini generateContent", status.as_u16(), &body));
        }

        let parsed: GeminiGenerateContentResponse = serde_json::from_slice(&body)
            .map_err(|e| AsrError::ProtocolError(format!("Invalid Gemini response JSON: {}", e)))?;
        extract_text(parsed)
    }

    async fn delete_file(&self, name: &str) -> Result<(), AsrError> {
        let response = self
            .http
            .delete(format!("{}/v1beta/{}", GEMINI_BASE_URL, name))
            .query(&[("key", self.config.gemini_api_key.as_str())])
            .send()
            .await
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;

        if response.status().is_success() || response.status().as_u16() == 404 {
            return Ok(());
        }

        let status = response.status();
        let body = response.bytes().await.map_err(|e| {
            AsrError::ConnectionFailed(format!("Failed to read delete response: {}", e))
        })?;
        Err(http_error("Gemini delete file", status.as_u16(), &body))
    }
}

fn prepare_upload(path: &Path) -> Result<PreparedUpload, AsrError> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("audio.wav")
        .to_string();

    if matches!(extension(path).as_deref(), Some("opus" | "ogg")) {
        let pcm = crate::asr::ogg_decoder::decode_ogg_opus_to_pcm16k(path)
            .map_err(AsrError::ProtocolError)?;
        return Ok(PreparedUpload {
            display_name: replace_extension(&filename, "wav"),
            mime_type: "audio/wav",
            bytes: pcm16_to_wav_bytes(16_000, 1, &pcm),
        });
    }

    let bytes = std::fs::read(path)
        .map_err(|e| AsrError::ConnectionFailed(format!("Failed to read audio file: {}", e)))?;
    Ok(PreparedUpload {
        display_name: filename,
        mime_type: mime_type_for_path(path),
        bytes,
    })
}

fn extract_upload_url(headers: &HeaderMap) -> Result<String, AsrError> {
    headers
        .get("x-goog-upload-url")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.to_string())
        .ok_or_else(|| AsrError::ProtocolError("Missing x-goog-upload-url header".to_string()))
}

fn parse_uploaded_file(body: &[u8]) -> Result<GeminiFile, AsrError> {
    serde_json::from_slice::<GeminiUploadEnvelope>(body)
        .map(|env| env.file)
        .or_else(|_| serde_json::from_slice::<GeminiFile>(body))
        .map_err(|e| AsrError::ProtocolError(format!("Invalid Gemini upload response JSON: {}", e)))
}

fn parse_file_resource(body: &[u8]) -> Result<GeminiFile, AsrError> {
    serde_json::from_slice::<GeminiFile>(body)
        .or_else(|_| serde_json::from_slice::<GeminiUploadEnvelope>(body).map(|env| env.file))
        .map_err(|e| AsrError::ProtocolError(format!("Invalid Gemini file resource JSON: {}", e)))
}

fn extract_text(response: GeminiGenerateContentResponse) -> Result<String, AsrError> {
    let text = response
        .candidates
        .unwrap_or_default()
        .into_iter()
        .filter_map(|candidate| candidate.content)
        .flat_map(|content| content.parts.unwrap_or_default().into_iter())
        .filter_map(|part| part.text)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if text.is_empty() {
        Err(AsrError::ServerError(
            "Gemini returned an empty transcript".to_string(),
        ))
    } else {
        Ok(text)
    }
}

fn build_transcription_prompt() -> String {
    "Generate a transcript of the speech. Return only the transcript text.".to_string()
}

fn build_system_instruction(language: &str, hotwords: &[String]) -> String {
    let mut lines = vec![
        "You are an automatic speech recognition engine.".to_string(),
        "Transcribe the attached audio into plain text.".to_string(),
        "Return only the transcript text with no summary, explanation, translation, or markdown."
            .to_string(),
        "Do not add content that was not spoken.".to_string(),
    ];

    match normalize_language_hint(language) {
        GeminiLanguageHint::Auto => {
            lines.push(
                "Detect the spoken language automatically. The audio may contain code-switching or mixed languages."
                    .to_string(),
            );
        }
        GeminiLanguageHint::Chinese => {
            lines.push("The audio is primarily in Simplified Chinese.".to_string());
        }
        GeminiLanguageHint::English => {
            lines.push("The audio is primarily in English.".to_string());
        }
        GeminiLanguageHint::ChineseEnglish => {
            lines.push(
                "The audio may mix Simplified Chinese and English. Preserve English words, acronyms, and proper nouns as spoken."
                    .to_string(),
            );
        }
    }

    let prompt_hotwords = hotwords
        .iter()
        .map(|word| word.trim())
        .filter(|word| !word.is_empty())
        .take(MAX_PROMPT_HOTWORDS)
        .collect::<Vec<_>>();

    if !prompt_hotwords.is_empty() {
        lines.push(
            "If the audio plausibly matches one of these domain hotwords, prefer the exact spelling from this list."
                .to_string(),
        );
        lines.push(format!("Hotwords: {}", prompt_hotwords.join(", ")));
    }

    lines.join("\n")
}

enum GeminiLanguageHint {
    Auto,
    Chinese,
    English,
    ChineseEnglish,
}

fn normalize_language_hint(language: &str) -> GeminiLanguageHint {
    match language.trim() {
        "zh" => GeminiLanguageHint::Chinese,
        "en" => GeminiLanguageHint::English,
        "zh-en" | "en-zh" | "zh+en" | "en+zh" => GeminiLanguageHint::ChineseEnglish,
        _ => GeminiLanguageHint::Auto,
    }
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

fn replace_extension(filename: &str, ext: &str) -> String {
    match filename.rsplit_once('.') {
        Some((base, _)) if !base.is_empty() => format!("{}.{}", base, ext),
        _ => format!("{}.{}", filename, ext),
    }
}

fn mime_type_for_path(path: &Path) -> &'static str {
    match extension(path).as_deref() {
        Some("wav") => "audio/wav",
        Some("mp3" | "mpeg" | "mpga") => "audio/mpeg",
        Some("flac") => "audio/flac",
        Some("ogg" | "opus") => "audio/ogg",
        _ => "application/octet-stream",
    }
}

fn pcm16_to_wav_bytes(sample_rate: u32, channels: u16, pcm: &[u8]) -> Vec<u8> {
    let bits_per_sample = 16u16;
    let byte_rate = sample_rate * channels as u32 * (bits_per_sample as u32 / 8);
    let block_align = channels * (bits_per_sample / 8);
    let data_len = pcm.len() as u32;
    let riff_len = 36 + data_len;

    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_len.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

fn http_error(context: &str, status: u16, body: &[u8]) -> AsrError {
    AsrError::ServerError(format!(
        "{} HTTP {}: {}",
        context,
        status,
        String::from_utf8_lossy(body)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transcribe_reads_only_model_text_and_rejects_incomplete_responses() {
        let body = json!({"status":"completed", "steps":[
            {"type":"user_input", "content":[{"type":"text","text":"ignore me"}]},
            {"type":"model_output", "content":[{"type":"text","text":"你好，VoiceX。"}]}
        ]});
        assert_eq!(
            parse_transcribe_response(&serde_json::to_vec(&body).unwrap()).unwrap(),
            "你好，VoiceX。"
        );
        for invalid in [
            json!({"status":"in_progress","steps":[]}),
            json!({"outputs":[]}),
            json!({"steps":[{"type":"model_output","content":[]}]}),
            json!({"steps":[{"type":"model_output"}]}),
        ] {
            assert!(parse_transcribe_response(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
    }
    #[test]
    fn transcribe_request_uses_audio_and_bounded_deduplicated_vocabulary() {
        let mut config = AsrConfig::default();
        config.gemini_model = "gemini-3.5-transcribe".into();
        config.gemini_language = "zh-en".into();
        config.hotwords = vec![" VoiceX ".into(), "VoiceX".into(), "".into()];
        let request = build_transcribe_request(&config, "files/test", "audio/ogg");
        assert_eq!(request["store"], false);
        assert_eq!(request["input"][0]["uri"], "files/test");
        assert_eq!(
            request["generation_config"]["transcription_config"]["language_codes"],
            json!(["cmn-Hans-CN", "en-US"])
        );
        assert_eq!(
            request["generation_config"]["transcription_config"]["custom_vocabulary"],
            json!(["VoiceX"])
        );
        config.hotwords = (0..150).map(|i| format!("word{i}")).collect();
        assert_eq!(transcription_vocabulary(&config).len(), 100);
    }
}
