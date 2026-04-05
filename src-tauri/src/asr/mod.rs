//! ASR (Automatic Speech Recognition) module
//!
//! Handles communication with ASR services (Volcengine, Google Cloud Speech-to-Text,
//! Qwen realtime ASR, Gemini/Cohere/OpenAI/ElevenLabs audio transcription, and local `coli`
//! offline ASR.

pub mod audio_utils;
mod client;
pub mod cohere_client;
pub mod coli_client;
mod config;
pub mod elevenlabs_client;
pub mod elevenlabs_realtime_client;
pub mod gemini_client;
pub mod gemini_live_client;
pub mod google_client;
pub mod ogg_decoder;
pub mod openai_client;
pub mod openai_realtime_client;
mod protocol;
pub mod qwen_client;
pub mod qwen_transcription_client;
pub mod soniox_client;
mod transcription;
pub mod volc_auth;

pub use client::AsrClient;
pub use cohere_client::CohereTranscriptionClient;
pub use coli_client::{
    is_ffmpeg_available, probe_coli_status, resolve_coli_command, ColiAsrClient, ColiAsrStatus,
    ColiRefinementMode,
};
pub use config::{
    AsrConfig, AsrPipelineMode, AsrProviderCapabilities, AsrProviderType,
    ElevenLabsRecognitionMode, PostRecordingBatchRefineMode, QwenRecognitionMode,
};
pub use elevenlabs_client::ElevenLabsTranscriptionClient;
pub use elevenlabs_realtime_client::ElevenLabsRealtimeClient;
pub use gemini_client::GeminiTranscriptionClient;
pub use gemini_live_client::GeminiLiveClient;
pub use google_client::GoogleSttClient;
pub use openai_client::OpenAITranscriptionClient;
pub use openai_realtime_client::OpenAIRealtimeClient;
pub use protocol::{AsrError, AsrEvent, AsrFailure, AsrFailureKind, AsrPhase};
pub use qwen_client::QwenRealtimeClient;
pub use qwen_transcription_client::QwenTranscriptionClient;
pub use soniox_client::SonioxClient;
pub use transcription::{
    transcribe_audio_path, transcribe_audio_path_detailed, AsrTranscriptionOutcome,
};
