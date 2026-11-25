//! History-related commands

use crate::services::sync_service::SyncService;
use crate::storage::{
    AppUsageRange, AppUsageStat, DailyActivityResponse, HistoryRecord, HistoryRecordType, UsageStats,
};
use crate::sync_v2::store::{ModelUsageResponse, OverviewDeviceSyncInfo, OverviewScope};
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
    target_app_key: Option<String>,
) -> Result<Vec<HistoryRecord>, String> {
    let query = query.unwrap_or_default();
    let record_type = record_type.unwrap_or_default();
    let searching = !query.trim().is_empty()
        || !matches!(record_type, HistoryRecordType::All)
        || target_app_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty());
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

    crate::storage::history_search::page(search_id, limit, offset, query, record_type, target_app_key).await
}

/// How many records a search matches in all; History pages load 20 at a time.
#[tauri::command]
pub async fn count_history(
    search_id: u64,
    query: Option<String>,
    record_type: Option<HistoryRecordType>,
    target_app_key: Option<String>,
) -> Result<u32, String> {
    crate::storage::history_search::count(
        search_id,
        query.unwrap_or_default(),
        record_type.unwrap_or_default(),
        target_app_key,
    )
    .await
}

/// Delete a history record
#[tauri::command]
pub async fn delete_history_record(
    id: String,
    sync: State<'_, SyncService>,
    sync_v2: State<'_, crate::sync_v2::engine::SyncV2Service>,
) -> Result<(), String> {
    let sync = sync.inner().clone();
    let sync_v2 = sync_v2.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::storage::delete_history_record(&id).map_err(|e| e.to_string())?;
        sync.request_sync_now();
        sync_v2.notify_local_change();
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Get usage statistics (defaults to all devices; pass `scope = "local"` for this device only).
#[tauri::command]
pub async fn get_usage_stats(scope: Option<OverviewScope>) -> Result<UsageStats, String> {
    let scope = scope.unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        let device_id = crate::storage::get_or_create_device_id().map_err(|e| e.to_string())?;
        crate::storage::with_db(|conn| {
            crate::sync_v2::store::get_overview_usage_stats_in(conn, &device_id, scope)
        })
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
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

/// Get per-app dictation usage statistics across all devices or the current device.
#[tauri::command]
pub async fn get_app_usage_stats(
    range: Option<AppUsageRange>,
    scope: Option<OverviewScope>,
) -> Result<Vec<AppUsageStat>, String> {
    let range = range.unwrap_or_default();
    let scope = scope.unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        let device_id = crate::storage::get_or_create_device_id().map_err(|e| e.to_string())?;
        let today = chrono::Local::now().date_naive();
        crate::storage::with_db(|conn| {
            crate::sync_v2::store::get_app_usage_stats_scoped_in(
                conn,
                &device_id,
                scope,
                range,
                today,
            )
        })
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Get daily dictation activity for the 52-week dot grid.
#[tauri::command]
pub async fn get_daily_activity(
    scope: Option<OverviewScope>,
) -> Result<DailyActivityResponse, String> {
    let scope = scope.unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        let device_id = crate::storage::get_or_create_device_id().map_err(|e| e.to_string())?;
        let today = chrono::Local::now().date_naive();
        crate::storage::with_db(|conn| {
            crate::sync_v2::store::get_daily_activity_scoped_in(conn, &device_id, scope, today)
        })
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Get ASR and LLM model usage breakdown for Overview insights.
#[tauri::command]
pub async fn get_model_usage_stats(
    range: Option<AppUsageRange>,
    scope: Option<OverviewScope>,
) -> Result<ModelUsageResponse, String> {
    let range = range.unwrap_or_default();
    let scope = scope.unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        let device_id = crate::storage::get_or_create_device_id().map_err(|e| e.to_string())?;
        let today = chrono::Local::now().date_naive();
        crate::storage::with_db(|conn| {
            crate::sync_v2::store::get_model_usage_stats_scoped_in(
                conn,
                &device_id,
                scope,
                range,
                today,
            )
        })
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// List known synced devices and their most recent sync time for Overview.
#[tauri::command]
pub async fn get_overview_devices() -> Result<Vec<OverviewDeviceSyncInfo>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let device_id = crate::storage::get_or_create_device_id().map_err(|e| e.to_string())?;
        crate::storage::with_db(|conn| {
            crate::sync_v2::store::get_overview_devices_in(conn, &device_id)
        })
        .map_err(|e| e.to_string())
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
