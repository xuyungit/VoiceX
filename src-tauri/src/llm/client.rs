//! LLM client for text correction

use super::config::{LLMApiMode, LLMConfig};
use super::provider::{create_provider, LLMProvider, Message};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use std::env;

const DICTIONARY_PLACEHOLDER: &str = "{{DICTIONARY}}";
const INPUT_HISTORY_PLACEHOLDER: &str = "{{INPUT_HISTORY}}";

#[derive(Debug, Clone, Copy)]
pub struct PromptBuildOptions {
    pub append_dictionary_if_missing: bool,
    pub append_history_if_missing: bool,
}

impl Default for PromptBuildOptions {
    fn default() -> Self {
        Self {
            append_dictionary_if_missing: true,
            append_history_if_missing: true,
        }
    }
}

/// LLM client for text correction
pub struct LLMClient {
    config: LLMConfig,
    http: Client,
    provider: Box<dyn LLMProvider>,
}

impl LLMClient {
    pub fn new(config: LLMConfig) -> Self {
        let provider = create_provider(&config.provider_type);
        Self {
            config,
            http: Client::new(),
            provider,
        }
    }

    /// Correct text using LLM
    pub async fn correct(
        &self,
        text: &str,
        prompt_template: &str,
        dictionary_text: &str,
        history: Option<&[String]>,
        options: PromptBuildOptions,
    ) -> Result<String, LLMError> {
        if !self.config.is_valid() {
            return Err(LLMError::InvalidConfig("Missing API key".to_string()));
        }

        let system_prompt = build_system_prompt(prompt_template, dictionary_text, history, options);
        let user_message = format!("原文：\n{}", text);

        match self.config.api_mode {
            LLMApiMode::ChatCompletions => {
                self.correct_with_chat_completions(&system_prompt, &user_message)
                    .await
            }
            LLMApiMode::Responses => {
                self.correct_with_responses(&system_prompt, &user_message)
                    .await
            }
        }
    }
}

impl LLMClient {
    async fn correct_with_chat_completions(
        &self,
        system_prompt: &str,
        user_message: &str,
    ) -> Result<String, LLMError> {
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );

        let messages = vec![
            Message {
                role: "system".to_string(),
                content: system_prompt.to_string(),
            },
            Message {
                role: "user".to_string(),
                content: user_message.to_string(),
            },
        ];

