use chrono::Utc;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

use crate::asr::{AsrConfig, AsrPipelineMode, AsrProviderType, ColiRefinementMode};
use crate::commands::settings::AppSettings;
use crate::services::sync_service::SyncService;
use crate::storage;
use crate::storage::HistoryRecord;

#[derive(Clone, Default)]
pub struct HistoryService;

impl HistoryService {
    pub fn new() -> Self {
        Self
    }

    pub fn capture_model_snapshot() -> (Option<String>, Option<String>) {
        match storage::get_settings() {
            Ok(settings) => (
                Self::resolve_asr_model_name(&settings),
                Self::resolve_llm_model_name(&settings),
            ),
            Err(err) => {
                log::warn!("Failed to capture history model snapshot: {}", err);
                (None, None)
            }
        }
    }

    pub fn persist(
        &self,
        final_text: String,
        original_text: Option<String>,
        ai_correction_applied: bool,
        llm_invoked: bool,
        mode: String,
        duration_ms: Option<u64>,
        audio_path: Option<String>,
        asr_model_name: Option<String>,
        llm_model_name: Option<String>,
        app_handle: Option<AppHandle>,
    ) {
        let settings = match storage::get_settings() {
            Ok(s) => s,
            Err(e) => {
                log::warn!("History not persisted (failed to load settings): {}", e);
                return;
            }
        };

        let record = HistoryRecord {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            text: final_text,
            original_text,
            ai_correction_applied,
            llm_invoked,
            mode,
            duration_ms: duration_ms.unwrap_or(0).min(i64::MAX as u64) as i64,
            audio_path,
            is_final: true,
            error_code: 0,
            source_device_id: None,
            source_device_name: None,
            asr_model_name,
            llm_model_name,
        };

        let text_retention = if settings.sync_enabled {
            0
        } else {
            settings.text_retention_days
        };
        let audio_retention = settings.audio_retention_days;
        let handle_for_emit = app_handle;

        let update_stats = !settings.sync_enabled
            || settings.sync_server_url.trim().is_empty()
            || settings.sync_token.trim().is_empty()
            || settings.sync_shared_secret.trim().is_empty()
            || settings.sync_device_name.trim().is_empty();

        tauri::async_runtime::spawn_blocking(move || {
            let mut record = record;
            if record.source_device_id.is_none() {
                if let Ok(device_id) = storage::get_or_create_device_id() {
                    record.source_device_id = Some(device_id);
                }
            }

            let mut persisted = false;
            match storage::insert_history_record_with_stats(&record, update_stats) {
                Ok(inserted) => persisted = inserted,
                Err(err) => log::warn!("Failed to persist history: {}", err),
            }
            if !persisted {
                return;
            }

            if let Some(device_id) = record.source_device_id.as_ref() {
                let chars = record.text.chars().count() as i64;
                if let Err(err) = storage::increment_device_usage_stats(
                    device_id,
                    record.duration_ms,
                    chars,
                    record.llm_invoked,
                ) {
                    log::warn!("Failed to update device usage stats: {}", err);
                }
            }

            if let Some(app) = handle_for_emit.as_ref() {
                let sync = app.state::<SyncService>();
                sync.enqueue_history_upsert(&record);
            }

            if let Err(err) = storage::cleanup_history_retention(text_retention, audio_retention) {
                log::warn!("Failed to apply history retention: {}", err);
            }

            if let Some(app) = handle_for_emit {
                let _ = app.emit("history:updated", json!({ "id": record.id }));
            }
        });
    }

    pub fn get_recent_history(&self, limit: u32) -> Vec<String> {
        match storage::get_history(limit, 0) {
            Ok(records) => records.into_iter().map(|r| r.text).collect(),
            Err(e) => {
                log::warn!("Failed to fetch history for context: {}", e);
                Vec::new()
            }
        }
    }

