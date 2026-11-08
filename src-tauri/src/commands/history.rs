//! History-related commands

use crate::services::history_service::HISTORY_MODE_TRANSLATE_READ;
use crate::services::sync_service::SyncService;
use crate::storage::{HistoryRecord, HistoryRecordType, UsageStats};
use tauri::State;

/// Get history records
///
/// The first unfiltered page applies retention, so History opened after days
/// without a dictation does not show expired records. Later pages and searches
/// need not repeat it: a search runs on every pause in typing.
#[tauri::command(async)]
pub fn get_history(
    limit: u32,
    offset: u32,
    query: Option<String>,
    record_type: Option<HistoryRecordType>,
) -> Result<Vec<HistoryRecord>, String> {
    let query = query.unwrap_or_default();
    let record_type = record_type.unwrap_or_default();
    let searching = !query.trim().is_empty() || !matches!(record_type, HistoryRecordType::All);
    if offset == 0 && !searching {
        if let Ok(settings) = crate::storage::get_settings() {
            let text_retention = if settings.sync_enabled {
                0
            } else {
                settings.text_retention_days
            };
            if let Err(err) = crate::storage::cleanup_history_retention(
                text_retention,
                settings.audio_retention_days,
            ) {
                log::warn!("History retention cleanup failed: {}", err);
            }
        }
    }

    crate::storage::search_history(limit, offset, &query, record_type).map_err(|e| e.to_string())
}

/// How many records a search matches in all; History pages load 20 at a time.
#[tauri::command(async)]
pub fn count_history(
    query: Option<String>,
    record_type: Option<HistoryRecordType>,
) -> Result<u32, String> {
    crate::storage::count_history(
        query.as_deref().unwrap_or_default(),
        record_type.unwrap_or_default(),
    )
    .map_err(|e| e.to_string())
}

/// Delete a history record
#[tauri::command(async)]
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
#[tauri::command(async)]
pub fn get_usage_stats() -> Result<UsageStats, String> {
    crate::storage::get_usage_stats().map_err(|e| e.to_string())
}

/// Get usage statistics for the current device.
#[tauri::command(async)]
pub fn get_local_usage_stats() -> Result<UsageStats, String> {
    let device_id = crate::storage::get_or_create_device_id().map_err(|e| e.to_string())?;
    crate::storage::get_local_usage_stats(&device_id).map_err(|e| e.to_string())
}

/// Load a history record's recording for renderer playback. The record names
/// the file, so the renderer cannot read arbitrary paths; the bytes go back
/// raw rather than as a JSON array of numbers.
#[tauri::command(async)]
pub fn load_history_audio(record_id: String) -> Result<tauri::ipc::Response, String> {
    let path = crate::storage::history_audio_path(&record_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("History record {} has no recording", record_id))?;
    let bytes =
        std::fs::read(&path).map_err(|e| format!("Failed to read audio file {}: {}", path, e))?;
    Ok(tauri::ipc::Response::new(bytes))
}
