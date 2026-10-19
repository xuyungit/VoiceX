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
    StepAudio,
    Mimo,
    Coli,
    QwenLocal,
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
            Self::OpenAI => "OpenAI",
            Self::ElevenLabs => "ElevenLabs",
            Self::Soniox => "Soniox",
            Self::StepAudio => "StepAudio",
            Self::Mimo => "MiMo",
            Self::Coli => "coli",
            Self::QwenLocal => "Qwen3-ASR (local)",
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
    pub qwen_workspace_id: String,
    pub qwen_language: String,
    pub qwen_post_recording_refine: bool,
    pub qwen_vocabulary_id: String,
    pub qwen_hotword_weight: u32,
    pub qwen_semantic_punctuation_enabled: bool,
    pub qwen_max_sentence_silence_ms: u32,
    pub qwen_heartbeat: bool,

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
    pub openai_asr_refine_model: String,
    pub openai_asr_base_url: String,
    pub openai_asr_language: String,
    pub openai_asr_prompt: String,
    pub openai_asr_mode: String,
    pub openai_asr_delay: String,
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

    // StepAudio ASR settings (HTTP + SSE batch transcription)
    pub stepaudio_api_key: String,
    pub stepaudio_model: String,
    pub stepaudio_base_url: String,
    pub stepaudio_language: String,

    // Xiaomi MiMo ASR settings
    pub mimo_api_key: String,
    pub mimo_model: String,
    pub mimo_base_url: String,
    pub mimo_language: String,

    // Local ASR via the `qwen-asr` CLI (Qwen3-ASR)
    pub qwen_local_command_path: String,
    pub qwen_local_model_dir: String,
    pub qwen_local_language: String,
    pub qwen_local_use_dictionary: bool,

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
            funasr_model: crate::asr::models::default_model("funasr"),
            funasr_ws_url: "wss://dashscope.aliyuncs.com/api-ws/v1/inference".to_string(),
            funasr_language: String::new(),
            qwen_api_key: String::new(),
            qwen_recognition_mode: QwenRecognitionMode::Realtime,
            qwen_model: crate::asr::models::default_model("qwen"),
            qwen_batch_model: crate::asr::models::default_model("qwenBatch"),
            qwen_ws_url: "wss://dashscope.aliyuncs.com/api-ws/v1/realtime".to_string(),
            qwen_workspace_id: String::new(),
            qwen_language: String::new(),
            qwen_post_recording_refine: false,
            qwen_vocabulary_id: String::new(),
            qwen_hotword_weight: 4,
            qwen_semantic_punctuation_enabled: false,
            qwen_max_sentence_silence_ms: 1300,
            qwen_heartbeat: false,
            gemini_api_key: String::new(),
            gemini_model: crate::asr::models::default_model("gemini"),
            gemini_live_model: crate::asr::models::default_model("geminiLive"),
            gemini_language: "auto".to_string(),
            cohere_api_key: String::new(),
            cohere_model: crate::asr::models::default_model("cohere"),
            cohere_language: "zh".to_string(),
            openai_asr_api_key: String::new(),
            openai_asr_model: crate::asr::models::default_model("openai"),
            openai_asr_refine_model: crate::asr::models::default_model("openaiRefine"),
            openai_asr_base_url: "https://api.openai.com/v1".to_string(),
            openai_asr_language: String::new(),
            openai_asr_prompt: "Transcribe faithfully with natural punctuation and capitalization. Preserve the original wording and do not omit spoken content.".to_string(),
            openai_asr_mode: "batch".to_string(),
            openai_asr_delay: String::new(),
            openai_asr_post_recording_refine: PostRecordingBatchRefineMode::Off,
            elevenlabs_api_key: String::new(),
            elevenlabs_recognition_mode: ElevenLabsRecognitionMode::Realtime,
            elevenlabs_post_recording_refine: PostRecordingBatchRefineMode::Off,
            elevenlabs_realtime_model: crate::asr::models::default_model("elevenlabsRealtime"),
            elevenlabs_batch_model: crate::asr::models::default_model("elevenlabsBatch"),
            elevenlabs_language: String::new(),
            elevenlabs_enable_keyterms: true,
            soniox_api_key: String::new(),
            soniox_model: crate::asr::models::default_model("soniox"),
            soniox_language: String::new(),
            soniox_max_endpoint_delay_ms: None,
            stepaudio_api_key: String::new(),
            stepaudio_model: crate::asr::models::default_model("stepaudio"),
            stepaudio_base_url: "https://api.stepfun.com/v1".to_string(),
            stepaudio_language: "auto".to_string(),
            mimo_api_key: String::new(),
            mimo_model: crate::asr::models::default_model("mimo"),
            mimo_base_url: "https://api.xiaomimimo.com/v1".to_string(),
            mimo_language: "auto".to_string(),
            qwen_local_command_path: String::new(),
            qwen_local_model_dir: String::new(),
            qwen_local_language: "Chinese".to_string(),
            qwen_local_use_dictionary: true,
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
            "stepaudio" => AsrProviderType::StepAudio,
            "mimo" => AsrProviderType::Mimo,
            "coli" => AsrProviderType::Coli,
            "qwen-local" => AsrProviderType::QwenLocal,
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
            qwen_workspace_id: settings.qwen_asr_workspace_id.clone(),
            qwen_language: settings.qwen_asr_language.clone(),
            qwen_post_recording_refine: settings.qwen_asr_post_recording_refine,
            qwen_vocabulary_id: settings.qwen_asr_vocabulary_id.clone(),
            qwen_hotword_weight: settings.qwen_asr_hotword_weight,
            qwen_semantic_punctuation_enabled: settings.qwen_asr_semantic_punctuation_enabled,
            qwen_max_sentence_silence_ms: settings.qwen_asr_max_sentence_silence_ms,
            qwen_heartbeat: settings.qwen_asr_heartbeat,
            gemini_api_key: settings.gemini_api_key.clone(),
            gemini_model: settings.gemini_model.clone(),
            gemini_live_model: settings.gemini_live_model.clone(),
            gemini_language: settings.gemini_language.clone(),
            cohere_api_key: settings.cohere_api_key.clone(),
            cohere_model: settings.cohere_model.clone(),
            cohere_language: settings.cohere_language.clone(),
            openai_asr_api_key: settings.openai_asr_api_key.clone(),
            openai_asr_model: settings.openai_asr_model.clone(),
            openai_asr_refine_model: settings.openai_asr_refine_model.clone(),
            openai_asr_delay: settings.openai_asr_delay.clone(),
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
            stepaudio_api_key: settings.stepaudio_api_key.clone(),
            stepaudio_model: settings.stepaudio_model.clone(),
            stepaudio_base_url: settings.stepaudio_base_url.clone(),
            stepaudio_language: settings.stepaudio_language.clone(),
            mimo_api_key: settings.mimo_api_key.clone(),
            mimo_model: settings.mimo_model.clone(),
            mimo_base_url: settings.mimo_base_url.clone(),
            mimo_language: settings.mimo_language.clone(),
            qwen_local_command_path: settings.qwen_local_command_path.clone(),
            qwen_local_model_dir: settings.qwen_local_model_dir.clone(),
            qwen_local_language: settings.qwen_local_language.clone(),
            qwen_local_use_dictionary: settings.qwen_local_use_dictionary,
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
            AsrProviderType::StepAudio => AsrProviderCapabilities {
                supports_realtime: false,
                supports_realtime_with_final_pass: false,
                supports_batch: true,
                supports_post_recording_batch_refine: false,
            },
            AsrProviderType::Mimo => AsrProviderCapabilities {
                supports_realtime: false,
                supports_realtime_with_final_pass: false,
                supports_batch: true,
                supports_post_recording_batch_refine: false,
            },
            AsrProviderType::Coli => AsrProviderCapabilities {
                supports_realtime: true,
                supports_realtime_with_final_pass: true,
                supports_batch: true,
                supports_post_recording_batch_refine: false,
            },
            // The `qwen-asr` CLI emits only a final line, so realtime is not
            // reachable through this transport.
            AsrProviderType::QwenLocal => AsrProviderCapabilities {
                supports_realtime: false,
                supports_realtime_with_final_pass: false,
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
                if self.openai_asr_post_recording_refine
                    == PostRecordingBatchRefineMode::BatchRefine
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
            AsrProviderType::StepAudio => AsrPipelineMode::Batch,
            AsrProviderType::Mimo => AsrPipelineMode::Batch,
            AsrProviderType::QwenLocal => AsrPipelineMode::Batch,
            AsrProviderType::Coli if !self.coli_realtime => AsrPipelineMode::Batch,
            AsrProviderType::Coli
                if self.coli_final_refinement_mode != crate::asr::ColiRefinementMode::Off =>
            {
                AsrPipelineMode::RealtimeWithFinalPass
            }
            AsrProviderType::Coli => AsrPipelineMode::Realtime,
        }
    }

    pub fn max_recording_minutes_limit(&self) -> Option<u32> {
        match (self.provider_type, self.pipeline_mode()) {
            (AsrProviderType::Qwen, AsrPipelineMode::Batch)
            | (AsrProviderType::Qwen, AsrPipelineMode::RealtimeWithFinalPass) => Some(5),
            (AsrProviderType::StepAudio, AsrPipelineMode::Batch) => Some(30),
            (AsrProviderType::GeminiLive, _) => {
                super::models::max_recording_minutes("gemini-live", &self.gemini_live_model)
            }
            _ => None,
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

    /// Validate required fields and the selected model, retaining separate diagnostics
    /// and user-facing explanations. Neither includes credential values.
    pub fn validate_for_recognition(
        &self,
        phase: super::AsrPhase,
    ) -> Result<(), super::AsrFailure> {
        let missing = self.missing_configuration_fields();
        if !missing.is_empty() {
            let fields = missing
                .iter()
                .map(|(field, _)| *field)
                .collect::<Vec<_>>()
                .join(", ");
            let labels = missing
                .iter()
                .map(|(_, label)| *label)
                .collect::<Vec<_>>()
                .join("、");
            let mut failure = super::AsrFailure::configuration(
                self.provider_type,
                phase,
                format!("Missing or unavailable ASR configuration fields: {fields}"),
            );
            failure.display_message = format!(
                "{} 配置不完整，请检查{}。",
                self.provider_type.display_name(),
                labels
            );
            return Err(failure);
        }
        self.validate_model_selection_details().map_err(|error| {
            let mut failure = super::AsrFailure::configuration(
                self.provider_type,
                phase,
                error.technical_message,
            );
            failure.display_message = format!(
                "{} 配置无效：{}",
                self.provider_type.display_name(),
                error.display_message
            );
            failure
        })
    }

    /// Check required fields for the selected provider; catalogue checks are separate.
    pub fn is_valid(&self) -> bool {
        self.missing_configuration_fields().is_empty()
    }

    /// Returns field names for logs and labels for UI, never the configured values.
    fn missing_configuration_fields(&self) -> Vec<(&'static str, &'static str)> {
        use AsrProviderType::*;
        let mut missing = Vec::new();
        let mut require = |field, label, value: &str| {
            if value.trim().is_empty() {
                missing.push((field, label));
            }
        };
        match self.provider_type {
            Volcengine => {
                require("app_key", "App Key", &self.app_key);
                require("access_key", "Access Key", &self.access_key);
                require("resource_id", "资源 ID", &self.resource_id);
            }
            Google => {
                require("google_project_id", "项目 ID", &self.google_project_id);
                require("google_api_key", "服务账号密钥", &self.google_api_key);
            }
            FunAsr => {
                require("funasr_api_key", "API Key", &self.funasr_api_key);
                require("funasr_model", "识别模型", &self.funasr_model);
                require("funasr_ws_url", "服务地址", &self.funasr_ws_url);
            }
            Qwen => {
                require("qwen_api_key", "API Key", &self.qwen_api_key);
                require("qwen_ws_url", "服务地址", &self.qwen_ws_url);
                if !self.is_batch() {
                    require("qwen_model", "实时模型", &self.qwen_model);
                }
                if self.is_batch() || self.post_recording_batch_refine_enabled() {
                    require("qwen_batch_model", "批量识别模型", &self.qwen_batch_model);
                }
            }
            Gemini | GeminiLive => {
                require("gemini_api_key", "API Key", &self.gemini_api_key);
                if self.provider_type == Gemini {
                    require("gemini_model", "识别模型", &self.gemini_model);
                } else {
                    require("gemini_live_model", "实时模型", &self.gemini_live_model);
                }
            }
            Cohere => {
                require("cohere_api_key", "API Key", &self.cohere_api_key);
                require("cohere_model", "识别模型", &self.cohere_model);
                require("cohere_language", "识别语言", &self.cohere_language);
            }
            OpenAI => {
                require("openai_asr_api_key", "API Key", &self.openai_asr_api_key);
                require("openai_asr_model", "识别模型", &self.openai_asr_model);
                require("openai_asr_base_url", "服务地址", &self.openai_asr_base_url);
                if self.post_recording_batch_refine_enabled() {
                    require(
                        "openai_asr_refine_model",
                        "精修模型",
                        &self.openai_asr_refine_model,
                    );
                }
            }
            ElevenLabs => {
                require("elevenlabs_api_key", "API Key", &self.elevenlabs_api_key);
                if !self.is_batch() {
                    require(
                        "elevenlabs_realtime_model",
                        "实时模型",
                        &self.elevenlabs_realtime_model,
                    );
                }
                if self.is_batch() || self.post_recording_batch_refine_enabled() {
                    require(
                        "elevenlabs_batch_model",
                        "批量识别模型",
                        &self.elevenlabs_batch_model,
                    );
                }
            }
            Soniox => {
                require("soniox_api_key", "API Key", &self.soniox_api_key);
            }
            StepAudio => {
                require("stepaudio_api_key", "API Key", &self.stepaudio_api_key);
                require("stepaudio_model", "识别模型", &self.stepaudio_model);
                require("stepaudio_base_url", "服务地址", &self.stepaudio_base_url);
            }
            Mimo => {
                require("mimo_api_key", "API Key", &self.mimo_api_key);
                require("mimo_model", "识别模型", &self.mimo_model);
                require("mimo_base_url", "服务地址", &self.mimo_base_url);
            }
            QwenLocal => {
                require(
                    "qwen_local_model_dir",
                    "本地模型目录",
                    &self.qwen_local_model_dir,
                );
            }
            Coli => {
                if crate::asr::resolve_coli_command(&self.coli_command_path).is_none() {
                    missing.push(("coli_command_path", "本地识别程序路径"));
                }
            }
        }
        missing
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn validation_names_missing_fields_without_exposing_values() {
        let mut config = super::AsrConfig::default();
        config.provider_type = super::AsrProviderType::Qwen;
        config.qwen_api_key = "private-test-key".into();
        config.qwen_recognition_mode = super::QwenRecognitionMode::Batch;
        config.qwen_batch_model = " ".into();
        let error = config
            .validate_for_recognition(crate::asr::AsrPhase::Finalizing)
            .unwrap_err();
        assert_eq!(error.kind, crate::asr::AsrFailureKind::Config);
        assert!(error.technical_message.contains("qwen_batch_model"));
        assert!(error.display_message.contains("批量识别模型"));
        assert!(!error.technical_message.contains("private-test-key"));
        assert!(!error.display_message.contains("private-test-key"));
        assert!(!config.is_valid());
    }

    #[test]
    fn model_validation_preserves_model_and_mode_in_user_message() {
        let mut config = super::AsrConfig::default();
        config.provider_type = super::AsrProviderType::Qwen;
        config.qwen_api_key = "private-test-key".into();
        config.qwen_recognition_mode = super::QwenRecognitionMode::Batch;
        config.qwen_batch_model = "qwen3-asr-flash-realtime".into();
        let error = config
            .validate_for_recognition(crate::asr::AsrPhase::Finalizing)
            .unwrap_err();
        for message in [&error.technical_message, &error.display_message] {
            assert!(message.contains("qwen3-asr-flash-realtime"));
            assert!(message.contains("batch"));
        }
        assert!(!error.retryable);
    }

    use super::{
        AsrConfig, AsrPipelineMode, AsrProviderType, ElevenLabsRecognitionMode,
        PostRecordingBatchRefineMode, QwenRecognitionMode,
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
    fn stepaudio_pipeline_mode_is_batch_with_thirty_minute_cap() {
        let mut config = AsrConfig::default();
        config.provider_type = AsrProviderType::StepAudio;

        assert_eq!(config.pipeline_mode(), AsrPipelineMode::Batch);
        assert!(config.capabilities().supports_batch);
        assert!(!config.capabilities().supports_realtime);
        assert_eq!(config.max_recording_minutes_limit(), Some(30));
    }

    #[test]
    fn mimo_pipeline_mode_is_batch_only() {
        let mut config = AsrConfig::default();
        config.provider_type = AsrProviderType::Mimo;

        assert_eq!(config.pipeline_mode(), AsrPipelineMode::Batch);
        assert!(config.capabilities().supports_batch);
        assert!(!config.capabilities().supports_realtime);
        assert!(!config.capabilities().supports_realtime_with_final_pass);
        assert_eq!(config.max_recording_minutes_limit(), None);
    }

    #[test]
    fn mimo_requires_api_key_model_and_base_url() {
        let mut config = AsrConfig::default();
        config.provider_type = AsrProviderType::Mimo;
        assert!(!config.is_valid());

        config.mimo_api_key = "key".to_string();
        assert!(config.is_valid());
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
    fn qwen_batch_has_five_minute_recording_cap() {
        let mut config = AsrConfig::default();
        config.provider_type = AsrProviderType::Qwen;
        config.qwen_recognition_mode = QwenRecognitionMode::Batch;

        assert_eq!(config.max_recording_minutes_limit(), Some(5));
    }

    #[test]
    fn gemini_transcribe_live_uses_its_documented_ten_minute_cap() {
        let mut config = AsrConfig::default();
        config.provider_type = AsrProviderType::GeminiLive;
        assert_eq!(config.max_recording_minutes_limit(), Some(10));
        config.gemini_live_model = "gemini-3.1-flash-live-preview".into();
        assert_eq!(config.max_recording_minutes_limit(), None);
        config.gemini_live_model = "my-custom-model".into();
        assert_eq!(config.max_recording_minutes_limit(), None);
    }

    #[test]
    fn qwen_realtime_with_refine_has_five_minute_recording_cap() {
        let mut config = AsrConfig::default();
        config.provider_type = AsrProviderType::Qwen;
        config.qwen_recognition_mode = QwenRecognitionMode::Realtime;
        config.qwen_post_recording_refine = true;

        assert_eq!(config.max_recording_minutes_limit(), Some(5));
    }

    #[test]
    fn qwen_pure_realtime_has_no_recording_cap() {
        let mut config = AsrConfig::default();
        config.provider_type = AsrProviderType::Qwen;
        config.qwen_recognition_mode = QwenRecognitionMode::Realtime;
        config.qwen_post_recording_refine = false;

        assert_eq!(config.max_recording_minutes_limit(), None);
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
