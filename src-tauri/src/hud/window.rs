//! HUD window management

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// Stream-mode HUD content size (logical points). Shared definition; the
/// same values drive `HudService::sync_bounds` and HUD repositioning.
pub const STREAM_HUD_WIDTH: f64 = 256.0;
pub const STREAM_HUD_HEIGHT: f64 = 100.0;
/// Batch-mode HUD content size (logical points).
pub const BATCH_HUD_WIDTH: f64 = 204.0;
pub const BATCH_HUD_HEIGHT: f64 = 78.0;
/// A read with captions: the stream layout's status row over up to three
/// lines of the sentence being spoken, widened so a 120-character piece is
/// mostly legible rather than mostly ellipsis.
pub const CAPTION_HUD_WIDTH: f64 = 400.0;
pub const CAPTION_HUD_HEIGHT: f64 = 108.0;

const HUD_BOTTOM_MARGIN: f64 = 120.0;
const HUD_MIN_WIDTH: f64 = 128.0;
const HUD_MAX_WIDTH: f64 = 560.0;
const HUD_MIN_HEIGHT: f64 = 56.0;
const HUD_MAX_HEIGHT: f64 = 120.0;
/// macOS whole-window opacity via `NSWindow::setAlphaValue` when the setting
/// is on. Windows does not read this: it keeps the native window
/// composition-transparent and changes CSS alpha instead (see
/// `apply_windows_hud_appearance`).
#[cfg(target_os = "macos")]
const HUD_WINDOW_ALPHA: f64 = 0.88;

/// Desired logical content size, last set via `set_hud_content_bounds`.
///
/// Single source of truth for repositioning. The desired size must never be
/// read back from the window: on Windows, tao's `WM_DPICHANGED` handling can
/// permanently distort the physical size when the window crosses monitors
/// with different scale factors, and treating that distorted size as the
/// desired one would freeze the error in place (refresh would compute the
/// same wrong size forever).
static DESIRED_BOUNDS: std::sync::Mutex<(f64, f64)> =
    std::sync::Mutex::new((STREAM_HUD_WIDTH, STREAM_HUD_HEIGHT));

fn desired_bounds() -> (f64, f64) {
    *DESIRED_BOUNDS
        .lock()
        .expect("DESIRED_BOUNDS mutex poisoned")
}

fn hud_transparent_from_settings() -> bool {
    crate::storage::get_settings()
        .map(|settings| settings.hud_transparent)
        .unwrap_or(false)
}

/// Create the HUD window
pub fn create_hud_window(app: &AppHandle) -> Result<(), HudError> {
    let transparent = hud_transparent_from_settings();

    if let Some(existing) = app.get_webview_window("hud") {
        reposition_hud_window(&existing)?;
        apply_platform_hud_appearance(app, &existing, transparent)?;
        return Ok(());
    }

    let (width, height) = desired_bounds();
    let builder =
        WebviewWindowBuilder::new(app, "hud", WebviewUrl::App("src/hud/index.html".into()))
            .title("VoiceX HUD")
            .inner_size(width, height)
            .resizable(false)
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .visible(false);

    // Windows: composition transparency is a create-time WebView2 flag.
    // The user setting only changes CSS alpha on top of it — recreating the
    // window on toggle would flash and could race a live recording.
    // `shadow(false)` avoids the 1px white border Tauri draws on undecorated
    // Windows windows, which would show through a translucent fill.
    #[cfg(target_os = "windows")]
    let builder = builder
        .transparent(true)
        .shadow(false)
        .background_color(tauri::window::Color(0, 0, 0, 0));

    let window = builder
        .build()
        .map_err(|e| HudError::CreateFailed(e.to_string()))?;

    // Position at the bottom center of the screen under the cursor
    position_hud_window(&window, width, height)?;
    apply_platform_hud_appearance(app, &window, transparent)?;

    log::info!("HUD window created");
    Ok(())
}

/// Apply the current overlay appearance to an already-created HUD.
///
/// macOS changes `NSWindow` alpha. Windows emits a CSS class change; the
/// native window stays composition-transparent for the lifetime of the process.
pub fn apply_hud_appearance(app: &AppHandle, transparent: bool) {
    if let Err(err) = apply_hud_appearance_inner(app, transparent) {
        log::warn!("Failed to apply HUD appearance: {err}");
    }
}

