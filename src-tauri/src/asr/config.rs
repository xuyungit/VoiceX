//! ASR configuration

use serde::{Deserialize, Serialize};

/// ASR provider type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AsrProviderType {
    Volcengine,
    Google,
    FunAsr,
    Qwen,
    Gemini,
    GeminiLive,
    Cohere,
    OpenAI,
    ElevenLabs,
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
            Self::FunAsr => "Fun-ASR",
            Self::Qwen => "Qwen ASR",
            Self::Gemini => "Gemini",
            Self::GeminiLive => "Gemini Live",
            Self::Cohere => "Cohere",
            Self::OpenAI => "OpenAI Realtime",
            Self::ElevenLabs => "ElevenLabs",
            Self::Soniox => "Soniox",
            Self::Coli => "coli",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ElevenLabsRecognitionMode {
    Realtime,
    Batch,
}

impl Default for ElevenLabsRecognitionMode {
    fn default() -> Self {
        Self::Realtime
    }
}

impl ElevenLabsRecognitionMode {
    pub fn from_str(value: &str) -> Self {
        match value {
            "batch" => Self::Batch,
            _ => Self::Realtime,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Realtime => "realtime",
            Self::Batch => "batch",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QwenRecognitionMode {
    Realtime,
    Batch,
}

impl Default for QwenRecognitionMode {
    fn default() -> Self {
        Self::Realtime
    }
}

impl QwenRecognitionMode {
    pub fn from_str(value: &str) -> Self {
        match value {
            "batch" => Self::Batch,
            _ => Self::Realtime,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PostRecordingBatchRefineMode {
    Off,
    BatchRefine,
}

impl Default for PostRecordingBatchRefineMode {
    fn default() -> Self {
        Self::Off
    }
}

impl PostRecordingBatchRefineMode {
    pub fn from_str(value: &str) -> Self {
        match value {
            "batch_refine" => Self::BatchRefine,
            _ => Self::Off,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::BatchRefine => "batch_refine",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrPipelineMode {
    Realtime,
    RealtimeWithFinalPass,
    Batch,
}

impl AsrPipelineMode {
    pub fn is_batch(self) -> bool {
        matches!(self, Self::Batch)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsrProviderCapabilities {
    pub supports_realtime: bool,
    pub supports_realtime_with_final_pass: bool,
    pub supports_batch: bool,
    pub supports_post_recording_batch_refine: bool,
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

    // DashScope Fun-ASR realtime settings
    pub funasr_api_key: String,
    pub funasr_model: String,
    pub funasr_ws_url: String,
    pub funasr_language: String,

    // Qwen Realtime ASR settings
    pub qwen_api_key: String,
    pub qwen_recognition_mode: QwenRecognitionMode,
    pub qwen_model: String,
    pub qwen_batch_model: String,
    pub qwen_ws_url: String,
    pub qwen_language: String,
    pub qwen_post_recording_refine: bool,

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
    pub openai_asr_post_recording_refine: PostRecordingBatchRefineMode,

    // ElevenLabs Speech-to-Text settings
    pub elevenlabs_api_key: String,
    pub elevenlabs_recognition_mode: ElevenLabsRecognitionMode,
    pub elevenlabs_post_recording_refine: PostRecordingBatchRefineMode,
    pub elevenlabs_realtime_model: String,
    pub elevenlabs_batch_model: String,
    pub elevenlabs_language: String,
    pub elevenlabs_enable_keyterms: bool,

    // Soniox real-time ASR settings
    pub soniox_api_key: String,
    pub soniox_model: String,
    pub soniox_language: String,
    pub soniox_max_endpoint_delay_ms: Option<u32>,

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
            funasr_api_key: String::new(),
            funasr_model: "fun-asr-realtime".to_string(),
            funasr_ws_url: "wss://dashscope.aliyuncs.com/api-ws/v1/inference".to_string(),
            funasr_language: String::new(),
            qwen_api_key: String::new(),
            qwen_recognition_mode: QwenRecognitionMode::Realtime,
            qwen_model: "qwen3-asr-flash-realtime".to_string(),
            qwen_batch_model: "qwen3-asr-flash".to_string(),
            qwen_ws_url: "wss://dashscope.aliyuncs.com/api-ws/v1/realtime".to_string(),
            qwen_language: String::new(),
            qwen_post_recording_refine: false,
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
            openai_asr_post_recording_refine: PostRecordingBatchRefineMode::Off,
            elevenlabs_api_key: String::new(),
            elevenlabs_recognition_mode: ElevenLabsRecognitionMode::Realtime,
            elevenlabs_post_recording_refine: PostRecordingBatchRefineMode::Off,
            elevenlabs_realtime_model: "scribe_v2_realtime".to_string(),
            elevenlabs_batch_model: "scribe_v2".to_string(),
            elevenlabs_language: String::new(),
            elevenlabs_enable_keyterms: true,
            soniox_api_key: String::new(),
            soniox_model: "stt-rt-v4".to_string(),
            soniox_language: String::new(),
            soniox_max_endpoint_delay_ms: None,
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
            "funasr" => AsrProviderType::FunAsr,
            "qwen" => AsrProviderType::Qwen,
            "gemini" => AsrProviderType::Gemini,
            "gemini-live" => AsrProviderType::GeminiLive,
            "cohere" => AsrProviderType::Cohere,
            "openai" => AsrProviderType::OpenAI,
            "elevenlabs" => AsrProviderType::ElevenLabs,
            "soniox" => AsrProviderType::Soniox,
            "coli" => AsrProviderType::Coli,
            _ => AsrProviderType::Volcengine,
        };
        let mut config = Self {
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
            funasr_api_key: settings.funasr_api_key.clone(),
            funasr_model: settings.funasr_model.clone(),
            funasr_ws_url: settings.funasr_ws_url.clone(),
            funasr_language: settings.funasr_language.clone(),
            qwen_api_key: settings.qwen_asr_api_key.clone(),
            qwen_recognition_mode: QwenRecognitionMode::from_str(
                &settings.qwen_asr_recognition_mode,
            ),
            qwen_model: settings.qwen_asr_model.clone(),
            qwen_batch_model: settings.qwen_asr_batch_model.clone(),
            qwen_ws_url: settings.qwen_asr_ws_url.clone(),
            qwen_language: settings.qwen_asr_language.clone(),
            qwen_post_recording_refine: settings.qwen_asr_post_recording_refine,
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
            openai_asr_post_recording_refine: PostRecordingBatchRefineMode::from_str(
                &settings.openai_asr_post_recording_refine,
            ),
            elevenlabs_api_key: settings.elevenlabs_api_key.clone(),
            elevenlabs_recognition_mode: ElevenLabsRecognitionMode::from_str(
                &settings.elevenlabs_recognition_mode,
            ),
            elevenlabs_post_recording_refine: PostRecordingBatchRefineMode::from_str(
                &settings.elevenlabs_post_recording_refine,
            ),
            elevenlabs_realtime_model: settings.elevenlabs_realtime_model.clone(),
            elevenlabs_batch_model: settings.elevenlabs_batch_model.clone(),
            elevenlabs_language: settings.elevenlabs_language.clone(),
            elevenlabs_enable_keyterms: settings.elevenlabs_enable_keyterms,
            soniox_api_key: settings.soniox_api_key.clone(),
            soniox_model: settings.soniox_model.clone(),
            soniox_language: settings.soniox_language.clone(),
            soniox_max_endpoint_delay_ms: settings.soniox_max_endpoint_delay_ms,
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
        };
        config.normalize_provider_settings();
        config
    }
}

impl AsrConfig {
    fn normalize_provider_settings(&mut self) {
        if self.provider_type == AsrProviderType::Qwen
            && self.qwen_recognition_mode == QwenRecognitionMode::Batch
        {
            self.qwen_post_recording_refine = false;
        }

        if self.provider_type == AsrProviderType::ElevenLabs
            && self.elevenlabs_recognition_mode == ElevenLabsRecognitionMode::Batch
        {
            self.elevenlabs_post_recording_refine = PostRecordingBatchRefineMode::Off;
        }

        if self.provider_type == AsrProviderType::OpenAI && self.openai_asr_mode == "batch" {
            self.openai_asr_post_recording_refine = PostRecordingBatchRefineMode::Off;
        }
    }

    pub fn capabilities(&self) -> AsrProviderCapabilities {
        match self.provider_type {
            AsrProviderType::Volcengine => AsrProviderCapabilities {
                supports_realtime: true,
                supports_realtime_with_final_pass: true,
                supports_batch: false,
                supports_post_recording_batch_refine: false,
            },
            AsrProviderType::Google => AsrProviderCapabilities {
                supports_realtime: true,
                supports_realtime_with_final_pass: false,
                supports_batch: false,
                supports_post_recording_batch_refine: false,
            },
            AsrProviderType::FunAsr => AsrProviderCapabilities {
                supports_realtime: true,
                supports_realtime_with_final_pass: false,
                supports_batch: false,
                supports_post_recording_batch_refine: false,
            },
            AsrProviderType::Qwen => AsrProviderCapabilities {
                supports_realtime: true,
                supports_realtime_with_final_pass: true,
                supports_batch: true,
                supports_post_recording_batch_refine: true,
            },
            AsrProviderType::Gemini => AsrProviderCapabilities {
                supports_realtime: false,
                supports_realtime_with_final_pass: false,
                supports_batch: true,
                supports_post_recording_batch_refine: false,
            },
            AsrProviderType::GeminiLive => AsrProviderCapabilities {
                supports_realtime: true,
                supports_realtime_with_final_pass: false,
                supports_batch: false,
                supports_post_recording_batch_refine: false,
            },
            AsrProviderType::Cohere => AsrProviderCapabilities {
                supports_realtime: false,
                supports_realtime_with_final_pass: false,
                supports_batch: true,
                supports_post_recording_batch_refine: false,
            },
            AsrProviderType::OpenAI => AsrProviderCapabilities {
                supports_realtime: true,
                supports_realtime_with_final_pass: true,
                supports_batch: true,
                supports_post_recording_batch_refine: true,
            },
            AsrProviderType::ElevenLabs => AsrProviderCapabilities {
                supports_realtime: true,
                supports_realtime_with_final_pass: true,
                supports_batch: true,
                supports_post_recording_batch_refine: true,
            },
            AsrProviderType::Soniox => AsrProviderCapabilities {
                supports_realtime: true,
                supports_realtime_with_final_pass: false,
                supports_batch: false,
                supports_post_recording_batch_refine: false,
            },
            AsrProviderType::Coli => AsrProviderCapabilities {
                supports_realtime: true,
                supports_realtime_with_final_pass: true,
                supports_batch: true,
                supports_post_recording_batch_refine: false,
            },
        }
    }

    pub fn pipeline_mode(&self) -> AsrPipelineMode {
        match self.provider_type {
            AsrProviderType::Volcengine if self.ws_url.contains("bigmodel_nostream") => {
                AsrPipelineMode::Batch
            }
            AsrProviderType::Volcengine if self.enable_nonstream => {
                AsrPipelineMode::RealtimeWithFinalPass
            }
            AsrProviderType::Volcengine => AsrPipelineMode::Realtime,
            AsrProviderType::Google => AsrPipelineMode::Realtime,
            AsrProviderType::FunAsr => AsrPipelineMode::Realtime,
            AsrProviderType::Qwen if self.qwen_recognition_mode == QwenRecognitionMode::Batch => {
                AsrPipelineMode::Batch
            }
            AsrProviderType::Qwen if self.qwen_post_recording_refine => {
                AsrPipelineMode::RealtimeWithFinalPass
            }
            AsrProviderType::Qwen => AsrPipelineMode::Realtime,
            AsrProviderType::Gemini => AsrPipelineMode::Batch,
            AsrProviderType::GeminiLive => AsrPipelineMode::Realtime,
            AsrProviderType::Cohere => AsrPipelineMode::Batch,
            AsrProviderType::OpenAI if self.openai_asr_mode == "realtime" => {
                if self.openai_asr_post_recording_refine == PostRecordingBatchRefineMode::BatchRefine
                {
                    AsrPipelineMode::RealtimeWithFinalPass
                } else {
                    AsrPipelineMode::Realtime
                }
            }
            AsrProviderType::OpenAI => AsrPipelineMode::Batch,
            AsrProviderType::ElevenLabs
                if self.elevenlabs_recognition_mode == ElevenLabsRecognitionMode::Batch =>
            {
                AsrPipelineMode::Batch
            }
            AsrProviderType::ElevenLabs
                if self.elevenlabs_post_recording_refine
                    == PostRecordingBatchRefineMode::BatchRefine =>
            {
                AsrPipelineMode::RealtimeWithFinalPass
            }
            AsrProviderType::ElevenLabs => AsrPipelineMode::Realtime,
            AsrProviderType::Soniox => AsrPipelineMode::Realtime,
            AsrProviderType::Coli if !self.coli_realtime => AsrPipelineMode::Batch,
            AsrProviderType::Coli
                if self.coli_final_refinement_mode != crate::asr::ColiRefinementMode::Off =>
            {
                AsrPipelineMode::RealtimeWithFinalPass
            }
            AsrProviderType::Coli => AsrPipelineMode::Realtime,
        }
    }

    pub fn post_recording_batch_refine_enabled(&self) -> bool {
        self.capabilities().supports_post_recording_batch_refine
            && match self.provider_type {
                AsrProviderType::ElevenLabs => {
                    self.elevenlabs_recognition_mode == ElevenLabsRecognitionMode::Realtime
                        && self.elevenlabs_post_recording_refine
                            == PostRecordingBatchRefineMode::BatchRefine
                }
                AsrProviderType::Qwen => {
                    self.qwen_recognition_mode == QwenRecognitionMode::Realtime
                        && self.qwen_post_recording_refine
                }
                AsrProviderType::OpenAI => {
                    self.openai_asr_mode == "realtime"
                        && self.openai_asr_post_recording_refine
                            == PostRecordingBatchRefineMode::BatchRefine
                }
                _ => false,
            }
    }

    /// Returns true when the provider does not support streaming and requires
    /// the complete audio file before recognition can start.
    pub fn is_batch(&self) -> bool {
        self.pipeline_mode().is_batch()
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
            AsrProviderType::FunAsr => {
                !self.funasr_api_key.trim().is_empty()
                    && !self.funasr_model.trim().is_empty()
                    && !self.funasr_ws_url.trim().is_empty()
            }
            AsrProviderType::Qwen => {
                !self.qwen_api_key.is_empty()
                    && !self.qwen_ws_url.is_empty()
                    && match self.qwen_recognition_mode {
                        QwenRecognitionMode::Realtime => {
                            !self.qwen_model.trim().is_empty()
                                && (!self.post_recording_batch_refine_enabled()
                                    || !self.qwen_batch_model.trim().is_empty())
                        }
                        QwenRecognitionMode::Batch => !self.qwen_batch_model.trim().is_empty(),
                    }
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
            AsrProviderType::ElevenLabs => {
                !self.elevenlabs_api_key.trim().is_empty()
                    && match self.elevenlabs_recognition_mode {
                        ElevenLabsRecognitionMode::Realtime => {
                            !self.elevenlabs_realtime_model.trim().is_empty()
                                && (!self.post_recording_batch_refine_enabled()
                                    || !self.elevenlabs_batch_model.trim().is_empty())
                        }
                        ElevenLabsRecognitionMode::Batch => {
                            !self.elevenlabs_batch_model.trim().is_empty()
                        }
                    }
            }
            AsrProviderType::Soniox => !self.soniox_api_key.is_empty(),
            AsrProviderType::Coli => {
                crate::asr::resolve_coli_command(&self.coli_command_path).is_some()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AsrConfig, AsrPipelineMode, AsrProviderType, PostRecordingBatchRefineMode,
        ElevenLabsRecognitionMode, QwenRecognitionMode,
    };

    #[test]
    fn soniox_pipeline_mode_is_realtime_only() {
        let mut config = AsrConfig::default();
        config.provider_type = AsrProviderType::Soniox;

        assert_eq!(config.pipeline_mode(), AsrPipelineMode::Realtime);
        assert!(!config.capabilities().supports_batch);
        assert!(!config.capabilities().supports_realtime_with_final_pass);
    }

    #[test]
    fn funasr_pipeline_mode_is_realtime_only() {
        let mut config = AsrConfig::default();
        config.provider_type = AsrProviderType::FunAsr;

        assert_eq!(config.pipeline_mode(), AsrPipelineMode::Realtime);
        assert!(!config.capabilities().supports_batch);
        assert!(!config.capabilities().supports_realtime_with_final_pass);
    }

    #[test]
    fn qwen_refine_pipeline_mode_maps_to_realtime_with_final_pass() {
        let mut config = AsrConfig::default();
        config.provider_type = AsrProviderType::Qwen;
        config.qwen_recognition_mode = QwenRecognitionMode::Realtime;
        config.qwen_post_recording_refine = true;

        assert_eq!(
            config.pipeline_mode(),
            AsrPipelineMode::RealtimeWithFinalPass
        );
    }

    #[test]
    fn elevenlabs_batch_pipeline_mode_maps_to_batch() {
        let mut config = AsrConfig::default();
        config.provider_type = AsrProviderType::ElevenLabs;
        config.elevenlabs_recognition_mode = ElevenLabsRecognitionMode::Batch;
        config.elevenlabs_post_recording_refine = PostRecordingBatchRefineMode::BatchRefine;
        config.normalize_provider_settings();

        assert_eq!(config.pipeline_mode(), AsrPipelineMode::Batch);
        assert_eq!(
            config.elevenlabs_post_recording_refine,
            PostRecordingBatchRefineMode::Off
        );
    }

    #[test]
    fn openai_refine_pipeline_mode_maps_to_realtime_with_final_pass() {
        let mut config = AsrConfig::default();
        config.provider_type = AsrProviderType::OpenAI;
        config.openai_asr_mode = "realtime".to_string();
        config.openai_asr_post_recording_refine = PostRecordingBatchRefineMode::BatchRefine;

        assert_eq!(
            config.pipeline_mode(),
            AsrPipelineMode::RealtimeWithFinalPass
        );
        assert!(config.capabilities().supports_post_recording_batch_refine);
    }

    #[test]
    fn volcengine_native_two_pass_maps_to_realtime_with_final_pass() {
        let mut config = AsrConfig::default();
        config.provider_type = AsrProviderType::Volcengine;
        config.enable_nonstream = true;

        assert_eq!(
            config.pipeline_mode(),
            AsrPipelineMode::RealtimeWithFinalPass
        );
    }
}
