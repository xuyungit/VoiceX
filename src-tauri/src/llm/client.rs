//! LLM client for text correction

use super::config::LLMConfig;
use super::provider::{create_provider, LLMProvider, Message};
use reqwest::Client;
use serde::Deserialize;
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

        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let system_prompt = build_system_prompt(prompt_template, dictionary_text, history, options);
        let user_message = format!("原文：\n{}", text);

        let messages = vec![
            Message {
                role: "system".to_string(),
                content: system_prompt,
            },
            Message {
                role: "user".to_string(),
                content: user_message,
            },
        ];

        let payload = self.provider.build_request(messages, &self.config);
        let request_body =
            serde_json::to_vec(&payload).map_err(|e| LLMError::InvalidRequest(e.to_string()))?;
        let body_preview = String::from_utf8_lossy(&request_body);
        let llm_diag = llm_diag_enabled();

        log::info!(
            "LLM request provider={} model={} endpoint={}",
            self.provider.name(),
            self.config.model_name,
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

        let parsed: ChatResponse =
            serde_json::from_slice(&bytes).map_err(|e| LLMError::InvalidResponse(e.to_string()))?;
        let content = parsed
            .choices
            .get(0)
            .and_then(|c| c.message.content.as_ref())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        if let Some(text) = content {
            if llm_diag {
                log::info!("LLM diag response text: {}", text);
            } else {
                log::debug!("LLM response text: {}", text);
            }
            return Ok(text);
        }

        Err(LLMError::EmptyResponse)
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
