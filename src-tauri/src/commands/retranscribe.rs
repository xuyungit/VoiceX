//! Re-transcribe commands — re-run ASR/LLM on an existing audio file and optionally inject it.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;

use crate::asr::{transcribe_audio_path, transcribe_audio_path_detailed, AsrConfig};
use crate::commands::settings::AppSettings;
use crate::foreground_app::{detect_foreground_app, match_text_injection_override};
use crate::injector::{inject_serialized, TextInjectionMode};
use crate::llm::{LLMApiMode, LLMClient, LLMConfig, LLMProviderType, PromptBuildOptions};
use crate::services::{
    history_service::HistoryService, post_processing_service::PostProcessingService,
};
use crate::state::ProcessingIntent;
use crate::storage;

static RETRANSCRIBE_CANCEL: std::sync::OnceLock<Mutex<Option<CancellationToken>>> =
    std::sync::OnceLock::new();

fn cancel_store() -> &'static Mutex<Option<CancellationToken>> {
    RETRANSCRIBE_CANCEL.get_or_init(|| Mutex::new(None))
}

const OVERALL_TIMEOUT_SECS: u64 = 300;
const LLM_TIMEOUT_SECS: u64 = 8;
const DEFAULT_REPLAY_DELAY_MS: u64 = 3_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReTranscribeRequest {
    pub audio_path: String,
    pub asr_provider: String,
    pub enable_llm: bool,
    pub llm_provider: Option<String>,
    #[serde(default)]
    pub history_mode: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReTranscribeResult {
    pub asr_text: String,
    pub llm_text: Option<String>,
    pub final_text: String,
    pub asr_model_name: String,
    pub llm_model_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayHistoryInjectionRequest {
    pub audio_path: String,
    pub asr_provider: String,
    pub enable_llm: bool,
    pub llm_provider: Option<String>,
    #[serde(default)]
    pub history_mode: Option<String>,
    #[serde(default = "default_replay_delay_ms")]
    pub delay_ms: u64,
}

fn default_replay_delay_ms() -> u64 {
    DEFAULT_REPLAY_DELAY_MS
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayHistoryInjectionResult {
    pub asr_text: String,
    pub llm_text: Option<String>,
    pub final_text: String,
    pub asr_model_name: String,
    pub llm_model_name: Option<String>,
    pub target_app_name: Option<String>,
    pub injection_mode: String,
}

struct PipelineResult {
    asr_text: String,
    llm_text: Option<String>,
    final_text: String,
    asr_model_name: String,
    llm_model_name: Option<String>,
}

#[tauri::command]
pub async fn re_transcribe(request: ReTranscribeRequest) -> Result<ReTranscribeResult, String> {
    let path = ensure_audio_file(&request.audio_path)?;
    let settings = load_settings_with_provider_overrides(
        &request.asr_provider,
        request.llm_provider.as_ref(),
    )?;
    let intent = intent_from_history_mode(request.history_mode.as_deref());

    let cancel = CancellationToken::new();
    {
        let mut store = cancel_store().lock().unwrap();
        *store = Some(cancel.clone());
    }

    let result = tokio::time::timeout(
        Duration::from_secs(OVERALL_TIMEOUT_SECS),
        run_transcription_pipeline(&path, &settings, request.enable_llm, intent, cancel),
    )
    .await;

    {
        let mut store = cancel_store().lock().unwrap();
        *store = None;
    }

    match result {
        Ok(Ok(pipeline)) => Ok(ReTranscribeResult {
            asr_text: pipeline.asr_text,
            llm_text: pipeline.llm_text,
            final_text: pipeline.final_text,
            asr_model_name: pipeline.asr_model_name,
            llm_model_name: pipeline.llm_model_name,
        }),
        Ok(Err(err)) => Err(err),
        Err(_) => Err("转录超时，请稍后重试".into()),
    }
}

#[tauri::command]
pub async fn replay_history_injection(
    app: AppHandle,
    request: ReplayHistoryInjectionRequest,
) -> Result<ReplayHistoryInjectionResult, String> {
    let path = ensure_audio_file(&request.audio_path)?;
    let settings = load_settings_with_provider_overrides(
        &request.asr_provider,
        request.llm_provider.as_ref(),
    )?;
    let intent = intent_from_history_mode(request.history_mode.as_deref());

    let cancel = CancellationToken::new();
    {
        let mut store = cancel_store().lock().unwrap();
        *store = Some(cancel.clone());
    }

    let result = tokio::time::timeout(Duration::from_secs(OVERALL_TIMEOUT_SECS), async {
        let started_at = Instant::now();
        let pipeline =
            run_transcription_pipeline(&path, &settings, request.enable_llm, intent, cancel.clone())
                .await?;

        if pipeline.final_text.trim().is_empty() {
            return Err("最终文本为空，已跳过注入".to_string());
        }

        let elapsed_ms = started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        let remaining_delay_ms = request.delay_ms.saturating_sub(elapsed_ms);
        if remaining_delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(remaining_delay_ms)).await;
        }

        if cancel.is_cancelled() {
            return Err("操作已取消".to_string());
        }

        let target_app =
            detect_foreground_app(&app).map_err(|err| format!("获取前台应用失败: {err}"))?;
        if target_app.is_self {
            return Err("请在开始测试后切回目标应用，再试一次".to_string());
        }

        let matched_override =
            match_text_injection_override(&target_app, &settings.text_injection_overrides);
        let injection_mode = matched_override
            .map(|override_item| TextInjectionMode::from_str(&override_item.mode))
            .unwrap_or_else(|| TextInjectionMode::from_str(&settings.text_injection_mode));

        log::info!(
            "Replay injection target captured: display_name={:?}, process_name={:?}, bundle_id={:?}, pid={}, mode={:?}, override_match={:?}",
            target_app.display_name,
            target_app.process_name,
            target_app.bundle_id,
            target_app.process_id,
            injection_mode,
            matched_override.map(|override_item| override_item.app_name.clone())
        );
        log::info!(
            "Replay injecting final text (len={}, intent={:?})",
            pipeline.final_text.chars().count(),
            intent
        );

        let final_text = pipeline.final_text.clone();
        tauri::async_runtime::spawn_blocking(move || inject_serialized(injection_mode, &final_text))
            .await
            .map_err(|err| format!("注入任务失败: {err}"))?
            .map_err(|err| format!("文本注入失败: {err}"))?;

        Ok(ReplayHistoryInjectionResult {
            asr_text: pipeline.asr_text,
            llm_text: pipeline.llm_text,
            final_text: pipeline.final_text,
            asr_model_name: pipeline.asr_model_name,
            llm_model_name: pipeline.llm_model_name,
            target_app_name: preferred_app_name(&target_app),
            injection_mode: injection_mode_label(injection_mode).to_string(),
        })
    })
    .await;

    {
        let mut store = cancel_store().lock().unwrap();
        *store = None;
    }

    match result {
        Ok(inner) => inner,
        Err(_) => Err("测试重放超时，请稍后重试".into()),
    }
}

fn ensure_audio_file(audio_path: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(audio_path);
    if !path.is_file() {
        return Err(format!("录音文件不存在: {}", path.display()));
    }
    Ok(path)
}

fn load_settings_with_provider_overrides(
    asr_provider: &str,
    llm_provider: Option<&String>,
) -> Result<AppSettings, String> {
    let mut settings = storage::get_settings().map_err(|e| format!("加载设置失败: {}", e))?;
    settings.asr_provider_type = asr_provider.to_string();
    if let Some(llm_provider) = llm_provider {
        settings.llm_provider_type = llm_provider.clone();
    }
    Ok(settings)
}

fn intent_from_history_mode(history_mode: Option<&str>) -> ProcessingIntent {
    let Some(mode) = history_mode else {
        return ProcessingIntent::Assistant;
    };

    if mode.trim().to_ascii_lowercase().starts_with("translate_en") {
        ProcessingIntent::TranslateEn
    } else {
        ProcessingIntent::Assistant
    }
}

fn preferred_app_name(app: &crate::foreground_app::ForegroundAppInfo) -> Option<String> {
    app.display_name
        .clone()
        .or_else(|| app.process_name.clone())
}

fn injection_mode_label(mode: TextInjectionMode) -> &'static str {
    match mode {
        TextInjectionMode::Pasteboard => "pasteboard",
        TextInjectionMode::Typing => "typing",
    }
}