fn apply_hud_appearance_inner(app: &AppHandle, transparent: bool) -> Result<(), HudError> {
    let Some(window) = app.get_webview_window("hud") else {
        return Ok(());
    };
    apply_platform_hud_appearance(app, &window, transparent)
}

fn apply_platform_hud_appearance(
    app: &AppHandle,
    window: &tauri::WebviewWindow,
    transparent: bool,
) -> Result<(), HudError> {
    #[cfg(target_os = "macos")]
    {
        let _ = app;
        return configure_macos_hud(window, transparent);
    }

    #[cfg(target_os = "windows")]
    {
        let _ = window;
        return apply_windows_hud_appearance(app, transparent);
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (app, window, transparent);
        Ok(())
    }
}

fn reposition_hud_window(window: &tauri::WebviewWindow) -> Result<(), HudError> {
    let (width, height) = desired_bounds();
    position_hud_window(window, width, height)
}

fn position_hud_window(
    window: &tauri::WebviewWindow,
    width: f64,
    height: f64,
) -> Result<(), HudError> {
    let width = width.clamp(HUD_MIN_WIDTH, HUD_MAX_WIDTH);
    let height = height.clamp(HUD_MIN_HEIGHT, HUD_MAX_HEIGHT);

    // Track the screen the cursor is on: that is where the user is working.
    // Deliberately not `window.current_monitor()` — while hidden, the HUD
    // stays on whatever screen the system placed it at startup (usually the
    // one with the main settings window) and never follows the user.
    #[cfg(target_os = "macos")]
    return position_hud_on_cursor_screen_macos(window, width, height);

    #[cfg(not(target_os = "macos"))]
    position_hud_on_cursor_screen(window, width, height)
}

/// macOS: place the HUD at the bottom center of the screen nearest the
/// cursor, entirely in AppKit logical coordinates.
///
/// Tauri's `cursor_position()` + `monitor_from_point()` is unusable here:
/// tao flips `NSEvent::mouseLocation` (points) with `CGDisplay::main()
/// .pixels_high()` (pixels) and then scales it by the primary monitor's
/// factor, while `monitor_from_point` compares against `CGDisplayBounds`
/// (points) — the units are wrong twice on a Retina primary display, so the
/// lookup lands on the wrong screen or none. Staying inside the Cocoa
/// coordinate space end-to-end (cursor → screen → window frame) needs no
/// unit conversion at all and is correct under mixed scale factors.
#[cfg(target_os = "macos")]
fn position_hud_on_cursor_screen_macos(
    window: &tauri::WebviewWindow,
    width: f64,
    height: f64,
) -> Result<(), HudError> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSEvent, NSScreen, NSWindow};
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    // `NSScreen::screens` must run on the main thread, and so should frame
    // mutations. When the caller is already on the main thread (app setup or
    // the `ScaleFactorChanged` window event), tauri inlines the closure, so
    // the synchronous `recv` below cannot deadlock.
    let win = window.clone();
    let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();

    window
        .run_on_main_thread(move || {
            let fail = |msg: String| {
                let _ = tx.send(Some(msg));
            };

            // run_on_main_thread guarantees we are on the main thread.
            #[allow(clippy::undocumented_unsafe_blocks)]
            let mtm = unsafe { MainThreadMarker::new_unchecked() };

            // Cocoa global logical coordinates: origin at the main screen's
            // bottom-left corner, y growing upwards. Pick the screen nearest
            // to the cursor instead of an exact hit-test with a
            // `mainScreen` fallback: `mouseLocation` can equal the frame's
            // top/right bound (pointer on the menu-bar row of a secondary
            // screen would miss), and `mainScreen` is indeterminate for an
            // Accessory app whose HUD can never become the key window.
            let mouse = NSEvent::mouseLocation();
            let distance = |f: NSRect| -> f64 {
                let dx = (f.origin.x - mouse.x)
                    .max(0.0)
                    .max(mouse.x - (f.origin.x + f.size.width));
                let dy = (f.origin.y - mouse.y)
                    .max(0.0)
                    .max(mouse.y - (f.origin.y + f.size.height));
                (dx * dx + dy * dy).sqrt()
            };
            let mut screen: Option<objc2::rc::Retained<NSScreen>> = None;
            let mut best_distance = f64::INFINITY;
            for candidate in NSScreen::screens(mtm).iter() {
                let d = distance(candidate.frame());
                if d < best_distance {
                    best_distance = d;
                    screen = Some(candidate.clone());
                }
            }

            let Some(screen) = screen else {
                log::warn!("HUD positioning skipped: no NSScreen available");
                let _ = tx.send(None);
                return;
            };

            let ns_window_ptr = match win.ns_window() {
                Ok(ptr) => ptr,
                Err(e) => {
                    fail(format!("ns_window unavailable: {e:?}"));
                    return;
                }
            };
            #[allow(clippy::undocumented_unsafe_blocks)]
            let ns_win: &NSWindow = unsafe { &*(ns_window_ptr as *const NSWindow) };

            // No clamp against the screen bounds is needed here: width ≤
            // HUD_MAX_WIDTH and HUD_BOTTOM_MARGIN + height fit within any
            // real display. (The Windows branch keeps an explicit clamp
            // because its physical-pixel math makes one cheap.)
            let frame = screen.frame();
            let x = frame.origin.x + (frame.size.width - width) / 2.0;
            // HUD_BOTTOM_MARGIN means "window bottom edge above the screen
            // bottom"; the Cocoa origin is the screen's bottom-left corner.
            let bottom_y = frame.origin.y + HUD_BOTTOM_MARGIN;

            let current = ns_win.frame();
            let size_changed = (current.size.width - width).abs() > 0.5
                || (current.size.height - height).abs() > 0.5;
            let position_changed = (current.origin.x - x).abs() > 0.5
                || (current.origin.y - bottom_y).abs() > 0.5;

            if size_changed {
                // display=true: the HUD may be visible right now (e.g. a
                // `ScaleFactorChanged` mid-recording); skipping the redraw
                // would leave one stale frame.
                ns_win.setFrame_display(
                    NSRect::new(NSPoint::new(x, bottom_y), NSSize::new(width, height)),
                    true,
                );
            } else if position_changed {
                // setFrameTopLeftPoint takes the top-left corner: bottom y
                // plus the window height.
                ns_win.setFrameTopLeftPoint(NSPoint::new(x, bottom_y + height));
            }

            let _ = tx.send(None);
        })
        .map_err(|e| HudError::PositionFailed(e.to_string()))?;

    match rx.recv() {
        Ok(Some(msg)) => Err(HudError::PositionFailed(msg)),
        Ok(None) => Ok(()),
        Err(_) => Err(HudError::PositionFailed(
            "main-thread HUD positioning dropped".into(),
        )),
    }
}

