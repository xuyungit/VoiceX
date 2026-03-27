use serde::Serialize;
use tauri::State;

use crate::services::sync_service::SyncService;
use crate::storage::{self, SyncState};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStateResponse {
    pub state: SyncState,
    pub device_id: String,
}

#[tauri::command]
pub fn get_sync_state() -> Result<SyncStateResponse, String> {
    let state = storage::get_sync_state().map_err(|e| e.to_string())?;
    let device_id = storage::get_or_create_device_id().map_err(|e| e.to_string())?;

    Ok(SyncStateResponse { state, device_id })
}

#[tauri::command]
pub fn sync_now(sync: State<'_, SyncService>) -> Result<(), String> {
    sync.request_sync_now();
    Ok(())
}