async fn run_transcription_pipeline(
    path: &PathBuf,
    settings: &AppSettings,
    enable_llm: bool,
    intent: ProcessingIntent,
    cancel: CancellationToken,
) -> Result<PipelineResult, String> {
    let mut config = AsrConfig::from(settings);
    if !config.is_valid() {
        return Err("所选 ASR 服务配置不完整，请先在设置页面填写相关凭证".into());
    }

    let outcome = transcribe_audio_path_detailed(path, &mut config, cancel.clone()).await?;
    if cancel.is_cancelled() {
        return Err("操作已取消".to_string());
    }

    let asr_text = outcome.text;
    if asr_text.trim().is_empty() {
        return Err("ASR 未能识别出任何文本，请检查录音质量或尝试其他模型".into());
    }

    let (llm_text, llm_model_name) =
        run_llm_correction(&asr_text, settings, enable_llm, intent).await;
    if cancel.is_cancelled() {
        return Err("操作已取消".to_string());
    }

    let asr_model_name = outcome.model_name.unwrap_or_else(|| {
        HistoryService::resolve_asr_model_name(settings).unwrap_or_else(|| "Unknown".to_string())
    });

    let llm_or_asr_text = llm_text.clone().unwrap_or_else(|| asr_text.clone());
    let final_text = if intent == ProcessingIntent::Assistant {
        PostProcessingService::process(
            &llm_or_asr_text,
            settings.remove_trailing_punctuation,
            settings.short_sentence_threshold,
            &settings.replacement_rules,
        )
    } else {
        llm_or_asr_text
    };

    Ok(PipelineResult {
        asr_text,
        llm_text,
        final_text,
        asr_model_name,
        llm_model_name,
    })
}