    pub fn resolve_asr_model_name(settings: &AppSettings) -> Option<String> {
        let config = AsrConfig::from(settings);
        let snapshot = match config.provider_type {
            AsrProviderType::Google => Some("Google / chirp_3".to_string()),
            AsrProviderType::Volcengine => match config.pipeline_mode() {
                AsrPipelineMode::Batch => Some("Volcengine / bigmodel_nostream".to_string()),
                AsrPipelineMode::RealtimeWithFinalPass => {
                    Some("Volcengine / bigmodel_async + native two-pass".to_string())
                }
                AsrPipelineMode::Realtime => Some("Volcengine / bigmodel_async".to_string()),
            },
            AsrProviderType::Qwen => match config.pipeline_mode() {
                AsrPipelineMode::Batch => Self::qwen_batch_model_name(&config.qwen_batch_model),
                AsrPipelineMode::RealtimeWithFinalPass => {
                    Self::qwen_realtime_batch_refine_model_name(
                        &config.qwen_model,
                        &config.qwen_batch_model,
                    )
                }
                AsrPipelineMode::Realtime => {
                    Self::format_provider_model("Qwen", &config.qwen_model)
                }
            },
            AsrProviderType::Gemini => Self::format_provider_model("Gemini", &config.gemini_model),
            AsrProviderType::GeminiLive => {
                Self::format_provider_model("Gemini Live", &config.gemini_live_model)
            }
            AsrProviderType::Cohere => Self::format_provider_model("Cohere", &config.cohere_model),
            AsrProviderType::OpenAI => match config.pipeline_mode() {
                AsrPipelineMode::Realtime => {
                    Self::format_provider_model("OpenAI Realtime", &config.openai_asr_model)
                }
                AsrPipelineMode::RealtimeWithFinalPass => {
                    Self::openai_realtime_batch_refine_model_name(&config.openai_asr_model)
                }
                AsrPipelineMode::Batch => {
                    Self::format_provider_model("OpenAI", &config.openai_asr_model)
                }
            },
            AsrProviderType::ElevenLabs => match config.pipeline_mode() {
                AsrPipelineMode::Batch => {
                    Self::elevenlabs_batch_model_name(&config.elevenlabs_batch_model)
                }
                AsrPipelineMode::RealtimeWithFinalPass => {
                    Self::elevenlabs_realtime_batch_refine_model_name(
                        &config.elevenlabs_realtime_model,
                        &config.elevenlabs_batch_model,
                    )
                }
                AsrPipelineMode::Realtime => {
                    Self::elevenlabs_realtime_model_name(&config.elevenlabs_realtime_model)
                }
            },
            AsrProviderType::Soniox => Self::format_provider_model("Soniox", &config.soniox_model),
            AsrProviderType::Coli => match config.pipeline_mode() {
                AsrPipelineMode::Batch => Some("Local / coli / batch / sensevoice".to_string()),
                AsrPipelineMode::RealtimeWithFinalPass => match config.coli_final_refinement_mode {
                    ColiRefinementMode::SenseVoice => {
                        Some("Local / coli / stream + sensevoice refine".to_string())
                    }
                    ColiRefinementMode::Whisper => {
                        Some("Local / coli / stream + whisper refine".to_string())
                    }
                    ColiRefinementMode::Off => Some("Local / coli / sensevoice-small".to_string()),
                },
                AsrPipelineMode::Realtime => Some("Local / coli / sensevoice-small".to_string()),
            },
        };

        snapshot.and_then(Self::normalize_snapshot)
    }

    pub fn elevenlabs_realtime_model_name(model: &str) -> Option<String> {
        Some(format!(
            "ElevenLabs / {}",
            Self::normalize_model_or_default(model, "scribe_v2_realtime")
        ))
    }

    pub fn elevenlabs_batch_model_name(model: &str) -> Option<String> {
        Some(format!(
            "ElevenLabs / {}",
            Self::normalize_model_or_default(model, "scribe_v2")
        ))
    }

    pub fn elevenlabs_realtime_batch_refine_model_name(
        realtime_model: &str,
        batch_model: &str,
    ) -> Option<String> {
        Some(format!(
            "ElevenLabs / {} + batch refine({})",
            Self::normalize_model_or_default(realtime_model, "scribe_v2_realtime"),
            Self::normalize_model_or_default(batch_model, "scribe_v2")
        ))
    }

    pub fn qwen_realtime_batch_refine_model_name(
        realtime_model: &str,
        batch_model: &str,
    ) -> Option<String> {
        Some(format!(
            "Qwen / {} + batch refine({})",
            Self::normalize_model_or_default(realtime_model, "qwen3-asr-flash-realtime"),
            Self::normalize_model_or_default(batch_model, "qwen3-asr-flash")
        ))
    }

    pub fn qwen_batch_model_name(model: &str) -> Option<String> {
        Some(format!(
            "Qwen / {}",
            Self::normalize_model_or_default(model, "qwen3-asr-flash")
        ))
    }

    pub fn openai_realtime_batch_refine_model_name(model: &str) -> Option<String> {
        let model = Self::normalize_model_or_default(model, "gpt-4o-transcribe");
        Some(format!("OpenAI / {} + batch refine({})", model, model))
    }

    pub fn resolve_llm_model_name(settings: &AppSettings) -> Option<String> {
        let snapshot = match settings.llm_provider_type.as_str() {
            "openai" => Self::format_provider_model("OpenAI", &settings.llm_openai_model),
            "qwen" => Self::format_provider_model("Qwen", &settings.llm_qwen_model),
            "custom" => Self::format_provider_model("Custom", &settings.llm_custom_model),
            _ => Self::format_provider_model("Volcengine", &settings.llm_volcengine_model),
        };

        snapshot.and_then(Self::normalize_snapshot)
    }

    pub fn llm_model_for_record(llm_invoked: bool, snapshot: Option<String>) -> Option<String> {
        if llm_invoked {
            snapshot.and_then(Self::normalize_snapshot)
        } else {
            None
        }
    }