        let payload = self.provider.build_chat_request(messages, &self.config);
        let bytes = self.send_json_request(&url, &payload).await?;
        let parsed: ChatResponse =
            serde_json::from_slice(&bytes).map_err(|e| LLMError::InvalidResponse(e.to_string()))?;
        extract_chat_response_text(&parsed).ok_or(LLMError::EmptyResponse)
    }

    async fn correct_with_responses(
        &self,
        system_prompt: &str,
        user_message: &str,
    ) -> Result<String, LLMError> {
        let url = format!("{}/responses", self.config.base_url.trim_end_matches('/'));
        let payload =
            self.provider
                .build_responses_request(system_prompt, user_message, &self.config);
        let bytes = self.send_json_request(&url, &payload).await?;
        let body_text = String::from_utf8_lossy(&bytes).to_string();

        if body_text.trim_start().starts_with("event:") || body_text.contains("\ndata: ") {
            return parse_responses_sse(&body_text).ok_or(LLMError::EmptyResponse);
        }

        let parsed: ResponsesResponse =
            serde_json::from_slice(&bytes).map_err(|e| LLMError::InvalidResponse(e.to_string()))?;
        extract_responses_output(parsed.output.as_deref()).ok_or(LLMError::EmptyResponse)
    }

    async fn send_json_request(&self, url: &str, payload: &Value) -> Result<Vec<u8>, LLMError> {
        let request_body =
            serde_json::to_vec(payload).map_err(|e| LLMError::InvalidRequest(e.to_string()))?;
        let body_preview = String::from_utf8_lossy(&request_body);
        let llm_diag = llm_diag_enabled();

        log::info!(
            "LLM request provider={} model={} api_mode={:?} endpoint={}",
            self.provider.name(),
            self.config.model_name,
            self.config.api_mode,
            url
        );
        if llm_diag {
            log::info!(
                "LLM diag request auth={}",
                mask_api_key(&self.config.api_key)
            );
            log::info!("LLM diag request body: {}", body_preview);
        } else {
            log::debug!("LLM request auth={}", mask_api_key(&self.config.api_key));
            log::debug!("LLM request body: {}", body_preview);
        }

        let response = self
            .http
            .post(url)
            .header("Content-Type", "application/json")
            .bearer_auth(&self.config.api_key)
            .body(request_body)
            .send()
            .await
            .map_err(|e| LLMError::HttpError(e.to_string()))?;

        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| LLMError::HttpError(e.to_string()))?;
        let body_text = String::from_utf8_lossy(&bytes).to_string();
        log::info!("LLM response status={}", status);
        if llm_diag {
            log::info!("LLM diag response body: {}", body_text);
        } else {
            log::debug!("LLM response body: {}", body_text);
        }

        if !status.is_success() {
            return Err(LLMError::HttpStatus {
                status: status.as_u16(),
                body: body_text,
            });
        }

        Ok(bytes.to_vec())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LLMError {
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Failed to build request: {0}")]
    InvalidRequest(String),

    #[error("HTTP error: {0}")]
    HttpError(String),

    #[error("HTTP status {status}: {body}")]
    HttpStatus { status: u16, body: String },

    #[error("Invalid response: {0}")]
    InvalidResponse(String),

    #[error("Empty response from LLM")]
    EmptyResponse,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponsesResponse {
    output: Option<Vec<ResponsesOutputItem>>,
}

#[derive(Debug, Deserialize)]
struct ResponsesOutputItem {
    #[serde(rename = "type")]
    item_type: String,
    content: Option<Vec<ResponsesContentPart>>,
}

#[derive(Debug, Deserialize)]
struct ResponsesContentPart {
    #[serde(rename = "type")]
    content_type: Option<String>,
    text: Option<String>,
}

fn extract_chat_response_text(parsed: &ChatResponse) -> Option<String> {
    let content = parsed
        .choices
        .first()
        .and_then(|c| c.message.content.as_ref())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    log_response_text(&content);
    content
}

fn extract_responses_output(output: Option<&[ResponsesOutputItem]>) -> Option<String> {
    let text = output
        .unwrap_or_default()
        .iter()
        .filter(|item| item.item_type == "message")
        .filter_map(|item| item.content.as_ref())
        .flat_map(|content| content.iter())
        .filter(|part| part.content_type.as_deref() == Some("output_text"))
        .filter_map(|part| part.text.as_deref())
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string();

    let result = (!text.is_empty()).then_some(text);
    log_response_text(&result);
    result
}

fn parse_responses_sse(body: &str) -> Option<String> {
    let mut delta_output = String::new();
    let mut fallback_output: Option<String> = None;
    let mut event_data_lines: Vec<&str> = Vec::new();

    let flush_event = |event_data_lines: &mut Vec<&str>,
                       delta_output: &mut String,
                       fallback_output: &mut Option<String>| {
        if event_data_lines.is_empty() {
            return;
        }

        let payload = event_data_lines.join("\n");
        event_data_lines.clear();

        if payload.trim().is_empty() || payload.trim() == "[DONE]" {
            return;
        }

        let Ok(event_json) = serde_json::from_str::<Value>(&payload) else {
            return;
        };

        if let Some(delta) = event_json.get("delta").and_then(|value| value.as_str()) {
            delta_output.push_str(delta);
            return;
        }

        if fallback_output.is_none() {
            *fallback_output = extract_responses_output_from_value(&event_json);
        }
    };

    for line in body.lines() {
        if line.is_empty() {
            flush_event(
                &mut event_data_lines,
                &mut delta_output,
                &mut fallback_output,
            );
            continue;
        }

        if let Some(rest) = line.strip_prefix("data: ") {
            event_data_lines.push(rest);
        }
    }

    flush_event(
        &mut event_data_lines,
        &mut delta_output,
        &mut fallback_output,
    );

    let result = if delta_output.trim().is_empty() {
        fallback_output.and_then(|text| {
            let trimmed = text.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        })
    } else {
        Some(delta_output.trim().to_string())
    };
    log_response_text(&result);
    result
}

fn extract_responses_output_from_value(value: &Value) -> Option<String> {
    if let Ok(parsed) = serde_json::from_value::<ResponsesResponse>(value.clone()) {
        if let Some(text) = extract_responses_output(parsed.output.as_deref()) {
            return Some(text);
        }
    }

    if let Some(item) = value.get("item") {
        if let Ok(parsed_item) = serde_json::from_value::<ResponsesOutputItem>(item.clone()) {
            return extract_responses_output(Some(&[parsed_item]));
        }
    }

    value
        .get("response")
        .and_then(extract_responses_output_from_value)
}

fn log_response_text(text: &Option<String>) {
    let Some(text) = text else {
        return;
    };

    if llm_diag_enabled() {
        log::info!("LLM diag response text: {}", text);
    } else {
        log::debug!("LLM response text: {}", text);
    }
}

fn build_system_prompt(
    template: &str,
    dictionary_text: &str,
    history: Option<&[String]>,
    options: PromptBuildOptions,
) -> String {
    let dictionary = format_dictionary(dictionary_text);

    let mut prompt = template.to_string();
    let has_dictionary = prompt.contains(DICTIONARY_PLACEHOLDER);
    if has_dictionary {
        prompt = prompt.replace(DICTIONARY_PLACEHOLDER, &dictionary);
    }

    let has_history = prompt.contains(INPUT_HISTORY_PLACEHOLDER);
    let mut history_append: Option<String> = None;
    match history {
        Some(history) => {
            let history_text = format_history(history);
            if has_history {
                prompt = prompt.replace(INPUT_HISTORY_PLACEHOLDER, &history_text);
            } else {
                history_append = Some(history_text);
            }
        }
        None => {
            if has_history {
                prompt = prompt.replace(INPUT_HISTORY_PLACEHOLDER, "（已关闭）");
            }
        }
    }

    if !has_dictionary && options.append_dictionary_if_missing {
        prompt = format!("{prompt}\n\n用户热词词典：\n{dictionary}");
    }
    if let Some(history_text) = history_append {
        if !options.append_history_if_missing {
            return prompt;
        }
        prompt = format!("{prompt}\n\n用户输入历史供参考：\n{history_text}");
    }

    prompt
}

#[cfg(test)]
mod tests {
    use super::{extract_responses_output_from_value, parse_responses_sse};
    use serde_json::json;

    #[test]
    fn parses_responses_sse_delta_stream() {
        let sse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\"}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"lo\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\"}\n\n"
        );

        assert_eq!(parse_responses_sse(sse).as_deref(), Some("Hello"));
    }

    #[test]
    fn parses_responses_output_item_done_event() {
        let event = json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [
                    { "type": "output_text", "text": "OK" }
                ]
            }
        });

        assert_eq!(
            extract_responses_output_from_value(&event).as_deref(),
            Some("OK")
        );
    }
}