pub async fn transcribe_with_config(
    path: &PathBuf,
    config: &mut AsrConfig,
    cancel: CancellationToken,
) -> Result<String, String> {
    transcribe_audio_path(path, config, cancel).await
}

async fn run_llm_correction(
    asr_text: &str,
    settings: &AppSettings,
    enable_llm: bool,
    intent: ProcessingIntent,
) -> (Option<String>, Option<String>) {
    if !enable_llm {
        return (None, None);
    }

    let provider_type = LLMProviderType::from_str(&settings.llm_provider_type);
    let (base_url, api_key, model_name, api_mode, volcengine_reasoning_effort) = match provider_type
    {
        LLMProviderType::Volcengine => (
            settings.llm_volcengine_base_url.clone(),
            settings.llm_volcengine_api_key.clone(),
            settings.llm_volcengine_model.clone(),
            LLMApiMode::ChatCompletions,
            settings.llm_volcengine_reasoning_effort.clone(),
        ),
        LLMProviderType::Openai => (
            settings.llm_openai_base_url.clone(),
            settings.llm_openai_api_key.clone(),
            settings.llm_openai_model.clone(),
            LLMApiMode::ChatCompletions,
            None,
        ),
        LLMProviderType::Qwen => (
            settings.llm_qwen_base_url.clone(),
            settings.llm_qwen_api_key.clone(),
            settings.llm_qwen_model.clone(),
            LLMApiMode::ChatCompletions,
            None,
        ),
        LLMProviderType::Custom => (
            settings.llm_custom_base_url.clone(),
            settings.llm_custom_api_key.clone(),
            settings.llm_custom_model.clone(),
            LLMApiMode::from_str(&settings.llm_custom_api_mode),
            None,
        ),
    };

    let client = LLMClient::new(LLMConfig {
        provider_type: provider_type.clone(),
        base_url,
        api_key,
        model_name,
        api_mode,
        volcengine_reasoning_effort,
    });

    let (prompt_template, dictionary_text, history, prompt_options) = match intent {
        ProcessingIntent::Assistant => {
            let history = if settings.enable_llm_history_context {
                Some(HistoryService::new().get_recent_history(5))
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
        ProcessingIntent::TranslateEn => (
            settings.translation_prompt_template.clone(),
            String::new(),
            None,
            PromptBuildOptions {
                append_dictionary_if_missing: false,
                append_history_if_missing: false,
            },
        ),
    };

    let llm_model_name = HistoryService::resolve_llm_model_name(settings);

    match tokio::time::timeout(
        Duration::from_secs(LLM_TIMEOUT_SECS),
        client.correct(
            asr_text.trim(),
            &prompt_template,
            &dictionary_text,
            history.as_deref(),
            prompt_options,
        ),
    )
    .await
    {
        Ok(Ok(corrected)) => (Some(corrected), llm_model_name),
        Ok(Err(err)) => {
            log::warn!("Re-transcribe LLM correction failed: {}", err);
            (None, llm_model_name)
        }
        Err(_) => {
            log::warn!("Re-transcribe LLM correction timed out");
            (None, llm_model_name)
        }
    }
}

#[tauri::command]
pub fn cancel_retranscribe() {
    let store = cancel_store().lock().unwrap();
    if let Some(token) = store.as_ref() {
        token.cancel();
        log::info!("Re-transcribe cancelled by user");
    }
}
