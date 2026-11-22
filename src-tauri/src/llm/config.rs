//! LLM configuration

use super::reasoning::ReasoningChoice;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelDefaults {
    volcengine_default_model: String,
    volcengine_retired_default_model: String,
    openai_base_url: String,
    openai_regional_base_urls: Vec<String>,
    openai_default_model: String,
    deepseek_base_url: String,
    deepseek_default_model: String,
    gemini_base_url: String,
    gemini_default_model: String,
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

pub fn default_openai_base_url() -> &'static str {
    &model_defaults().openai_base_url
}

pub fn is_official_openai_base_url(base_url: &str) -> bool {
    let base_url = base_url.trim().trim_end_matches('/');
    base_url == default_openai_base_url()
        || model_defaults()
            .openai_regional_base_urls
            .iter()
            .any(|url| url == base_url)
}

pub fn default_deepseek_base_url() -> &'static str {
    &model_defaults().deepseek_base_url
}

pub fn default_deepseek_model() -> &'static str {
    &model_defaults().deepseek_default_model
}

pub fn default_gemini_base_url() -> &'static str {
    &model_defaults().gemini_base_url
}

pub fn default_gemini_model() -> &'static str {
    &model_defaults().gemini_default_model
}

pub fn is_official_gemini_base_url(base_url: &str) -> bool {
    let base_url = base_url.trim().trim_end_matches('/');
    let official = default_gemini_base_url().trim_end_matches('/');
    base_url == official
        || base_url == format!("{official}/v1beta")
        || base_url == format!("{official}/v1")
        || base_url == format!("{official}/v1alpha")
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
    Deepseek, // DeepSeek official API
    Gemini,    // Google Gemini
    Anthropic, // Anthropic Claude (native Messages API)
    Custom,    // 自定义 OpenAI 兼容
}

impl LLMProviderType {
    pub fn from_str(s: &str) -> Self {
        match s {
            "openai" => Self::Openai,
            "qwen" => Self::Qwen,
            "deepseek" => Self::Deepseek,
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
            Self::Deepseek => "DeepSeek",
            Self::Gemini => "Google Gemini",
            Self::Anthropic => "Anthropic",
            Self::Custom => "Custom",
        }
    }
}

/// API protocol mode for OpenAI-compatible and custom endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LLMApiMode {
    #[default]
    ChatCompletions,
    Responses,
    Gemini,
}

impl LLMApiMode {
    pub fn from_str(s: &str) -> Self {
        match s {
            "responses" => Self::Responses,
            "gemini" => Self::Gemini,
            _ => Self::ChatCompletions,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OpenAiServiceTier {
    #[default]
    Default,
    Fast,
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
    /// Service tier for the official OpenAI API.
    pub openai_service_tier: Option<OpenAiServiceTier>,
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
            openai_service_tier: None,
            extra_body: None,
        }
    }
}

impl LLMConfig {
    pub fn is_gemini_protocol(&self) -> bool {
        self.provider_type == LLMProviderType::Gemini
            || (self.provider_type == LLMProviderType::Custom && self.api_mode == LLMApiMode::Gemini)
    }

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
