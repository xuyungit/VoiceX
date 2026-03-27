//! Hotkey related Tauri commands

use serde::Serialize;
use tauri::State;

use crate::hotkey::{HotkeyConfiguration, HotkeyManager, HotkeyPermissionStatus};

#[derive(Serialize)]
pub struct RecordedHotkey {
    pub storage: String,
    pub display: String,
}

/// Record the next global key press and return the captured hotkey configuration.
#[tauri::command]
pub async fn record_hotkey(manager: State<'_, HotkeyManager>) -> Result<RecordedHotkey, String> {
    // Wait up to 15 seconds for user input
    let config = manager
        .record_once(15_000)
        .map_err(|e| format!("Failed to record hotkey: {e}"))?;

    Ok(RecordedHotkey {
        storage: config.to_storage(),
        display: config.display_string(),
    })
}

/// Apply the given hotkey configuration for global recognition.
#[tauri::command]
pub async fn apply_hotkey_config(
    manager: State<'_, HotkeyManager>,
    config: Option<String>,
) -> Result<(), String> {
    let parsed = config
        .and_then(|s| HotkeyConfiguration::from_storage(&s))
        .or_else(|| Some(HotkeyConfiguration::default()));

    manager.set_config(parsed);
    Ok(())
}

/// Get the current active hotkey configuration string.
#[tauri::command]
pub fn current_hotkey(manager: State<'_, HotkeyManager>) -> Option<String> {
    manager.current_config().map(|c| c.display_string())
}

/// Check current hotkey-related permissions (macOS only).
#[tauri::command]
pub fn hotkey_permission_status() -> HotkeyPermissionStatus {
    HotkeyPermissionStatus::detect()
}
