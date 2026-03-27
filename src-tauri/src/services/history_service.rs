use chrono::Utc;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

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
        let snapshot = match settings.asr_provider_type.as_str() {
            "google" => Some("Google / chirp_3".to_string()),
            "qwen" => {
                let model = settings.qwen_asr_model.trim();
                if model.is_empty() {
                    Some("Qwen".to_string())
                } else {
                    Some(format!("Qwen / {}", model))
                }
            }
            "gemini" => Self::format_provider_model("Gemini", &settings.gemini_model),
            "gemini-live" => {
                Self::format_provider_model("Gemini Live", &settings.gemini_live_model)
            }
            "cohere" => Self::format_provider_model("Cohere", &settings.cohere_model),
            "coli" if settings.coli_final_refinement_mode == "sensevoice" => {
                Some("Local / coli / stream + sensevoice refine".to_string())
            }
            "coli" if settings.coli_final_refinement_mode == "whisper" => {
                Some("Local / coli / stream + whisper refine".to_string())
            }
            "coli" => Some("Local / coli / sensevoice-small".to_string()),
            _ => Some("Volcengine / bigmodel".to_string()),
        };

        snapshot.and_then(Self::normalize_snapshot)
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

    fn format_provider_model(provider: &str, model: &str) -> Option<String> {
        let model = model.trim();
        if model.is_empty() {
            Some(provider.to_string())
        } else {
            Some(format!("{} / {}", provider, model))
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
