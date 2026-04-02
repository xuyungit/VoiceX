//! ASR configuration

use serde::{Deserialize, Serialize};

/// ASR provider type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AsrProviderType {
    Volcengine,
    Google,
    Qwen,
    Gemini,
    GeminiLive,
    Cohere,
    OpenAI,
    Soniox,
    Coli,
}

impl Default for AsrProviderType {
    fn default() -> Self {
        Self::Volcengine
    }
}

impl AsrProviderType {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Volcengine => "Volcengine",
            Self::Google => "Google STT",
            Self::Qwen => "Qwen ASR",
            Self::Gemini => "Gemini",
            Self::GeminiLive => "Gemini Live",
            Self::Cohere => "Cohere",
            Self::OpenAI => "OpenAI Realtime",
            Self::Soniox => "Soniox",
            Self::Coli => "coli",
        }
    }
}

/// ASR service configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsrConfig {
    pub provider_type: AsrProviderType,

    // Volcengine settings
    pub ws_url: String,
    pub app_key: String,
    pub access_key: String,
    pub resource_id: String,

    // Google Cloud Speech-to-Text V2 settings
    pub google_api_key: String,
    pub google_project_id: String,
    pub google_language_code: String,
    pub google_location: String,
    pub google_endpointing: String,
    pub google_phrase_boost: f32,

    // Qwen Realtime ASR settings
    pub qwen_api_key: String,
    pub qwen_model: String,
    pub qwen_ws_url: String,
    pub qwen_language: String,

    // Gemini audio transcription settings
    pub gemini_api_key: String,
    pub gemini_model: String,
    pub gemini_live_model: String,
    pub gemini_language: String,

    // Cohere Audio Transcription settings
    pub cohere_api_key: String,
    pub cohere_model: String,
    pub cohere_language: String,

    // OpenAI Audio Transcription settings
    pub openai_asr_api_key: String,
    pub openai_asr_model: String,
    pub openai_asr_base_url: String,
    pub openai_asr_language: String,
    pub openai_asr_prompt: String,
    pub openai_asr_mode: String,

    // Soniox real-time ASR settings
    pub soniox_api_key: String,
    pub soniox_model: String,
    pub soniox_language: String,

    // Local ASR via `coli`
    pub coli_command_path: String,
    pub coli_use_vad: bool,
    pub coli_asr_interval_ms: u32,
    pub coli_final_refinement_mode: crate::asr::ColiRefinementMode,
    pub coli_realtime: bool,

    // Audio config
    pub chunk_ms: u32,

    // Recognition config (Volcengine-specific)
    pub enable_nonstream: bool,
    pub show_utterances: bool,
    pub enable_accelerate_text: bool,
    pub accelerate_score: Option<u8>,
    pub enable_itn: bool,
    pub enable_punc: bool,
    pub enable_ddc: bool,
    pub model_name: String,

    // VAD config
    pub end_window_size: Option<u32>,
    pub force_to_speech_time: Option<u32>,

    // Hotwords
    pub hotwords: Vec<String>,
    // Online hotword table ID (from Volcengine self-learning platform)
    pub online_hotword_id: Option<String>,
    // Context
    pub enable_context: bool,
    // Diagnostics
    pub enable_diagnostics: bool,
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            provider_type: AsrProviderType::default(),
            ws_url: "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async".to_string(),
            app_key: String::new(),
            access_key: String::new(),
            resource_id: "volc.seedasr.sauc.duration".to_string(),
            google_api_key: String::new(),
            google_project_id: String::new(),
            google_language_code: "cmn-Hans-CN, en-US".to_string(),
            google_location: "us".to_string(),
            google_endpointing: "supershort".to_string(),
            google_phrase_boost: 8.0,
            qwen_api_key: String::new(),
            qwen_model: "qwen3-asr-flash-realtime".to_string(),
            qwen_ws_url: "wss://dashscope.aliyuncs.com/api-ws/v1/realtime".to_string(),
            qwen_language: "zh".to_string(),
            gemini_api_key: String::new(),
            gemini_model: "gemini-3.1-flash-lite-preview".to_string(),
            gemini_live_model: "gemini-3.1-flash-live-preview".to_string(),
            gemini_language: "auto".to_string(),
            cohere_api_key: String::new(),
            cohere_model: "cohere-transcribe-03-2026".to_string(),
            cohere_language: "zh".to_string(),
            openai_asr_api_key: String::new(),
            openai_asr_model: "gpt-4o-transcribe".to_string(),
            openai_asr_base_url: "https://api.openai.com/v1".to_string(),
            openai_asr_language: String::new(),
            openai_asr_prompt: "Transcribe faithfully with natural punctuation and capitalization. Preserve the original wording and do not omit spoken content.".to_string(),
            openai_asr_mode: "batch".to_string(),
            soniox_api_key: String::new(),
            soniox_model: "stt-rt-v4".to_string(),
            soniox_language: String::new(),
            coli_command_path: String::new(),
            coli_use_vad: true,
            coli_asr_interval_ms: 1000,
            coli_final_refinement_mode: crate::asr::ColiRefinementMode::Off,
            coli_realtime: true,
            chunk_ms: 100,
            enable_nonstream: false,
            show_utterances: false,
            enable_accelerate_text: true,
            accelerate_score: Some(3),
            enable_itn: true,
            enable_punc: true,
            enable_ddc: true,
            model_name: "bigmodel".to_string(),
            end_window_size: Some(1400),
            force_to_speech_time: Some(3500),
            hotwords: Vec::new(),
            online_hotword_id: None,
            enable_context: false,
            enable_diagnostics: false,
        }
    }
}

