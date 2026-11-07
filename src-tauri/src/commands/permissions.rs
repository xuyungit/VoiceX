//! Privacy permission commands: status, system settings, and the macOS
//! drag-to-authorize helper.

use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

use crate::hotkey::HotkeyManager;
use crate::permissions::{self, PermissionKind, PermissionsSnapshot};

#[tauri::command]
pub fn permissions_status(manager: State<'_, HotkeyManager>) -> PermissionsSnapshot {
    permissions::snapshot(manager.is_listener_active())
}

/// Show the system microphone prompt (macOS, while not yet asked).
#[tauri::command]
pub fn permissions_request_microphone() -> Result<(), String> {
    permissions::request_microphone()
}

/// Open the system settings page where `kind` is granted.
#[tauri::command]
pub fn permissions_open_settings(app: AppHandle, kind: PermissionKind) -> Result<(), String> {
    let url = permissions::settings_url(kind)
        .ok_or_else(|| format!("No settings page for {kind:?} on this platform"))?;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("Failed to open {url}: {e}"))
}

/// Open the privacy page for `kind` with the drag helper beside it (macOS).
#[tauri::command]
pub fn permissions_start_guide(app: AppHandle, kind: PermissionKind) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return permissions::guide::open(&app, kind);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, kind);
        Err("The permission guide is only available on macOS".to_string())
    }
}

#[tauri::command]
pub fn permissions_close_guide(app: AppHandle) {
    #[cfg(target_os = "macos")]
    permissions::guide::close(&app);
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub fn permission_guide_info() -> permissions::guide::GuideInfo {
    permissions::guide::info()
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub fn permission_guide_start_drag(window: tauri::WebviewWindow) -> Result<(), String> {
    permissions::guide::start_drag(window)
}

/// Relaunch VoiceX: the way out when every grant is in place but macOS still
/// refuses the keyboard hook to the running process.
#[tauri::command]
pub fn restart_app(app: AppHandle) {
    log::info!("Restarting VoiceX on user request");
    app.restart();
}
