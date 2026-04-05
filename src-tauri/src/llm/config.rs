//! LLM configuration

use serde::{Deserialize, Serialize};

/// LLM provider type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LLMProviderType {
    #[default]
    Volcengine, // 火山引擎 (Doubao)
    Openai, // OpenAI
    Qwen,   // 阿里云千问
    Custom, // 自定义 OpenAI 兼容
}

impl LLMProviderType {
    pub fn from_str(s: &str) -> Self {
        match s {
            "openai" => Self::Openai,
            "qwen" => Self::Qwen,
            "custom" => Self::Custom,
            _ => Self::Volcengine,
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
    /// Volcengine-specific: reasoning effort level
    pub volcengine_reasoning_effort: Option<String>,
}

impl Default for LLMConfig {
    fn default() -> Self {
        Self {
            provider_type: LLMProviderType::default(),
            base_url: "https://ark.cn-beijing.volces.com/api/v3".to_string(),
            api_key: String::new(),
            model_name: "doubao-seed-2-0-mini-260215".to_string(),
            api_mode: LLMApiMode::default(),
            volcengine_reasoning_effort: Some("minimal".to_string()),
        }
    }
}

impl LLMConfig {
    pub fn is_valid(&self) -> bool {
        !self.api_key.is_empty()
    }
}