impl From<&crate::commands::settings::AppSettings> for AsrConfig {
    fn from(settings: &crate::commands::settings::AppSettings) -> Self {
        let provider_type = match settings.asr_provider_type.as_str() {
            "google" => AsrProviderType::Google,
            "qwen" => AsrProviderType::Qwen,
            "gemini" => AsrProviderType::Gemini,
            "gemini-live" => AsrProviderType::GeminiLive,
            "cohere" => AsrProviderType::Cohere,
            "openai" => AsrProviderType::OpenAI,
            "soniox" => AsrProviderType::Soniox,
            "coli" => AsrProviderType::Coli,
            _ => AsrProviderType::Volcengine,
        };
        Self {
            provider_type,
            ws_url: settings.asr_ws_url.clone(),
            app_key: settings.asr_app_key.clone(),
            access_key: settings.asr_access_key.clone(),
            resource_id: settings.asr_resource_id.clone(),
            google_api_key: settings.google_stt_api_key.clone(),
            google_project_id: settings.google_stt_project_id.clone(),
            google_language_code: settings.google_stt_language_code.clone(),
            google_location: settings.google_stt_location.clone(),
            google_endpointing: settings.google_stt_endpointing.clone(),
            google_phrase_boost: settings.google_stt_phrase_boost,
            qwen_api_key: settings.qwen_asr_api_key.clone(),
            qwen_model: settings.qwen_asr_model.clone(),
            qwen_ws_url: settings.qwen_asr_ws_url.clone(),
            qwen_language: settings.qwen_asr_language.clone(),
            gemini_api_key: settings.gemini_api_key.clone(),
            gemini_model: settings.gemini_model.clone(),
            gemini_live_model: settings.gemini_live_model.clone(),
            gemini_language: settings.gemini_language.clone(),
            cohere_api_key: settings.cohere_api_key.clone(),
            cohere_model: settings.cohere_model.clone(),
            cohere_language: settings.cohere_language.clone(),
            openai_asr_api_key: settings.openai_asr_api_key.clone(),
            openai_asr_model: settings.openai_asr_model.clone(),
            openai_asr_base_url: settings.openai_asr_base_url.clone(),
            openai_asr_language: settings.openai_asr_language.clone(),
            openai_asr_prompt: settings.openai_asr_prompt.clone(),
            openai_asr_mode: settings.openai_asr_mode.clone(),
            soniox_api_key: settings.soniox_api_key.clone(),
            soniox_model: settings.soniox_model.clone(),
            soniox_language: settings.soniox_language.clone(),
            coli_command_path: settings.coli_command_path.clone(),
            coli_use_vad: settings.coli_use_vad,
            coli_asr_interval_ms: settings.coli_asr_interval_ms,
            coli_final_refinement_mode: crate::asr::ColiRefinementMode::from_str(
                &settings.coli_final_refinement_mode,
            ),
            coli_realtime: settings.coli_realtime,
            chunk_ms: 100,
            enable_nonstream: settings.enable_nonstream,
            show_utterances: false,
            enable_accelerate_text: true,
            accelerate_score: Some(3),
            enable_itn: true,
            enable_punc: true,
            model_name: "bigmodel".to_string(),
            end_window_size: settings.end_window_size,
            force_to_speech_time: settings.force_to_speech_time,
            enable_ddc: settings.enable_ddc,
            hotwords: {
                let mut seen = std::collections::HashSet::new();
                settings
                    .dictionary_text
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .filter(|l| seen.insert(l.clone()))
                    .collect()
            },
            online_hotword_id: if settings.online_hotword_id.is_empty() {
                None
            } else {
                Some(settings.online_hotword_id.clone())
            },
            enable_context: settings.enable_asr_context,
            enable_diagnostics: settings.enable_diagnostics,
        }
    }
}

impl AsrConfig {
    /// Returns true when the provider does not support streaming and requires
    /// the complete audio file before recognition can start.
    pub fn is_batch(&self) -> bool {
        match self.provider_type {
            AsrProviderType::Coli => !self.coli_realtime,
            AsrProviderType::Gemini => true,
            AsrProviderType::GeminiLive => false,
            AsrProviderType::Cohere => true,
            AsrProviderType::OpenAI => self.openai_asr_mode != "realtime",
            AsrProviderType::Soniox => false,
            _ => false,
        }
    }

    /// Check if the configuration is valid for the selected provider
    pub fn is_valid(&self) -> bool {
        match self.provider_type {
            AsrProviderType::Volcengine => {
                !self.app_key.is_empty()
                    && !self.access_key.is_empty()
                    && !self.resource_id.is_empty()
            }
            AsrProviderType::Google => {
                !self.google_project_id.is_empty() && !self.google_api_key.is_empty()
            }
            AsrProviderType::Qwen => {
                !self.qwen_api_key.is_empty()
                    && !self.qwen_model.is_empty()
                    && !self.qwen_ws_url.is_empty()
            }
            AsrProviderType::Gemini => {
                !self.gemini_api_key.is_empty() && !self.gemini_model.is_empty()
            }
            AsrProviderType::GeminiLive => {
                !self.gemini_api_key.is_empty() && !self.gemini_live_model.is_empty()
            }
            AsrProviderType::Cohere => {
                !self.cohere_api_key.is_empty()
                    && !self.cohere_model.is_empty()
                    && !self.cohere_language.is_empty()
            }
            AsrProviderType::OpenAI => {
                !self.openai_asr_api_key.is_empty()
                    && !self.openai_asr_model.is_empty()
                    && !self.openai_asr_base_url.is_empty()
            }
            AsrProviderType::Soniox => !self.soniox_api_key.is_empty(),
            AsrProviderType::Coli => {
                crate::asr::resolve_coli_command(&self.coli_command_path).is_some()
            }
        }
    }
}
