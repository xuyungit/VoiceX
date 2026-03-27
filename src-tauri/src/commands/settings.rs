//! Settings-related commands

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::services::sync_service::SyncService;
use crate::session::SessionController;

/// Application settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    // ASR settings
    pub asr_provider_type: String, // "volcengine" | "google" | "qwen" | "coli"
    pub asr_app_key: String,
    pub asr_access_key: String,
    pub asr_resource_id: String,
    pub asr_ws_url: String,
    pub enable_nonstream: bool,
    pub end_window_size: Option<u32>,
    pub force_to_speech_time: Option<u32>,
    pub enable_ddc: bool,
    pub enable_asr_context: bool,

    // ASR Provider: Google Cloud Speech-to-Text V2
    pub google_stt_api_key: String,
    pub google_stt_project_id: String,
    pub google_stt_language_code: String,
    pub google_stt_location: String,
    pub google_stt_endpointing: String, // "supershort" | "short" | "standard"
    pub google_stt_phrase_boost: f32,

    // ASR Provider: Qwen Realtime ASR
    pub qwen_asr_api_key: String,
    pub qwen_asr_model: String,
    pub qwen_asr_ws_url: String,
    pub qwen_asr_language: String,

    // Local ASR Provider: `coli`
    pub coli_command_path: String,
    pub coli_use_vad: bool,
    pub coli_asr_interval_ms: u32,
    pub coli_final_refinement_mode: String, // "off" | "sensevoice" | "whisper"

    // LLM settings
    pub enable_llm_correction: bool,
    pub llm_provider_type: String, // "volcengine" | "openai" | "qwen" | "custom"
    pub llm_prompt_template: String,
    pub translation_prompt_template: String,
    pub enable_llm_history_context: bool,
    pub translation_enabled: bool,
    pub translation_trigger_mode: String, // "double_tap" | "off"
    pub translation_target_language: String, // "en"
    pub double_tap_window_ms: u32,

    // LLM Provider: Volcengine
    pub llm_volcengine_base_url: String,
    pub llm_volcengine_api_key: String,
    pub llm_volcengine_model: String,
    pub llm_volcengine_reasoning_effort: Option<String>,

    // LLM Provider: OpenAI
    pub llm_openai_base_url: String,
    pub llm_openai_api_key: String,
    pub llm_openai_model: String,

    // LLM Provider: Qwen (DashScope)
    pub llm_qwen_base_url: String,
    pub llm_qwen_api_key: String,
    pub llm_qwen_model: String,

    // LLM Provider: Custom
    pub llm_custom_base_url: String,
    pub llm_custom_api_key: String,
    pub llm_custom_model: String,

    // Hotkey settings
    pub hotkey_config: Option<String>,
    pub hold_threshold_ms: u32,
    pub max_recording_minutes: u32,

    // Input
    pub input_device_uid: Option<String>,
    pub text_injection_mode: String, // "pasteboard" or "typing"

    // Sync
    pub sync_enabled: bool,
    pub sync_server_url: String,
    pub sync_token: String,
    pub sync_shared_secret: String,
    pub sync_device_name: String,

    // Retention
    pub audio_retention_days: u32,
    pub text_retention_days: u32,

    // Dictionary
    pub dictionary_text: String,

    // Post-processing settings
    pub remove_trailing_punctuation: bool,
    pub short_sentence_threshold: u32,
    pub replacement_rules: Vec<ReplacementRule>,

    // Online Hotword Management
    pub volc_access_key: String,
    pub volc_secret_key: String,
    pub volc_app_id: String,
    pub online_hotword_id: String,
    pub remote_hotword_updated_at: String,
    pub local_hotword_updated_at: String,

    // Diagnostics
    pub enable_diagnostics: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplacementRule {
    pub id: String,
    pub keyword: String,
    pub replacement: String,
    pub match_mode: String, // "exact" | "contains" | "regex"
    pub enabled: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            asr_provider_type: "volcengine".to_string(),
            asr_app_key: String::new(),
            asr_access_key: String::new(),
            asr_resource_id: "volc.seedasr.sauc.duration".to_string(),
            asr_ws_url: "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async".to_string(),
            enable_nonstream: false,
            end_window_size: Some(1400),
            force_to_speech_time: Some(3500),
            enable_ddc: true,
            enable_asr_context: false,

            google_stt_api_key: String::new(),
            google_stt_project_id: String::new(),
            google_stt_language_code: "cmn-Hans-CN, en-US".to_string(),
            google_stt_location: "us".to_string(),
            google_stt_endpointing: "supershort".to_string(),
            google_stt_phrase_boost: 8.0,

            qwen_asr_api_key: String::new(),
            qwen_asr_model: "qwen3-asr-flash-realtime".to_string(),
            qwen_asr_ws_url: "wss://dashscope.aliyuncs.com/api-ws/v1/realtime".to_string(),
            qwen_asr_language: "zh".to_string(),
            coli_command_path: String::new(),
            coli_use_vad: false,
            coli_asr_interval_ms: 1000,
            coli_final_refinement_mode: "off".to_string(),

            enable_llm_correction: false,
            llm_provider_type: "volcengine".to_string(),
            llm_prompt_template: "你是一个语音转写文本纠正助手。\n\n你的任务：\n- 修正语音识别文本中的识别错误、同音字错误、错别字和标点问题\n- 保持原意，不增删信息\n- 当识别结果中出现与用户词典中词汇发音相似、拼写接近或语义相关的词时，将其替换为词典中的标准形式\n- 不要更改词典中词汇的拼写、大小写或符号\n- 即便识别文本中的英文和用户词典的词汇语义相似，不要用用户词典中的词汇去替换原文中的英文\n\n用户热词词典：\n{{DICTIONARY}}\n\n输出：\n纠正后的文本或原文（不要输出任何其他内容）".to_string(),
            translation_prompt_template: "你是一个专业翻译助手。\n\n你的任务：\n- 将用户提供的原文准确翻译成英文\n- 保持原意，不增删信息\n- 保留专有名词、数字、代码片段与格式\n- 如果原文已经是英文，只做必要润色并保持原意\n\n输出：\n只输出英文结果，不要输出解释或额外说明".to_string(),
            enable_llm_history_context: false,
            translation_enabled: true,
            translation_trigger_mode: "double_tap".to_string(),
            translation_target_language: "en".to_string(),
            double_tap_window_ms: 400,

            llm_volcengine_base_url: "https://ark.cn-beijing.volces.com/api/v3".to_string(),
            llm_volcengine_api_key: String::new(),
            llm_volcengine_model: "doubao-seed-2-0-mini-260215".to_string(),
            llm_volcengine_reasoning_effort: Some("minimal".to_string()),

            llm_openai_base_url: "https://api.openai.com/v1".to_string(),
            llm_openai_api_key: String::new(),
            llm_openai_model: "gpt-4o-mini".to_string(),

            llm_qwen_base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string(),
            llm_qwen_api_key: String::new(),
            llm_qwen_model: "qwen3.5-flash".to_string(),

            llm_custom_base_url: String::new(),
            llm_custom_api_key: String::new(),
            llm_custom_model: String::new(),

            hotkey_config: None,
            hold_threshold_ms: 1000,
            max_recording_minutes: 5,

            input_device_uid: None,
            text_injection_mode: "pasteboard".to_string(),

            sync_enabled: false,
            sync_server_url: String::new(),
            sync_token: String::new(),
            sync_shared_secret: String::new(),
            sync_device_name: String::new(),

            audio_retention_days: 7,
            text_retention_days: 30,

            dictionary_text: String::new(),

            remove_trailing_punctuation: true,
            short_sentence_threshold: 5,
            replacement_rules: Vec::new(),

            volc_access_key: String::new(),
            volc_secret_key: String::new(),
            volc_app_id: String::new(),
            online_hotword_id: String::new(),
            remote_hotword_updated_at: String::new(),
            local_hotword_updated_at: String::new(),

            enable_diagnostics: false,
        }
    }
}

