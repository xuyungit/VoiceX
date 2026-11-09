//! History-related commands

use crate::services::history_service::HISTORY_MODE_TRANSLATE_READ;
use crate::services::sync_service::SyncService;
use crate::storage::{HistoryRecord, HistoryRecordType, UsageStats};
use tauri::State;

/// Allocate a search generation and cancel older reads without touching the writer.
#[tauri::command]
pub fn begin_history_search() -> Result<u64, String> {
    crate::storage::history_search::begin()
}

/// Get history records
///
/// The first unfiltered page applies retention, so History opened after days
/// without a dictation does not show expired records. Later pages and searches
/// need not repeat it: a search runs on every pause in typing.
#[tauri::command]
pub async fn get_history(
    search_id: u64,
    limit: u32,
    offset: u32,
    query: Option<String>,
    record_type: Option<HistoryRecordType>,
) -> Result<Vec<HistoryRecord>, String> {
    let query = query.unwrap_or_default();
    let record_type = record_type.unwrap_or_default();
    let searching = !query.trim().is_empty() || !matches!(record_type, HistoryRecordType::All);
    if offset == 0 && !searching {
        tauri::async_runtime::spawn_blocking(|| {
            let settings = crate::storage::get_settings().map_err(|e| e.to_string())?;
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
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
    }

    crate::storage::history_search::page(search_id, limit, offset, query, record_type).await
}

/// How many records a search matches in all; History pages load 20 at a time.
#[tauri::command]
pub async fn count_history(
    search_id: u64,
    query: Option<String>,
    record_type: Option<HistoryRecordType>,
) -> Result<u32, String> {
    crate::storage::history_search::count(
        search_id,
        query.unwrap_or_default(),
        record_type.unwrap_or_default(),
    )
    .await
}

/// Delete a history record
#[tauri::command]
pub async fn delete_history_record(id: String, sync: State<'_, SyncService>) -> Result<(), String> {
    let sync = sync.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Translate-and-read rows are never uploaded (history_service), so there
        // is nothing on the server to delete and no event to send.
        let mode = crate::storage::history_record_mode(&id).map_err(|e| e.to_string())?;
        crate::storage::delete_history_record(&id).map_err(|e| e.to_string())?;
        if mode.as_deref() != Some(HISTORY_MODE_TRANSLATE_READ) {
            sync.enqueue_history_delete(&id);
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Get usage statistics
#[tauri::command]
pub async fn get_usage_stats() -> Result<UsageStats, String> {
    tauri::async_runtime::spawn_blocking(crate::storage::get_usage_stats)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Get usage statistics for the current device.
#[tauri::command]
pub async fn get_local_usage_stats() -> Result<UsageStats, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let device_id = crate::storage::get_or_create_device_id().map_err(|e| e.to_string())?;
        crate::storage::get_local_usage_stats(&device_id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Load a history record's recording for renderer playback. The record names
/// the file, so the renderer cannot read arbitrary paths; the bytes go back
/// raw rather than as a JSON array of numbers.
#[tauri::command]
pub async fn load_history_audio(record_id: String) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = crate::storage::history_audio_path(&record_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("History record {} has no recording", record_id))?;
        let bytes = std::fs::read(&path)
            .map_err(|e| format!("Failed to read audio file {}: {}", path, e))?;
        Ok(tauri::ipc::Response::new(bytes))
    })
    .await
    .map_err(|e| e.to_string())?
}