    pub fn format_provider_model(provider: &str, model: &str) -> Option<String> {
        let model = model.trim();
        if model.is_empty() {
            Some(provider.to_string())
        } else {
            Some(format!("{} / {}", provider, model))
        }
    }

    fn normalize_model_or_default(model: &str, default_model: &str) -> String {
        let trimmed = model.trim();
        if trimmed.is_empty() {
            default_model.to_string()
        } else {
            trimmed.to_string()
        }
    }

    fn normalize_snapshot(value: String) -> Option<String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HistoryService;
    use crate::commands::settings::AppSettings;

    #[test]
    fn elevenlabs_refine_snapshot_uses_defaults_when_models_are_empty() {
        assert_eq!(
            HistoryService::elevenlabs_realtime_batch_refine_model_name(" ", ""),
            Some("ElevenLabs / scribe_v2_realtime + batch refine(scribe_v2)".to_string())
        );
    }

    #[test]
    fn elevenlabs_realtime_snapshot_uses_default_model() {
        assert_eq!(
            HistoryService::elevenlabs_realtime_model_name(" "),
            Some("ElevenLabs / scribe_v2_realtime".to_string())
        );
    }

    #[test]
    fn resolve_asr_model_name_for_elevenlabs_refine_uses_shared_helper() {
        let mut settings = AppSettings::default();
        settings.asr_provider_type = "elevenlabs".to_string();
        settings.elevenlabs_recognition_mode = "realtime".to_string();
        settings.elevenlabs_post_recording_refine = "batch_refine".to_string();
        settings.elevenlabs_realtime_model = "scribe_v2_realtime".to_string();
        settings.elevenlabs_batch_model = "scribe_v2".to_string();

        assert_eq!(
            HistoryService::resolve_asr_model_name(&settings).as_deref(),
            Some("ElevenLabs / scribe_v2_realtime + batch refine(scribe_v2)")
        );
    }

    #[test]
    fn resolve_asr_model_name_for_qwen_refine_uses_shared_helper() {
        let mut settings = AppSettings::default();
        settings.asr_provider_type = "qwen".to_string();
        settings.qwen_asr_recognition_mode = "realtime".to_string();
        settings.qwen_asr_post_recording_refine = true;
        settings.qwen_asr_model = "qwen3-asr-flash-realtime".to_string();
        settings.qwen_asr_batch_model = "qwen3-asr-flash".to_string();

        assert_eq!(
            HistoryService::resolve_asr_model_name(&settings).as_deref(),
            Some("Qwen / qwen3-asr-flash-realtime + batch refine(qwen3-asr-flash)")
        );
    }

    #[test]
    fn resolve_asr_model_name_for_openai_refine_uses_shared_helper() {
        let mut settings = AppSettings::default();
        settings.asr_provider_type = "openai".to_string();
        settings.openai_asr_mode = "realtime".to_string();
        settings.openai_asr_post_recording_refine = "batch_refine".to_string();
        settings.openai_asr_model = "gpt-4o-transcribe".to_string();

        assert_eq!(
            HistoryService::resolve_asr_model_name(&settings).as_deref(),
            Some("OpenAI / gpt-4o-transcribe + batch refine(gpt-4o-transcribe)")
        );
    }

    #[test]
    fn resolve_asr_model_name_for_qwen_batch_uses_batch_model() {
        let mut settings = AppSettings::default();
        settings.asr_provider_type = "qwen".to_string();
        settings.qwen_asr_recognition_mode = "batch".to_string();
        settings.qwen_asr_batch_model = "qwen3-asr-flash".to_string();

        assert_eq!(
            HistoryService::resolve_asr_model_name(&settings).as_deref(),
            Some("Qwen / qwen3-asr-flash")
        );
    }

    #[test]
    fn resolve_asr_model_name_for_volcengine_native_two_pass_reflects_mode() {
        let mut settings = AppSettings::default();
        settings.asr_provider_type = "volcengine".to_string();
        settings.asr_ws_url =
            "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async".to_string();
        settings.enable_nonstream = true;

        assert_eq!(
            HistoryService::resolve_asr_model_name(&settings).as_deref(),
            Some("Volcengine / bigmodel_async + native two-pass")
        );
    }

    #[test]
    fn resolve_asr_model_name_for_volcengine_nostream_reflects_mode() {
        let mut settings = AppSettings::default();
        settings.asr_provider_type = "volcengine".to_string();
        settings.asr_ws_url =
            "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_nostream".to_string();
        settings.enable_nonstream = false;

        assert_eq!(
            HistoryService::resolve_asr_model_name(&settings).as_deref(),
            Some("Volcengine / bigmodel_nostream")
        );
    }

    #[test]
    fn resolve_asr_model_name_for_soniox_stays_realtime_only() {
        let mut settings = AppSettings::default();
        settings.asr_provider_type = "soniox".to_string();
        settings.soniox_model = "stt-rt-v4".to_string();

        assert_eq!(
            HistoryService::resolve_asr_model_name(&settings).as_deref(),
            Some("Soniox / stt-rt-v4")
        );
    }
}
