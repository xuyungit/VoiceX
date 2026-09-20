//! LLM configuration

use super::reasoning::ReasoningChoice;
use serde::{Deserialize, Serialize};

/// LLM provider type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LLMProviderType {
    #[default]
    Volcengine, // 火山引擎 (Doubao)
    Openai, // OpenAI
    Qwen,   // 阿里云千问
    Gemini, // Google Gemini
    Custom, // 自定义 OpenAI 兼容
}

impl LLMProviderType {
    pub fn from_str(s: &str) -> Self {
        match s {
            "openai" => Self::Openai,
            "qwen" => Self::Qwen,
            "gemini" => Self::Gemini,
            "custom" => Self::Custom,
            _ => Self::Volcengine,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Volcengine => "Volcengine Doubao",
            Self::Openai => "OpenAI",
            Self::Qwen => "Qwen",
            Self::Gemini => "Google Gemini",
            Self::Custom => "Custom",
        }
    }
}

/// OpenAI-compatible API mode
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LLMApiMode {
    #[default]
    ChatCompletions,
    Responses,
}

impl LLMApiMode {
    pub fn from_str(s: &str) -> Self {
        match s {
            "responses" => Self::Responses,
            _ => Self::ChatCompletions,
        }
    }
}

/// LLM service configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMConfig {
    pub provider_type: LLMProviderType,
    pub base_url: String,
    pub api_key: String,
    pub model_name: String,
    pub api_mode: LLMApiMode,
    /// How much the model may think. The default asks for the lowest this
    /// endpoint accepts, which `reasoning::plan` spells per vendor.
    pub reasoning: ReasoningChoice,
    /// Extra JSON object merged into every request body, the way llm-bench's
    /// `[provider.extra]` works. This is how endpoint-specific thinking knobs
    /// reach the wire (`enable_thinking: false` on DashScope,
    /// `thinking: {"type": "disabled"}` on DeepSeek, ...). Keys here override
    /// the fields the provider builds.
    pub extra_body: Option<String>,
}

impl Default for LLMConfig {
    fn default() -> Self {
        Self {
            provider_type: LLMProviderType::default(),
            base_url: "https://ark.cn-beijing.volces.com/api/v3".to_string(),
            api_key: String::new(),
            model_name: "doubao-seed-2-0-mini-260215".to_string(),
            api_mode: LLMApiMode::default(),
            reasoning: ReasoningChoice::default(),
            extra_body: None,
        }
    }
}

impl LLMConfig {
    pub fn is_valid(&self) -> bool {
        !self.api_key.trim().is_empty()
    }

    pub fn missing_request_fields(&self) -> Vec<&'static str> {
        let mut fields = Vec::new();
        if self.base_url.trim().is_empty() {
            fields.push("base URL");
        }
        if self.api_key.trim().is_empty() {
            fields.push("API key");
        }
        if self.model_name.trim().is_empty() {
            fields.push("model name");
        }
        fields
    }
}
