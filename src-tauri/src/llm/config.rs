//! LLM configuration

use super::reasoning::ReasoningChoice;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelDefaults {
    volcengine_default_model: String,
    volcengine_retired_default_model: String,
    openai_default_model: String,
    openai_previous_default_models: Vec<String>,
}

fn model_defaults() -> &'static ModelDefaults {
    static DEFAULTS: OnceLock<ModelDefaults> = OnceLock::new();
    DEFAULTS.get_or_init(|| {
        serde_json::from_str(include_str!("../../../src/shared/llmModels.json"))
            .expect("bundled LLM model defaults must be valid")
    })
}

pub fn default_volcengine_model() -> &'static str {
    &model_defaults().volcengine_default_model
}

/// Only the exact former bundled default is eligible; deployment IDs stay intact.
pub fn replacement_for_retired_volcengine_default(model: &str) -> Option<&'static str> {
    (model == model_defaults().volcengine_retired_default_model).then(default_volcengine_model)
}

pub fn default_openai_model() -> &'static str {
    &model_defaults().openai_default_model
}

pub fn replacement_for_previous_openai_default(model: &str) -> Option<&'static str> {
    model_defaults()
        .openai_previous_default_models
        .iter()
        .any(|previous| previous == model)
        .then(default_openai_model)
}

/// LLM provider type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LLMProviderType {
    #[default]
    Volcengine, // 火山引擎 (Doubao)
    Openai, // OpenAI
    Qwen,   // 阿里云千问
    Gemini,    // Google Gemini
    Anthropic, // Anthropic Claude (native Messages API)
    Custom,    // 自定义 OpenAI 兼容
}

impl LLMProviderType {
    pub fn from_str(s: &str) -> Self {
        match s {
            "openai" => Self::Openai,
            "qwen" => Self::Qwen,
            "gemini" => Self::Gemini,
            "anthropic" => Self::Anthropic,
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
            Self::Anthropic => "Anthropic",
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
            model_name: default_volcengine_model().to_string(),
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
