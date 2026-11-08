//! In-app update commands; the logic lives in [`crate::updater`].

use tauri::{AppHandle, State};

use crate::updater::{UpdateService, UpdateStatus};

#[tauri::command]
pub fn get_update_status(service: State<'_, UpdateService>) -> UpdateStatus {
    service.status()
}

#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> Result<UpdateStatus, String> {
    crate::updater::check(&app).await
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    crate::updater::install(&app).await
}
