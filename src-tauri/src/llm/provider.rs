//! LLM Provider trait and implementations

use super::config::{LLMConfig, LLMProviderType};
use serde::Serialize;
use serde_json::Value;

/// Message structure for chat completions
#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// Trait for LLM provider implementations
pub trait LLMProvider: Send + Sync {
    /// Build the request body for the provider
    fn build_chat_request(&self, messages: Vec<Message>, config: &LLMConfig) -> Value;

    /// Build a Responses API request body for the provider.
    fn build_responses_request(
        &self,
        instructions: &str,
        input_text: &str,
        config: &LLMConfig,
    ) -> Value {
        serde_json::json!({
            "model": config.model_name,
            "instructions": instructions,
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": input_text
                }]
            }],
            "stream": true
        })
    }

    /// Get the display name for logging
    fn name(&self) -> &'static str;
}

/// Create the appropriate provider based on config
pub fn create_provider(provider_type: &LLMProviderType) -> Box<dyn LLMProvider> {
    match provider_type {
        LLMProviderType::Volcengine => Box::new(VolcengineProvider),
        LLMProviderType::Openai => Box::new(OpenAIProvider),
        LLMProviderType::Qwen => Box::new(QwenProvider),
        LLMProviderType::Gemini => Box::new(GeminiProvider),
        LLMProviderType::Custom => Box::new(CustomProvider),
    }
}

// =============================================================================
// Volcengine Provider (Doubao)
// =============================================================================

pub struct VolcengineProvider;

impl LLMProvider for VolcengineProvider {
    fn build_chat_request(&self, messages: Vec<Message>, config: &LLMConfig) -> Value {
        let reasoning_effort = config
            .volcengine_reasoning_effort
            .clone()
            .unwrap_or_else(|| "minimal".to_string());

        serde_json::json!({
            "model": config.model_name,
            "messages": messages,
            "temperature": 0.2,
            "reasoning_effort": reasoning_effort
        })
    }

    fn name(&self) -> &'static str {
        "Volcengine"
    }
}

// =============================================================================
// OpenAI Provider
// =============================================================================

pub struct OpenAIProvider;

impl LLMProvider for OpenAIProvider {
    fn build_chat_request(&self, messages: Vec<Message>, config: &LLMConfig) -> Value {
        serde_json::json!({
            "model": config.model_name,
            "messages": messages,
            "max_completion_tokens": 4096
        })
    }

    fn name(&self) -> &'static str {
        "OpenAI"
    }
}

// =============================================================================
// Qwen Provider (Alibaba Cloud DashScope)
// =============================================================================

pub struct QwenProvider;

impl LLMProvider for QwenProvider {
    fn build_chat_request(&self, messages: Vec<Message>, config: &LLMConfig) -> Value {
        serde_json::json!({
            "model": config.model_name,
            "messages": messages,
            "temperature": 0.2,
            "max_tokens": 4096,
            "enable_thinking": false
        })
    }

    fn name(&self) -> &'static str {
        "Qwen"
    }
}

// =============================================================================
// Custom Provider (OpenAI-compatible)
// =============================================================================

pub struct CustomProvider;

impl LLMProvider for CustomProvider {
    fn build_chat_request(&self, messages: Vec<Message>, config: &LLMConfig) -> Value {
        // Generic OpenAI-compatible format
        serde_json::json!({
            "model": config.model_name,
            "messages": messages,
            "temperature": 0.2,
            "max_tokens": 4096
        })
    }

    fn build_responses_request(
        &self,
        instructions: &str,
        input_text: &str,
        config: &LLMConfig,
    ) -> Value {
        serde_json::json!({
            "model": config.model_name,
            "instructions": instructions,
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": input_text
                }]
            }],
            "temperature": 0.2,
            "max_output_tokens": 4096,
            "stream": true
        })
    }

    fn name(&self) -> &'static str {
        "Custom"
    }
}

// =============================================================================
// Gemini Provider (Google Generative AI)
// =============================================================================

