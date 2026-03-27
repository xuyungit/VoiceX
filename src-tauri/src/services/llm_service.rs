use crate::{
    llm::{LLMClient, LLMConfig, LLMProviderType, PromptBuildOptions},
    services::history_service::HistoryService,
    state::ProcessingIntent,
    storage,
};
use std::time::Duration;

/// Handles optional LLM correction, returning corrected text plus invocation metadata.
#[derive(Clone, Default)]
pub struct LlmService;

const LLM_HISTORY_LIMIT: u32 = 5;
const LLM_CORRECTION_TIMEOUT_SECS: u64 = 8;

#[derive(Debug, Clone)]
pub struct LlmCorrectionResult {
    pub text: String,
    pub invoked: bool,
    pub changed: bool,
}

impl LlmService {
    pub fn new() -> Self {
        Self
    }

    pub async fn correct_text_if_enabled(
        &self,
        text: &str,
        intent: ProcessingIntent,
    ) -> LlmCorrectionResult {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return LlmCorrectionResult {
                text: text.to_string(),
                invoked: false,
                changed: false,
            };
        }

        let settings = match storage::get_settings() {
            Ok(s) => s,
            Err(e) => {
                log::warn!("LLM correction skipped (load settings failed): {}", e);
                return LlmCorrectionResult {
                    text: text.to_string(),
                    invoked: false,
                    changed: false,
                };
            }
        };

        let provider_type = LLMProviderType::from_str(&settings.llm_provider_type);

        let (base_url, api_key, model_name, volcengine_reasoning_effort) = match provider_type {
            LLMProviderType::Volcengine => (
                settings.llm_volcengine_base_url.clone(),
                settings.llm_volcengine_api_key.clone(),
                settings.llm_volcengine_model.clone(),
                settings.llm_volcengine_reasoning_effort.clone(),
            ),
            LLMProviderType::Openai => (
                settings.llm_openai_base_url.clone(),
                settings.llm_openai_api_key.clone(),
                settings.llm_openai_model.clone(),
                None,
            ),
            LLMProviderType::Qwen => (
                settings.llm_qwen_base_url.clone(),
                settings.llm_qwen_api_key.clone(),
                settings.llm_qwen_model.clone(),
                None,
            ),
            LLMProviderType::Custom => (
                settings.llm_custom_base_url.clone(),
                settings.llm_custom_api_key.clone(),
                settings.llm_custom_model.clone(),
                None,
            ),
        };

        let client = LLMClient::new(LLMConfig {
            provider_type,
            base_url,
            api_key,
            model_name,
            volcengine_reasoning_effort,
        });

        let (prompt_template, dictionary_text, history, prompt_options) = match intent {
            ProcessingIntent::Assistant => {
                if !settings.enable_llm_correction {
                    return LlmCorrectionResult {
                        text: text.to_string(),
                        invoked: false,
                        changed: false,
                    };
                }
                let history = if settings.enable_llm_history_context {
                    Some(HistoryService::new().get_recent_history(LLM_HISTORY_LIMIT))
                } else {
                    None
                };
                (
                    settings.llm_prompt_template.clone(),
                    settings.dictionary_text.clone(),
                    history,
                    PromptBuildOptions::default(),
                )
            }
            ProcessingIntent::TranslateEn => {
                if !settings.translation_enabled {
                    return LlmCorrectionResult {
                        text: text.to_string(),
                        invoked: false,
                        changed: false,
                    };
                }
                (
                    settings.translation_prompt_template.clone(),
                    String::new(),
                    None,
                    PromptBuildOptions {
                        append_dictionary_if_missing: false,
                        append_history_if_missing: false,
                    },
                )
            }
        };

        match tokio::time::timeout(
            Duration::from_secs(LLM_CORRECTION_TIMEOUT_SECS),
            client.correct(
                trimmed,
                &prompt_template,
                &dictionary_text,
                history.as_deref(),
                prompt_options,
            ),
        )
        .await
        {
            Ok(Ok(corrected)) => {
                let changed = corrected.trim() != trimmed;
                LlmCorrectionResult {
                    text: corrected,
                    invoked: true,
                    changed,
                }
            }
            Ok(Err(err)) => {
                log::warn!("LLM correction failed: {}", err);
                LlmCorrectionResult {
                    text: text.to_string(),
                    invoked: true,
                    changed: false,
                }
            }
            Err(_) => {
                log::warn!(
                    "LLM correction timed out after {}s; using original text",
                    LLM_CORRECTION_TIMEOUT_SECS
                );
                LlmCorrectionResult {
                    text: text.to_string(),
                    invoked: true,
                    changed: false,
                }
            }
        }
    }
}
