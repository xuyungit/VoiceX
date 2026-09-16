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

/// `tts_llm_provider_key` value meaning "whatever the LLM page has active".
pub const LLM_PROVIDER_FOLLOW: &str = "follow";

/// The settings as the LLM layer should see them for provider `key`.
///
/// `follow` (or an empty key) is the LLM page's own selection, returned as-is.
/// Any other key — a provider name or `custom:<id>` — is applied onto a copy
/// exactly the way the LLM page would apply it, so `build_llm_config_from_settings`
/// and `HistoryService::resolve_llm_model_name` both see the same provider
/// without either learning about the reading feature.
pub fn settings_for_llm_key<'a>(
    settings: &'a AppSettings,
    key: &str,
) -> std::borrow::Cow<'a, AppSettings> {
    let key = key.trim();
    if key.is_empty() || key == LLM_PROVIDER_FOLLOW {
        return std::borrow::Cow::Borrowed(settings);
    }
    let mut selected = settings.clone();
    crate::commands::settings::apply_llm_provider_selection(&mut selected, key);
    std::borrow::Cow::Owned(selected)
}

pub fn build_llm_config_for_key(settings: &AppSettings, key: &str) -> LLMConfig {
    build_llm_config_from_settings(&settings_for_llm_key(settings, key))
}

pub fn build_llm_config_from_settings(settings: &AppSettings) -> LLMConfig {
    let provider_type = LLMProviderType::from_str(&settings.llm_provider_type);

    match provider_type {
        LLMProviderType::Volcengine => LLMConfig {
            provider_type: LLMProviderType::Volcengine,
            base_url: settings.llm_volcengine_base_url.clone(),
            api_key: settings.llm_volcengine_api_key.clone(),
            model_name: settings.llm_volcengine_model.clone(),
            api_mode: LLMApiMode::ChatCompletions,
            reasoning_effort: settings.llm_volcengine_reasoning_effort.clone(),
            extra_body: None,
        },
        LLMProviderType::Openai => LLMConfig {
            provider_type: LLMProviderType::Openai,
            base_url: settings.llm_openai_base_url.clone(),
            api_key: settings.llm_openai_api_key.clone(),
            model_name: settings.llm_openai_model.clone(),
            api_mode: LLMApiMode::ChatCompletions,
            reasoning_effort: non_empty(settings.llm_openai_reasoning_effort.as_deref()),
            extra_body: None,
        },
        LLMProviderType::Qwen => LLMConfig {
            provider_type: LLMProviderType::Qwen,
            base_url: settings.llm_qwen_base_url.clone(),
            api_key: settings.llm_qwen_api_key.clone(),
            model_name: settings.llm_qwen_model.clone(),
            api_mode: LLMApiMode::ChatCompletions,
            reasoning_effort: None,
            extra_body: None,
        },
        LLMProviderType::Gemini => LLMConfig {
            provider_type: LLMProviderType::Gemini,
            base_url: settings.llm_gemini_base_url.clone(),
            api_key: settings.llm_gemini_api_key.clone(),
            model_name: settings.llm_gemini_model.clone(),
            api_mode: LLMApiMode::ChatCompletions,
            reasoning_effort: None,
            extra_body: None,
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
                reasoning_effort: endpoint.and_then(|e| non_empty(Some(&e.reasoning_effort))),
                extra_body: endpoint.and_then(|e| non_empty(Some(&e.extra_body))),
            }
        }
    }
}

/// A settings string as an optional value: blank means "not set".
fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
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

#[cfg(test)]
mod llm_key_tests {
    use super::*;
    use crate::commands::settings::CustomLlmEndpoint;

    fn settings_with_two_providers() -> AppSettings {
        let mut settings = AppSettings::default();
        settings.llm_provider_type = "volcengine".to_string();
        settings.llm_volcengine_api_key = "volc-key".to_string();
        settings.llm_volcengine_model = "doubao".to_string();
        settings.llm_openai_api_key = "openai-key".to_string();
        settings.llm_openai_model = "gpt-x".to_string();
        settings.llm_custom_endpoints.push(CustomLlmEndpoint {
            id: "ep1".to_string(),
            name: "Mine".to_string(),
            base_url: "http://localhost:1".to_string(),
            api_key: "custom-key".to_string(),
            model: "local-model".to_string(),
            api_mode: "responses".to_string(),
            reasoning_effort: "  ".to_string(),
            extra_body: String::new(),
        });
        settings
    }

    #[test]
    fn blank_endpoint_knobs_send_nothing() {
        let config = build_llm_config_for_key(&settings_with_two_providers(), "custom:ep1");
        assert_eq!(config.reasoning_effort, None);
        assert_eq!(config.extra_body, None);
    }

    #[test]
    fn openai_reasoning_effort_is_optional() {
        let mut settings = settings_with_two_providers();
        settings.llm_provider_type = "openai".to_string();
        assert_eq!(build_llm_config_from_settings(&settings).reasoning_effort, None);
        settings.llm_openai_reasoning_effort = Some("minimal".to_string());
        assert_eq!(
            build_llm_config_from_settings(&settings).reasoning_effort.as_deref(),
            Some("minimal")
        );
    }

    #[test]
    fn follow_uses_the_llm_pages_active_provider() {
        let settings = settings_with_two_providers();
        for key in ["follow", "", "  "] {
            let config = build_llm_config_for_key(&settings, key);
            assert_eq!(config.provider_type, LLMProviderType::Volcengine, "key {key:?}");
            assert_eq!(config.api_key, "volc-key");
        }
    }

    #[test]
    fn a_provider_key_selects_that_provider_without_touching_the_original() {
        let settings = settings_with_two_providers();
        let config = build_llm_config_for_key(&settings, "openai");
        assert_eq!(config.provider_type, LLMProviderType::Openai);
        assert_eq!(config.api_key, "openai-key");
        assert_eq!(config.model_name, "gpt-x");
        assert_eq!(
            settings.llm_provider_type, "volcengine",
            "the LLM page's own selection is not what the reading feature picked"
        );
    }

    #[test]
    fn a_custom_key_resolves_the_endpoint_and_its_api_mode() {
        let settings = settings_with_two_providers();
        let config = build_llm_config_for_key(&settings, "custom:ep1");
        assert_eq!(config.provider_type, LLMProviderType::Custom);
        assert_eq!(config.api_key, "custom-key");
        assert_eq!(config.model_name, "local-model");
        assert_eq!(config.api_mode, LLMApiMode::Responses);
        assert_eq!(
            HistoryService::resolve_llm_model_name(&settings_for_llm_key(&settings, "custom:ep1"))
                .as_deref(),
            Some("Mine / local-model"),
            "history names the provider the read actually used"
        );
    }

    #[test]
    fn a_provider_with_no_key_yields_an_invalid_config_rather_than_a_fallback() {
        // Visibility over concealment: the HUD reports "not configured"
        // instead of the read quietly using a different provider.
        let settings = settings_with_two_providers();
        let config = build_llm_config_for_key(&settings, "gemini");
        assert_eq!(config.provider_type, LLMProviderType::Gemini);
        assert!(!config.is_valid());
    }
}
