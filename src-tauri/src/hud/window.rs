//! HUD window management

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

const HUD_WIDTH: f64 = 256.0;
const HUD_HEIGHT: f64 = 100.0;
const HUD_BOTTOM_MARGIN: f64 = 120.0;

/// Create the HUD window
pub fn create_hud_window(app: &AppHandle) -> Result<(), HudError> {
    if let Some(existing) = app.get_webview_window("hud") {
        position_hud_window(&existing)?;
        return Ok(());
    }

    let window =
        WebviewWindowBuilder::new(app, "hud", WebviewUrl::App("src/hud/index.html".into()))
            .title("VoiceX HUD")
            .inner_size(HUD_WIDTH, HUD_HEIGHT)
            .resizable(false)
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .visible(false) // Start hidden
            .build()
            .map_err(|e| HudError::CreateFailed(e.to_string()))?;

    // Position at bottom center of screen
    position_hud_window(&window)?;

    // Platform-specific configuration
    #[cfg(target_os = "macos")]
    configure_macos_hud(&window)?;

    #[cfg(target_os = "windows")]
    configure_windows_hud(&window)?;

    log::info!("HUD window created");
    Ok(())
}

fn position_hud_window(window: &tauri::WebviewWindow) -> Result<(), HudError> {
    if let Some(monitor) = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
    {
        let scale = monitor.scale_factor();
        let monitor_size = monitor.size().to_logical::<f64>(scale);
        let monitor_origin = monitor.position().to_logical::<f64>(scale);

        let center_x = monitor_origin.x + (monitor_size.width - HUD_WIDTH) / 2.0;
        let min_x = monitor_origin.x;
        let max_x = monitor_origin.x + (monitor_size.width - HUD_WIDTH).max(0.0);
        let x = center_x.clamp(min_x, max_x);

        let bottom_y = monitor_origin.y + monitor_size.height - HUD_HEIGHT - HUD_BOTTOM_MARGIN;
        let min_y = monitor_origin.y;
        let max_y = monitor_origin.y + (monitor_size.height - HUD_HEIGHT).max(0.0);
        let y = bottom_y.clamp(min_y, max_y);

        window
            .set_size(tauri::Size::Logical(tauri::LogicalSize::new(
                HUD_WIDTH, HUD_HEIGHT,
            )))
            .map_err(|e| HudError::SizeFailed(e.to_string()))?;
        window
            .set_position(tauri::Position::Logical(tauri::LogicalPosition::new(x, y)))
            .map_err(|e| HudError::PositionFailed(e.to_string()))?;
    }
    Ok(())
}

/// Recompute HUD size and position for the current monitor / scale factor.
pub fn refresh_hud_window(window: &tauri::WebviewWindow) -> Result<(), HudError> {
    position_hud_window(window)
}

#[cfg(target_os = "macos")]
fn configure_macos_hud(_window: &tauri::WebviewWindow) -> Result<(), HudError> {
    // TODO: Set NSWindow properties
    // - setLevel_(NSFloatingWindowLevel)
    // - setCollectionBehavior_(CanJoinAllSpaces | FullScreenAuxiliary)
    // - setIgnoresMouseEvents_(true)
    // - setHidesOnDeactivate_(false)
    log::debug!("macOS HUD configuration pending");
    Ok(())
}

#[cfg(target_os = "windows")]
fn configure_windows_hud(_window: &tauri::WebviewWindow) -> Result<(), HudError> {
    // TODO: Set WS_EX_TRANSPARENT | WS_EX_LAYERED
    log::debug!("Windows HUD configuration pending");
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn configure_platform_hud(_window: &tauri::WebviewWindow) -> Result<(), HudError> {
    Ok(())
}

/// Show the HUD window
pub fn show_hud(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("hud") {
        let _ = window.show();
    }
}

/// Hide the HUD window
pub fn hide_hud(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("hud") {
        let _ = window.hide();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HudError {
    #[error("Failed to create HUD window: {0}")]
    CreateFailed(String),

    #[error("Failed to resize HUD window: {0}")]
    SizeFailed(String),

    #[error("Failed to position HUD window: {0}")]
    PositionFailed(String),
}
