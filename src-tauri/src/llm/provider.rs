//! LLM Provider trait and implementations

use super::config::{LLMConfig, LLMProviderType};
use super::reasoning;
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

/// Put the reasoning fields `config` resolves to into `target`: the request
/// body, or Gemini's `generation_config`.
fn insert_reasoning_fields(target: &mut Value, config: &LLMConfig) {
    if let Some(object) = target.as_object_mut() {
        object.extend(reasoning::plan(config).fields);
    }
}

// =============================================================================
// Volcengine Provider (Doubao)
// =============================================================================

pub struct VolcengineProvider;

impl LLMProvider for VolcengineProvider {
    fn build_chat_request(&self, messages: Vec<Message>, config: &LLMConfig) -> Value {
        let mut req = serde_json::json!({
            "model": config.model_name,
            "messages": messages,
            "temperature": 0.2
        });
        insert_reasoning_fields(&mut req, config);
        req
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
        let mut req = serde_json::json!({
            "model": config.model_name,
            "messages": messages
        });
        insert_reasoning_fields(&mut req, config);
        req
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
        let mut req = serde_json::json!({
            "model": config.model_name,
            "messages": messages,
            "temperature": 0.2
        });
        insert_reasoning_fields(&mut req, config);
        req
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
        // Generic OpenAI-compatible format. No output cap: a fixed
        // `max_tokens` is what a reasoning model spends on thinking before it
        // gets to answer, and a long translation needs the whole budget.
        let mut req = serde_json::json!({
            "model": config.model_name,
            "messages": messages,
            "temperature": 0.2
        });
        insert_reasoning_fields(&mut req, config);
        req
    }

    fn build_responses_request(
        &self,
        instructions: &str,
        input_text: &str,
        config: &LLMConfig,
    ) -> Value {
        let mut req = serde_json::json!({
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
            "stream": true
        });
        insert_reasoning_fields(&mut req, config);
        req
    }

    fn name(&self) -> &'static str {
        "Custom"
    }
}

// =============================================================================
// Gemini Provider (Google Generative AI)
// =============================================================================

pub struct GeminiProvider;

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
            "temperature": 0.2
        });
        insert_reasoning_fields(&mut generation_config, config);

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
    use super::super::config::LLMApiMode;
    use super::super::reasoning::ReasoningChoice;
    use super::*;

    fn config_for(provider_type: LLMProviderType, reasoning: ReasoningChoice) -> LLMConfig {
        LLMConfig {
            provider_type,
            base_url: "https://example.test/v1".to_string(),
            api_key: "k".to_string(),
            model_name: "m".to_string(),
            api_mode: LLMApiMode::ChatCompletions,
            reasoning,
            extra_body: None,
        }
    }

    fn effort(value: &str) -> ReasoningChoice {
        ReasoningChoice::Effort(value.to_string())
    }

    fn user_message() -> Vec<Message> {
        vec![Message {
            role: "user".to_string(),
            content: "hi".to_string(),
        }]
    }

    #[test]
    fn test_gemini_provider_build_chat_request() {
        let provider = GeminiProvider;
        let mut config = config_for(LLMProviderType::Gemini, ReasoningChoice::Lowest);
        config.model_name = "gemini-3.5-flash-lite".to_string();

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
    fn gemini_thinking_goes_into_the_generation_config() {
        let mut config = config_for(LLMProviderType::Gemini, ReasoningChoice::Lowest);
        config.model_name = "gemini-3.8-flash".to_string();
        let req = GeminiProvider.build_chat_request(user_message(), &config);
        assert_eq!(req["generation_config"]["thinkingConfig"]["thinkingLevel"], "LOW");
        assert!(req.get("thinkingConfig").is_none());
        assert!(req["generation_config"].get("maxOutputTokens").is_none());
    }

    #[test]
    fn test_create_gemini_provider() {
        let provider = create_provider(&LLMProviderType::Gemini);
        assert_eq!(provider.name(), "Gemini");
    }

    #[test]
    fn volcengine_turns_thinking_off_unless_an_effort_is_chosen() {
        let lowest = VolcengineProvider.build_chat_request(
            user_message(),
            &config_for(LLMProviderType::Volcengine, ReasoningChoice::Lowest),
        );
        assert_eq!(lowest["thinking"]["type"], "disabled");
        assert!(lowest.get("reasoning_effort").is_none());

        let chosen = VolcengineProvider.build_chat_request(
            user_message(),
            &config_for(LLMProviderType::Volcengine, effort("medium")),
        );
        assert_eq!(chosen["reasoning_effort"], "medium");
        assert!(chosen.get("thinking").is_none());
    }

    #[test]
    fn openai_chat_request_sends_no_output_cap_and_the_models_own_floor() {
        let mut config = config_for(LLMProviderType::Openai, ReasoningChoice::Lowest);
        config.model_name = "gpt-5.6-luna".to_string();
        let lowest = OpenAIProvider.build_chat_request(user_message(), &config);
        assert!(lowest.get("max_completion_tokens").is_none());
        assert_eq!(lowest["reasoning_effort"], "none");

        config.model_name = "gpt-4.1-mini".to_string();
        let plain = OpenAIProvider.build_chat_request(user_message(), &config);
        assert!(plain.get("reasoning_effort").is_none());
    }

    #[test]
    fn custom_chat_request_sends_no_output_cap_and_nothing_to_an_unknown_host() {
        let quiet = CustomProvider.build_chat_request(
            user_message(),
            &config_for(LLMProviderType::Custom, ReasoningChoice::Lowest),
        );
        assert!(quiet.get("max_tokens").is_none());
        assert!(quiet.get("max_completion_tokens").is_none());
        assert!(quiet.get("reasoning_effort").is_none());

        let none = CustomProvider.build_chat_request(
            user_message(),
            &config_for(LLMProviderType::Custom, effort("none")),
        );
        assert_eq!(none["reasoning_effort"], "none");
    }

    #[test]
    fn custom_chat_request_uses_the_hosts_own_spelling() {
        let mut config = config_for(LLMProviderType::Custom, ReasoningChoice::Lowest);
        config.base_url = "https://api.deepseek.com".to_string();
        let req = CustomProvider.build_chat_request(user_message(), &config);
        assert_eq!(req["thinking"]["type"], "disabled");

        config.reasoning = ReasoningChoice::ServerDefault;
        let silent = CustomProvider.build_chat_request(user_message(), &config);
        assert!(silent.get("thinking").is_none());
        assert!(silent.get("reasoning_effort").is_none());
    }

    #[test]
    fn custom_responses_request_nests_the_effort_and_sends_no_output_cap() {
        let mut config = config_for(LLMProviderType::Custom, effort("low"));
        config.api_mode = LLMApiMode::Responses;
        let req = CustomProvider.build_responses_request("sys", "hi", &config);
        assert_eq!(req["reasoning"]["effort"], "low");
        assert!(req.get("reasoning_effort").is_none());
        assert!(req.get("max_output_tokens").is_none());

        config.reasoning = ReasoningChoice::ServerDefault;
        let quiet = CustomProvider.build_responses_request("sys", "hi", &config);
        assert!(quiet.get("reasoning").is_none());
    }

    #[test]
    fn qwen_turns_thinking_off_and_sends_no_cap() {
        let qwen = QwenProvider.build_chat_request(
            user_message(),
            &config_for(LLMProviderType::Qwen, ReasoningChoice::Lowest),
        );
        assert!(qwen.get("max_tokens").is_none());
        assert_eq!(qwen["enable_thinking"], false);
    }
}
