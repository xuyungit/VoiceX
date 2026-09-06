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

        super::models::validate_model("openai", &self.config.openai_asr_model, "batch")
            .map_err(AsrError::ProtocolError)?;
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

        let model = self.config.openai_asr_model.trim();
        let languages = parse_language_list(&self.config.openai_asr_language);

        if model_supports_keywords(model) {
            // Modern models take structured `keywords` / `languages`; keep `prompt`
            // for scene description only so the dictionary can't dilute it.
            for language in &languages {
                form = form.text("languages", language.clone());
            }
            for keyword in cleaned_openai_keywords(&self.config.hotwords) {
                form = form.text("keywords", keyword);
            }
            let prompt = self.config.openai_asr_prompt.trim();
            if !prompt.is_empty() {
                form = form.text("prompt", prompt.to_string());
            }
        } else {
            // Legacy models (gpt-4o-transcribe, whisper-1) only understand a single
            // `language` and have no keyword channel, so fall back to prompt stuffing.
            if let Some(language) = languages.first() {
                form = form.text("language", language.clone());
            }
            let prompt = build_transcription_prompt(
                self.config.openai_asr_prompt.trim(),
                &self.config.hotwords,
            );
            if !prompt.is_empty() {
                form = form.text("prompt", prompt);
            }
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

/// Upper bound on how many dictionary entries we forward as `keywords`.
///
/// OpenAI documents no hard limit, so this is our own guard against sending an
/// unbounded word list on every utterance.
const OPENAI_MAX_KEYWORDS: usize = 100;

/// Models that accept the structured `keywords` / `languages` parameters.
///
/// The legacy `gpt-4o-transcribe*` / `whisper-1` family silently ignores them,
/// which is why callers must branch instead of always sending the new fields.
pub(crate) fn model_supports_keywords(model: &str) -> bool {
    let model = model.trim();
    model == "gpt-transcribe"
        || model == "gpt-live-transcribe"
        || model.starts_with("gpt-transcribe-")
        || model.starts_with("gpt-live-transcribe-")
}

/// Split the user's language setting into an ISO 639-1 list.
///
/// Accepts the comma-separated form already used elsewhere in settings (e.g.
/// `"zh, en"`), so no settings migration is needed.
pub(crate) fn parse_language_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Normalize dictionary entries into `keywords` values.
///
/// OpenAI requires each keyword on one line and rejects `<`, `>`, CR and LF.
pub(crate) fn cleaned_openai_keywords(hotwords: &[String]) -> Vec<String> {
    let mut keywords: Vec<String> = Vec::new();
    let mut skipped = 0usize;

    for hotword in hotwords {
        let normalized: String = hotword
            .trim()
            .chars()
            .filter(|c| !matches!(c, '<' | '>' | '\r' | '\n'))
            .collect();
        let normalized = normalized.trim().to_string();

        if normalized.is_empty() {
            continue;
        }
        if keywords.contains(&normalized) {
            continue;
        }
        if keywords.len() >= OPENAI_MAX_KEYWORDS {
            skipped += 1;
            continue;
        }
        keywords.push(normalized);
    }

    if skipped > 0 {
        log::warn!(
            "OpenAI ASR: skipped {} dictionary entries beyond the {} keyword limit",
            skipped,
            OPENAI_MAX_KEYWORDS
        );
    }

    keywords
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

#[cfg(test)]
mod tests {
    use super::{cleaned_openai_keywords, model_supports_keywords, parse_language_list};

    #[test]
    fn keyword_support_tracks_model_family() {
        assert!(model_supports_keywords("gpt-transcribe"));
        assert!(model_supports_keywords("gpt-live-transcribe"));
        assert!(model_supports_keywords("gpt-transcribe-2026-07-28"));
        assert!(!model_supports_keywords("gpt-4o-transcribe"));
        assert!(!model_supports_keywords("gpt-4o-mini-transcribe"));
        assert!(!model_supports_keywords("whisper-1"));
    }

    #[test]
    fn languages_split_on_commas() {
        assert_eq!(parse_language_list("zh, en"), vec!["zh", "en"]);
        assert_eq!(parse_language_list(" zh "), vec!["zh"]);
        assert!(parse_language_list("").is_empty());
        assert!(parse_language_list("  ,  ").is_empty());
    }

    #[test]
    fn keywords_are_trimmed_deduped_and_stripped() {
        let input = vec![
            "  Tauri  ".to_string(),
            "Tauri".to_string(),
            "".to_string(),
            "Vo<ice>X".to_string(),
            "multi\nline".to_string(),
        ];
        assert_eq!(
            cleaned_openai_keywords(&input),
            vec!["Tauri", "VoiceX", "multiline"]
        );
    }

    #[test]
    fn keywords_respect_the_cap() {
        let input: Vec<String> = (0..150).map(|i| format!("word{i}")).collect();
        assert_eq!(
            cleaned_openai_keywords(&input).len(),
            super::OPENAI_MAX_KEYWORDS
        );
    }
}
