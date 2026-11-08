use serde::Serialize;
use tauri::Manager;
use tauri_plugin_opener::OpenerExt;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildInfo {
    pub version: String,
    pub profile: String,
    pub commit: String,
    pub built_at: String,
}

#[tauri::command]
pub fn get_build_info() -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        profile: env!("VOICEX_BUILD_PROFILE").to_string(),
        commit: env!("VOICEX_GIT_COMMIT").to_string(),
        built_at: env!("VOICEX_BUILD_TIME").to_string(),
    }
}

/// Open the folder holding the diagnostic log (`voicex.log`, plus one
/// rotated backup), so it can be attached to a bug report.
#[tauri::command]
pub fn open_logs_dir(app: tauri::AppHandle) -> Result<(), String> {
    let dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
    app.opener()
        .open_path(dir.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}
