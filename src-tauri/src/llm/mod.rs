//! LLM correction module

mod client;
mod config;
mod provider;
pub mod reasoning;
mod timeout;
pub mod warm;

pub use client::{LLMClient, LLMError, PromptBuildOptions};
pub use config::{LLMApiMode, LLMConfig, LLMProviderType};
pub use config::{default_volcengine_model, replacement_for_retired_volcengine_default};
pub use config::{
    default_openai_base_url, default_openai_model, is_official_openai_base_url,
    replacement_for_previous_openai_default, OpenAiServiceTier,
};
pub use config::{default_deepseek_base_url, default_deepseek_model};
pub use provider::{create_provider, LLMProvider, Message};
pub use reasoning::ReasoningChoice;
pub use timeout::correction_timeout_for_text;