fn format_dictionary(raw: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut cleaned: Vec<String> = Vec::new();
    for line in raw.lines() {
        let word = line.trim();
        if word.is_empty() {
            continue;
        }
        if seen.insert(word.to_string()) {
            cleaned.push(word.to_string());
        }
    }
    if cleaned.is_empty() {
        "（空）".to_string()
    } else {
        cleaned.join("\n")
    }
}

fn format_history(history: &[String]) -> String {
    let mut cleaned: Vec<String> = Vec::new();
    for entry in history {
        let text = entry.trim();
        if text.is_empty() {
            continue;
        }
        cleaned.push(text.to_string());
    }
    if cleaned.is_empty() {
        return "（空）".to_string();
    }

    cleaned
        .iter()
        .enumerate()
        .map(|(idx, text)| format!("{}. {}", idx + 1, text))
        .collect::<Vec<_>>()
        .join("\n")
}

fn mask_api_key(key: &str) -> String {
    if key.is_empty() {
        return "<empty>".to_string();
    }
    let prefix: String = key.chars().take(6).collect();
    let suffix: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{prefix}…{suffix}")
}

fn llm_diag_enabled() -> bool {
    match env::var("VOICEX_LLM_DIAG") {
        Ok(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        }
        Err(_) => false,
    }
}
