//! ASR (Automatic Speech Recognition) module
//!
//! Handles communication with ASR services (Volcengine, Google Cloud Speech-to-Text,
//! Qwen realtime ASR, and local `coli` offline ASR.

pub mod audio_utils;
mod client;
pub mod coli_client;
mod config;
pub mod google_client;
mod protocol;
pub mod qwen_client;
pub mod volc_auth;

pub use client::AsrClient;
pub use coli_client::{
    is_ffmpeg_available, probe_coli_status, resolve_coli_command, ColiAsrClient, ColiAsrStatus,
    ColiRefinementMode,
};
pub use config::{AsrConfig, AsrProviderType};
pub use google_client::GoogleSttClient;
pub use protocol::{AsrError, AsrEvent};
pub use qwen_client::QwenRealtimeClient;
