//! LLM correction module

mod client;
mod config;
mod provider;
pub mod reasoning;
mod timeout;

pub use client::{LLMClient, LLMError, PromptBuildOptions};
pub use config::{LLMApiMode, LLMConfig, LLMProviderType};
pub use provider::{create_provider, LLMProvider, Message};
pub use reasoning::ReasoningChoice;
pub use timeout::correction_timeout_for_text;
