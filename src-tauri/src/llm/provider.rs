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
    fn build_request(&self, messages: Vec<Message>, config: &LLMConfig) -> Value;

    /// Get the display name for logging
    fn name(&self) -> &'static str;
}

/// Create the appropriate provider based on config
pub fn create_provider(provider_type: &LLMProviderType) -> Box<dyn LLMProvider> {
    match provider_type {
        LLMProviderType::Volcengine => Box::new(VolcengineProvider),
        LLMProviderType::Openai => Box::new(OpenAIProvider),
        LLMProviderType::Qwen => Box::new(QwenProvider),
        LLMProviderType::Custom => Box::new(CustomProvider),
    }
}

// =============================================================================
// Volcengine Provider (Doubao)
// =============================================================================

pub struct VolcengineProvider;

impl LLMProvider for VolcengineProvider {
    fn build_request(&self, messages: Vec<Message>, config: &LLMConfig) -> Value {
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
    fn build_request(&self, messages: Vec<Message>, config: &LLMConfig) -> Value {
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
    fn build_request(&self, messages: Vec<Message>, config: &LLMConfig) -> Value {
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
    fn build_request(&self, messages: Vec<Message>, config: &LLMConfig) -> Value {
        // Generic OpenAI-compatible format
        serde_json::json!({
            "model": config.model_name,
            "messages": messages,
            "temperature": 0.2,
            "max_tokens": 4096
        })
    }

    fn name(&self) -> &'static str {
        "Custom"
    }
}
