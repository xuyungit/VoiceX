//! HUD-related commands

#[tauri::command]
pub fn set_hud_content_bounds(
    app: tauri::AppHandle,
    width: f64,
    height: f64,
) -> Result<(), String> {
    crate::hud::set_hud_content_bounds(&app, width, height).map_err(|e| e.to_string())
}