/// Non-macOS: tauri/tao cursor tracking. On Windows the underlying chain is
/// `GetCursorPos` + `MonitorFromPoint` — one uniform pixel coordinate
/// space, reliable. All math in physical pixels because the window can move
/// between screens with different scale factors, and Tauri's
/// logical-coordinate setters convert via the window's *current* screen
/// scale, which is wrong mid-move.
#[cfg(not(target_os = "macos"))]
fn position_hud_on_cursor_screen(
    window: &tauri::WebviewWindow,
    width: f64,
    height: f64,
) -> Result<(), HudError> {
    let app = window.app_handle();
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|cursor| app.monitor_from_point(cursor.x, cursor.y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());

    let Some(monitor) = monitor else {
        return Ok(());
    };

    let scale = monitor.scale_factor();
    let target_w = (width * scale).round();
    let target_h = (height * scale).round();

    let monitor_size = monitor.size();
    let origin_x = monitor.position().x as f64;
    let origin_y = monitor.position().y as f64;

    let x = (origin_x + (monitor_size.width as f64 - target_w) / 2.0).clamp(
        origin_x,
        origin_x + (monitor_size.width as f64 - target_w).max(0.0),
    );
    let y = (origin_y + monitor_size.height as f64 - target_h - HUD_BOTTOM_MARGIN * scale).clamp(
        origin_y,
        origin_y + (monitor_size.height as f64 - target_h).max(0.0),
    );

    let current_size = window
        .inner_size()
        .map_err(|e| HudError::SizeFailed(e.to_string()))?;
    let size_changed = (current_size.width as f64 - target_w).abs() > 1.0
        || (current_size.height as f64 - target_h).abs() > 1.0;

    let current_pos = window
        .outer_position()
        .map_err(|e| HudError::PositionFailed(e.to_string()))?;
    let position_changed =
        (current_pos.x as f64 - x).abs() > 1.0 || (current_pos.y as f64 - y).abs() > 1.0;

    // Move first, resize second. Moving across monitors fires tao's
    // `WM_DPICHANGED` handling, which re-derives the size using the *old*
    // scale; the explicit physical `set_size` afterwards pins the final
    // size to the new monitor's scale. Resizing first (on the old monitor)
    // would let that handler bake in a wrong physical size.
    if position_changed {
        window
            .set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(
                x.round() as i32,
                y.round() as i32,
            )))
            .map_err(|e| HudError::PositionFailed(e.to_string()))?;
    }

    if size_changed {
        window
            .set_size(tauri::Size::Physical(tauri::PhysicalSize::new(
                target_w as u32,
                target_h as u32,
            )))
            .map_err(|e| HudError::SizeFailed(e.to_string()))?;

        // `WM_DPICHANGED` may have adopted the OS-suggested rect, which can
        // shift the window; re-assert the requested position.
        let final_pos = window
            .outer_position()
            .map_err(|e| HudError::PositionFailed(e.to_string()))?;
        if (final_pos.x as f64 - x).abs() > 1.0 || (final_pos.y as f64 - y).abs() > 1.0 {
            window
                .set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(
                    x.round() as i32,
                    y.round() as i32,
                )))
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
    *DESIRED_BOUNDS
        .lock()
        .expect("DESIRED_BOUNDS mutex poisoned") = (width, height);

    if let Some(window) = app.get_webview_window("hud") {
        position_hud_window(&window, width, height)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn configure_macos_hud(
    window: &tauri::WebviewWindow,
    transparent: bool,
) -> Result<(), HudError> {
    use objc2_app_kit::{NSWindow, NSWindowCollectionBehavior};

    let alpha = if transparent {
        HUD_WINDOW_ALPHA
    } else {
        1.0
    };

    window
        .with_webview(move |webview| {
            #[allow(clippy::undocumented_unsafe_blocks)]
            unsafe {
                let ns_window_ptr: *mut std::ffi::c_void = webview.ns_window();
                let ns_win: &NSWindow = &*(ns_window_ptr as *const NSWindow);

                // canJoinAllSpaces: appear on every Space simultaneously
                // fullScreenAuxiliary: appear alongside full-screen apps
                // ignoresCycle: don't appear in Cmd+Tab / Mission Control
                ns_win.setCollectionBehavior(
                    NSWindowCollectionBehavior::CanJoinAllSpaces
                        | NSWindowCollectionBehavior::FullScreenAuxiliary
                        | NSWindowCollectionBehavior::IgnoresCycle,
                );

                // Use a high window level (1000) to float above full-screen apps.
                // NSFloatingWindowLevel (3) is too low for full-screen contexts.
                // kCGMaximumWindowLevelKey is 2147483631, macOS screensaver level
                // is 1000 — we use just below that.
                ns_win.setLevel(999);

                // Click-through HUD: never intercept mouse interactions from the
                // currently focused app underneath.
                ns_win.setIgnoresMouseEvents(true);

                // Public API: composites the entire HUD (chrome + webview)
                // against whatever is underneath. No macos-private-api needed.
                // CSS stays opaque so this alpha is the only fade.
                ns_win.setAlphaValue(alpha);
            }
        })
        .map_err(|e| HudError::PlatformConfigFailed(format!("{e:?}")))?;

    Ok(())
}

#[cfg(target_os = "windows")]
fn apply_windows_hud_appearance(app: &AppHandle, transparent: bool) -> Result<(), HudError> {
    use tauri::Emitter;

    // Click-through (WS_EX_TRANSPARENT) is a separate concern. Do not use
    // WS_EX_LAYERED / SetLayeredWindowAttributes for this visual: that style
    // fights WebView2's compositor, which `transparent(true)` already uses.
    app.emit_to(
        "hud",
        "hud:appearance",
        serde_json::json!({ "transparent": transparent }),
    )
    .map_err(|e| HudError::PlatformConfigFailed(e.to_string()))?;
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

    #[error("Failed to configure HUD window: {0}")]
    PlatformConfigFailed(String),

    #[error("Failed to resize HUD window: {0}")]
    SizeFailed(String),

    #[error("Failed to position HUD window: {0}")]
    PositionFailed(String),
}
