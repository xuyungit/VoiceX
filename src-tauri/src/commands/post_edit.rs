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

use crate::post_edit::records::{self, FinalText, Mode, RecordPage, Settings};

#[tauri::command]
pub fn get_post_edit_settings() -> Settings {
    records::settings()
}

#[tauri::command]
pub fn set_post_edit_mode(mode: Mode) -> Result<Settings, String> {
    records::set_mode(mode).map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn get_post_edit_records(limit: u32, offset: u32) -> Result<RecordPage, String> {
    records::list(limit, offset).map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn delete_post_edit_record(id: String, record_id: Option<String>) -> Result<(), String> {
    records::delete_record(&id, record_id.as_deref()).map_err(|e| e.to_string())
}


/// Sets a dictation's final text; used by both history views.
#[tauri::command]
pub fn set_history_final_text(record_id: String, text: String) -> Result<FinalText, String> {
    records::set_final_text(&record_id, &text).map_err(|e| e.to_string())
}