/// Get current settings
#[tauri::command]
pub fn get_settings() -> Result<AppSettings, String> {
    crate::storage::get_settings().map_err(|e| e.to_string())
}

/// Save settings
#[tauri::command]
pub fn save_settings(
    mut settings: AppSettings,
    session: State<'_, SessionController>,
    sync: State<'_, SyncService>,
) -> Result<(), String> {
    let current_settings = crate::storage::get_settings().map_err(|e| e.to_string())?;

    let text_changed = settings.dictionary_text != current_settings.dictionary_text;
    if text_changed {
        settings.local_hotword_updated_at = chrono::Utc::now().to_rfc3339();
        log::info!(
            "Dictionary changed, updated local_hotword_updated_at to: {}",
            settings.local_hotword_updated_at
        );
    } else {
        // IMPORTANT: If text is same, preserve the timestamp from DB.
        settings.local_hotword_updated_at = current_settings.local_hotword_updated_at;
    }

    crate::storage::save_settings(&settings).map_err(|e| e.to_string())?;

    session.inner().apply_settings(
        settings.hold_threshold_ms,
        settings.max_recording_minutes,
        &settings.text_injection_mode,
        settings.input_device_uid.clone(),
        settings.remove_trailing_punctuation,
        settings.short_sentence_threshold,
        settings.replacement_rules.clone(),
        settings.translation_enabled,
        &settings.translation_trigger_mode,
        settings.double_tap_window_ms,
    );

    sync.apply_settings(&settings);

    Ok(())
}

#[tauri::command]
pub async fn probe_local_asr(
    command_path: Option<String>,
) -> Result<crate::asr::ColiAsrStatus, String> {
    Ok(crate::asr::probe_coli_status(command_path.as_deref()))
}
