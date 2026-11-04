#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tauri::command]
pub fn get_post_edit_test_status() -> crate::post_edit::diagnostics::Snapshot {
    crate::post_edit::diagnostics::snapshot()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[tauri::command]
pub fn get_post_edit_test_status() -> Result<(), String> {
    Err("unsupported platform".into())
}

use crate::post_edit::records::{self, Mode, RecordPage, Settings};

#[tauri::command]
pub fn get_post_edit_settings() -> Settings {
    records::settings()
}

#[tauri::command]
pub fn set_post_edit_mode(mode: Mode) -> Result<Settings, String> {
    records::set_mode(mode).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_post_edit_records(limit: u32, offset: u32) -> Result<RecordPage, String> {
    records::list(limit, offset).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_post_edit_record(id: String) -> Result<(), String> {
    records::delete_record(&id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_post_edit_records() -> Result<(), String> {
    records::clear().map_err(|e| e.to_string())
}
