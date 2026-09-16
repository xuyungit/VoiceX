//! History-related commands

use crate::services::history_service::HISTORY_MODE_TRANSLATE_READ;
use crate::services::sync_service::SyncService;
use crate::storage::{HistoryRecord, UsageStats};
use tauri::State;

/// Get history records
#[tauri::command]
pub fn get_history(limit: u32, offset: u32) -> Result<Vec<HistoryRecord>, String> {
    if let Ok(settings) = crate::storage::get_settings() {
        let text_retention = if settings.sync_enabled {
            0
        } else {
            settings.text_retention_days
        };
        if let Err(err) =
            crate::storage::cleanup_history_retention(text_retention, settings.audio_retention_days)
        {
            log::warn!("History retention cleanup failed: {}", err);
        }
    }

    crate::storage::get_history(limit, offset).map_err(|e| e.to_string())
}

/// Delete a history record
#[tauri::command]
pub fn delete_history_record(id: String, sync: State<'_, SyncService>) -> Result<(), String> {
    // Translate-and-read rows are never uploaded (history_service), so there
    // is nothing on the server to delete and no event to send.
    let mode = crate::storage::history_record_mode(&id).map_err(|e| e.to_string())?;
    crate::storage::delete_history_record(&id).map_err(|e| e.to_string())?;
    if mode.as_deref() != Some(HISTORY_MODE_TRANSLATE_READ) {
        sync.enqueue_history_delete(&id);
    }
    Ok(())
}

/// Get usage statistics
#[tauri::command]
pub fn get_usage_stats() -> Result<UsageStats, String> {
    crate::storage::get_usage_stats().map_err(|e| e.to_string())
}

/// Get usage statistics for the current device.
#[tauri::command]
pub fn get_local_usage_stats() -> Result<UsageStats, String> {
    let device_id = crate::storage::get_or_create_device_id().map_err(|e| e.to_string())?;
    crate::storage::get_local_usage_stats(&device_id).map_err(|e| e.to_string())
}

/// Load an audio file from disk for renderer playback.
#[tauri::command]
pub fn load_audio_file(path: String) -> Result<Vec<u8>, String> {
    std::fs::read(&path).map_err(|e| format!("Failed to read audio file {}: {}", path, e))
}
