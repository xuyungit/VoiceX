//! Re-transcribe command — re-runs ASR (and optionally LLM) on an existing audio file.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::asr::{transcribe_audio_path, transcribe_audio_path_detailed, AsrConfig};
use crate::llm::{LLMApiMode, LLMClient, LLMConfig, LLMProviderType, PromptBuildOptions};
use crate::services::history_service::HistoryService;
use crate::storage;

static RETRANSCRIBE_CANCEL: std::sync::OnceLock<Mutex<Option<CancellationToken>>> =
    std::sync::OnceLock::new();

fn cancel_store() -> &'static Mutex<Option<CancellationToken>> {
    RETRANSCRIBE_CANCEL.get_or_init(|| Mutex::new(None))
}

const OVERALL_TIMEOUT_SECS: u64 = 300;
const LLM_TIMEOUT_SECS: u64 = 8;
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReTranscribeRequest {
    pub audio_path: String,
    pub asr_provider: String,
    pub enable_llm: bool,
    pub llm_provider: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReTranscribeResult {
    pub asr_text: String,
    pub llm_text: Option<String>,
    pub asr_model_name: String,
    pub llm_model_name: Option<String>,
}

#[tauri::command]
pub async fn re_transcribe(request: ReTranscribeRequest) -> Result<ReTranscribeResult, String> {
    let path = PathBuf::from(&request.audio_path);
    if !path.is_file() {
        return Err(format!("录音文件不存在: {}", path.display()));
    }

    // Load settings and override providers
    let mut settings = storage::get_settings().map_err(|e| format!("加载设置失败: {}", e))?;
    settings.asr_provider_type = request.asr_provider.clone();
    if let Some(ref llm_provider) = request.llm_provider {
        settings.llm_provider_type = llm_provider.clone();
    }

    let mut config = AsrConfig::from(&settings);
    if !config.is_valid() {
        return Err("所选 ASR 服务配置不完整，请先在设置页面填写相关凭证".into());
    }

    // Set up cancellation
    let cancel = CancellationToken::new();
    {
        let mut store = cancel_store().lock().unwrap();
        *store = Some(cancel.clone());
    }

    let result = tokio::time::timeout(
        Duration::from_secs(OVERALL_TIMEOUT_SECS),
        run_retranscribe(&path, &mut config, &settings, &request, cancel.clone()),
    )
    .await;

    // Clear cancellation token
    {
        let mut store = cancel_store().lock().unwrap();
        *store = None;
    }

    match result {
        Ok(inner) => inner,
        Err(_) => Err("转录超时，请稍后重试".into()),
    }
}

async fn run_retranscribe(
    path: &PathBuf,
    config: &mut AsrConfig,
    settings: &crate::commands::settings::AppSettings,
    request: &ReTranscribeRequest,
    cancel: CancellationToken,
) -> Result<ReTranscribeResult, String> {
    // --- ASR ---
    let outcome = transcribe_audio_path_detailed(path, config, cancel.clone()).await?;
    let asr_text = outcome.text;

    if asr_text.trim().is_empty() {
        return Err("ASR 未能识别出任何文本，请检查录音质量或尝试其他模型".into());
    }

    // --- LLM ---
    let (llm_text, llm_model_name) = if request.enable_llm {
        run_llm_correction(&asr_text, settings).await
    } else {
        (None, None)
    };

    // --- Model names ---
    let asr_model_name = outcome.model_name.unwrap_or_else(|| {
        HistoryService::resolve_asr_model_name(settings).unwrap_or_else(|| "Unknown".to_string())
    });

    Ok(ReTranscribeResult {
        asr_text,
        llm_text,
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

/// Run LLM correction on ASR text.
async fn run_llm_correction(
    asr_text: &str,
    settings: &crate::commands::settings::AppSettings,
) -> (Option<String>, Option<String>) {
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

    let history = if settings.enable_llm_history_context {
        Some(HistoryService::new().get_recent_history(5))
    } else {
        None
    };

    let llm_model_name = HistoryService::resolve_llm_model_name(settings);

    match tokio::time::timeout(
        Duration::from_secs(LLM_TIMEOUT_SECS),
        client.correct(
            asr_text.trim(),
            &settings.llm_prompt_template,
            &settings.dictionary_text,
            history.as_deref(),
            PromptBuildOptions::default(),
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
