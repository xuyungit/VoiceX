use crate::{
    commands::settings::AppSettings,
    llm::{
        correction_timeout_for_text, LLMApiMode, LLMClient, LLMConfig, LLMProviderType,
        PromptBuildOptions,
    },
    services::history_service::HistoryService,
    state::ProcessingIntent,
    storage,
};

/// Handles optional LLM correction, returning corrected text plus invocation metadata.
#[derive(Clone, Default)]
pub struct LlmService;

const LLM_HISTORY_LIMIT: u32 = 5;

pub fn build_llm_config_from_settings(settings: &AppSettings) -> LLMConfig {
    let provider_type = LLMProviderType::from_str(&settings.llm_provider_type);

    match provider_type {
        LLMProviderType::Volcengine => LLMConfig {
            provider_type: LLMProviderType::Volcengine,
            base_url: settings.llm_volcengine_base_url.clone(),
            api_key: settings.llm_volcengine_api_key.clone(),
            model_name: settings.llm_volcengine_model.clone(),
            api_mode: LLMApiMode::ChatCompletions,
            volcengine_reasoning_effort: settings.llm_volcengine_reasoning_effort.clone(),
        },
        LLMProviderType::Openai => LLMConfig {
            provider_type: LLMProviderType::Openai,
            base_url: settings.llm_openai_base_url.clone(),
            api_key: settings.llm_openai_api_key.clone(),
            model_name: settings.llm_openai_model.clone(),
            api_mode: LLMApiMode::ChatCompletions,
            volcengine_reasoning_effort: None,
        },
        LLMProviderType::Qwen => LLMConfig {
            provider_type: LLMProviderType::Qwen,
            base_url: settings.llm_qwen_base_url.clone(),
            api_key: settings.llm_qwen_api_key.clone(),
            model_name: settings.llm_qwen_model.clone(),
            api_mode: LLMApiMode::ChatCompletions,
            volcengine_reasoning_effort: None,
        },
        LLMProviderType::Custom => {
            let endpoint = crate::commands::settings::active_custom_endpoint(settings);
            LLMConfig {
                provider_type: LLMProviderType::Custom,
                base_url: endpoint.map(|e| e.base_url.clone()).unwrap_or_default(),
                api_key: endpoint.map(|e| e.api_key.clone()).unwrap_or_default(),
                model_name: endpoint.map(|e| e.model.clone()).unwrap_or_default(),
                api_mode: endpoint
                    .map(|e| LLMApiMode::from_str(&e.api_mode))
                    .unwrap_or_default(),
                volcengine_reasoning_effort: None,
            }
        }
    }
}

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

        let client = LLMClient::new(build_llm_config_from_settings(&settings));

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

        let correction_timeout = correction_timeout_for_text(trimmed);
        let started_at = std::time::Instant::now();
        let input_chars = trimmed.chars().count();

        let result = tokio::time::timeout(
            correction_timeout,
            client.correct(
                trimmed,
                &prompt_template,
                &dictionary_text,
                history.as_deref(),
                prompt_options,
            ),
        )
        .await;

        let elapsed_ms = started_at.elapsed().as_millis();
        match result {
            Ok(Ok(corrected)) => {
                let changed = corrected.trim() != trimmed;
                log::info!(
                    "LLM correction done intent={:?} elapsed={}ms input_chars={} output_chars={} changed={}",
                    intent,
                    elapsed_ms,
                    input_chars,
                    corrected.chars().count(),
                    changed
                );
                LlmCorrectionResult {
                    text: corrected,
                    invoked: true,
                    changed,
                }
            }
            Ok(Err(err)) => {
                log::warn!(
                    "LLM correction failed intent={:?} elapsed={}ms input_chars={}: {}",
                    intent,
                    elapsed_ms,
                    input_chars,
                    err
                );
                LlmCorrectionResult {
                    text: text.to_string(),
                    invoked: true,
                    changed: false,
                }
            }
            Err(_) => {
                log::warn!(
                    "LLM correction timed out intent={:?} after {}ms (limit={}s) input_chars={}; using original text",
                    intent,
                    elapsed_ms,
                    correction_timeout.as_secs(),
                    input_chars
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
