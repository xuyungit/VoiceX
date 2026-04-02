//! HUD window management

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

const HUD_DEFAULT_WIDTH: f64 = 256.0;
const HUD_DEFAULT_HEIGHT: f64 = 100.0;
const HUD_BOTTOM_MARGIN: f64 = 120.0;
const HUD_MIN_WIDTH: f64 = 128.0;
const HUD_MAX_WIDTH: f64 = 560.0;
const HUD_MIN_HEIGHT: f64 = 56.0;
const HUD_MAX_HEIGHT: f64 = 120.0;

/// Create the HUD window
pub fn create_hud_window(app: &AppHandle) -> Result<(), HudError> {
    if let Some(existing) = app.get_webview_window("hud") {
        reposition_hud_window(&existing)?;
        return Ok(());
    }

    let window =
        WebviewWindowBuilder::new(app, "hud", WebviewUrl::App("src/hud/index.html".into()))
            .title("VoiceX HUD")
            .inner_size(HUD_DEFAULT_WIDTH, HUD_DEFAULT_HEIGHT)
            .resizable(false)
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .visible(false) // Start hidden
            .build()
            .map_err(|e| HudError::CreateFailed(e.to_string()))?;

    // Position at bottom center of screen
    position_hud_window(&window, HUD_DEFAULT_WIDTH, HUD_DEFAULT_HEIGHT)?;

    // Platform-specific configuration
    #[cfg(target_os = "macos")]
    configure_macos_hud(&window)?;

    #[cfg(target_os = "windows")]
    configure_windows_hud(&window)?;

    log::info!("HUD window created");
    Ok(())
}

fn reposition_hud_window(window: &tauri::WebviewWindow) -> Result<(), HudError> {
    let size = window
        .inner_size()
        .map_err(|e| HudError::SizeFailed(e.to_string()))?;
    let scale = window
        .scale_factor()
        .map_err(|e| HudError::SizeFailed(e.to_string()))?;
    let logical_size = size.to_logical::<f64>(scale);
    position_hud_window(window, logical_size.width, logical_size.height)
}

fn position_hud_window(
    window: &tauri::WebviewWindow,
    width: f64,
    height: f64,
) -> Result<(), HudError> {
    let width = width.clamp(HUD_MIN_WIDTH, HUD_MAX_WIDTH);
    let height = height.clamp(HUD_MIN_HEIGHT, HUD_MAX_HEIGHT);
    if let Some(monitor) = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
    {
        let scale = monitor.scale_factor();
        let monitor_size = monitor.size().to_logical::<f64>(scale);
        let monitor_origin = monitor.position().to_logical::<f64>(scale);

        let center_x = monitor_origin.x + (monitor_size.width - width) / 2.0;
        let min_x = monitor_origin.x;
        let max_x = monitor_origin.x + (monitor_size.width - width).max(0.0);
        let x = center_x.clamp(min_x, max_x);

        let bottom_y = monitor_origin.y + monitor_size.height - height - HUD_BOTTOM_MARGIN;
        let min_y = monitor_origin.y;
        let max_y = monitor_origin.y + (monitor_size.height - height).max(0.0);
        let y = bottom_y.clamp(min_y, max_y);

        let current_size = window
            .inner_size()
            .map_err(|e| HudError::SizeFailed(e.to_string()))?
            .to_logical::<f64>(scale);
        let size_changed =
            (current_size.width - width).abs() > 0.5 || (current_size.height - height).abs() > 0.5;

        if size_changed {
            window
                .set_size(tauri::Size::Logical(tauri::LogicalSize::new(width, height)))
                .map_err(|e| HudError::SizeFailed(e.to_string()))?;
        }

        let current_pos = window
            .outer_position()
            .map_err(|e| HudError::PositionFailed(e.to_string()))?
            .to_logical::<f64>(scale);
        let position_changed = (current_pos.x - x).abs() > 0.5 || (current_pos.y - y).abs() > 0.5;

        if position_changed {
            window
                .set_position(tauri::Position::Logical(tauri::LogicalPosition::new(x, y)))
                .map_err(|e| HudError::PositionFailed(e.to_string()))?;
        }
    }
    Ok(())
}

/// Recompute HUD size and position for the current monitor / scale factor.
pub fn refresh_hud_window(window: &tauri::WebviewWindow) -> Result<(), HudError> {
    reposition_hud_window(window)
}

pub fn set_hud_content_bounds(app: &AppHandle, width: f64, height: f64) -> Result<(), HudError> {
    if let Some(window) = app.get_webview_window("hud") {
        position_hud_window(&window, width, height)?;
    }
    Ok(())
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