pub struct GeminiProvider;

/// Lowest thinking Gemini accepts for this model. 3.7/3.8 Flash reject MINIMAL;
/// 2.5 Flash can set thinkingBudget=0; Flash-Lite already thinks off by default.
fn gemini_thinking_config(model: &str) -> Option<Value> {
    let model = model.to_ascii_lowercase();
    if model.contains("2.5") {
        if model.contains("pro") {
            return Some(serde_json::json!({ "thinkingLevel": "LOW" }));
        }
        return Some(serde_json::json!({ "thinkingBudget": 0 }));
    }
    if model.contains("lite") {
        return None;
    }
    Some(serde_json::json!({ "thinkingLevel": "LOW" }))
}

impl LLMProvider for GeminiProvider {
    fn build_chat_request(&self, messages: Vec<Message>, config: &LLMConfig) -> Value {
        let mut system_instruction: Option<Value> = None;
        let mut contents: Vec<Value> = Vec::new();

        for msg in messages {
            if msg.role == "system" {
                system_instruction = Some(serde_json::json!({
                    "parts": [{ "text": msg.content }]
                }));
            } else {
                let role = if msg.role == "assistant" {
                    "model"
                } else {
                    "user"
                };
                contents.push(serde_json::json!({
                    "role": role,
                    "parts": [{ "text": msg.content }]
                }));
            }
        }

        let mut generation_config = serde_json::json!({
            "temperature": 0.2,
            "maxOutputTokens": 4096
        });
        if let Some(thinking) = gemini_thinking_config(&config.model_name) {
            generation_config["thinkingConfig"] = thinking;
        }

        let mut req = serde_json::json!({
            "contents": contents,
            "generation_config": generation_config
        });

        if let Some(sys) = system_instruction {
            if let Some(obj) = req.as_object_mut() {
                obj.insert("system_instruction".to_string(), sys);
            }
        }

        req
    }

    fn name(&self) -> &'static str {
        "Gemini"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gemini_provider_build_chat_request() {
        let provider = GeminiProvider;
        let config = LLMConfig {
            provider_type: LLMProviderType::Gemini,
            base_url: "https://generativelanguage.googleapis.com".to_string(),
            api_key: "test_key".to_string(),
            model_name: "gemini-3.5-flash-lite".to_string(),
            api_mode: super::super::config::LLMApiMode::ChatCompletions,
            volcengine_reasoning_effort: None,
        };

        let messages = vec![
            Message {
                role: "system".to_string(),
                content: "System prompt".to_string(),
            },
            Message {
                role: "user".to_string(),
                content: "User message".to_string(),
            },
        ];

        let req = provider.build_chat_request(messages, &config);
        assert_eq!(
            req["system_instruction"]["parts"][0]["text"],
            "System prompt"
        );
        assert_eq!(req["contents"][0]["role"], "user");
        assert_eq!(req["contents"][0]["parts"][0]["text"], "User message");
        assert!(req["generation_config"]["thinkingConfig"].is_null());
        assert_eq!(provider.name(), "Gemini");
    }

    #[test]
    fn gemini_flash_uses_low_thinking() {
        assert_eq!(
            gemini_thinking_config("gemini-3.7-flash"),
            Some(serde_json::json!({ "thinkingLevel": "LOW" }))
        );
        assert_eq!(
            gemini_thinking_config("gemini-3.8-flash"),
            Some(serde_json::json!({ "thinkingLevel": "LOW" }))
        );
        assert_eq!(gemini_thinking_config("gemini-3.5-flash-lite"), None);
        assert_eq!(
            gemini_thinking_config("gemini-2.5-flash"),
            Some(serde_json::json!({ "thinkingBudget": 0 }))
        );
    }

    #[test]
    fn test_create_gemini_provider() {
        let provider = create_provider(&LLMProviderType::Gemini);
        assert_eq!(provider.name(), "Gemini");
    }
}
