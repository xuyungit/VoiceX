//! HUD window management
//!
//! Every presentation is a transparent, click-through window with the visible
//! capsule drawn inside it by the page (`src/hud/hud.css`). The window is
//! sized here, for the screen it is about to appear on, before it is shown,
//! and never follows the capsule: a native resize cannot be redrawn in step
//! with the page, and flashes the window each time. The capsule sits
//! `HUD_SHADOW_PAD` in from the window's edges, which leaves room for its
//! CSS shadow.

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

/// Transparent margin between the window's edge and the capsule, holding the
/// capsule's shadow. Must match `--hud-pad` in `hud.css` and `HUD_PAD` in
/// `hud.ts`.
pub const HUD_SHADOW_PAD: f64 = 20.0;
/// Space between the capsule's bottom edge and the top of the work area,
/// which is above the Dock or taskbar. Shared by every presentation, so
/// dictation and reading captions appear in the same spot.
const HUD_CAPSULE_GAP: f64 = 24.0;

/// How wide a presentation's window is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HudWidth {
    /// A fixed width in logical points.
    Points(f64),
    /// This share of the target screen's width for the capsule, plus the
    /// shadow margin on both sides. `hud.ts` wraps its text against the same
    /// share, so the two must agree.
    ScreenShare(f64),
}

impl HudWidth {
    fn resolve(self, screen_width: f64) -> f64 {
        match self {
            HudWidth::Points(width) => width,
            HudWidth::ScreenShare(share) => (screen_width * share).floor() + 2.0 * HUD_SHADOW_PAD,
        }
    }
}

/// Stream dictation: the capsule grows with the transcript up to half the
/// screen width, two 22-point lines plus 9-point padding tall.
pub const STREAM_HUD_WIDTH: HudWidth = HudWidth::ScreenShare(0.5);
pub const STREAM_HUD_HEIGHT: f64 = 102.0;
/// Batch dictation and reads without captions: a 36-point waveform capsule.
/// The window leaves room for the widest it becomes, a two-line error.
pub const BATCH_HUD_WIDTH: HudWidth = HudWidth::Points(480.0);
pub const BATCH_HUD_HEIGHT: f64 = 96.0;
/// A read with captions: the sentence being spoken and nothing else, laid out
/// like a film subtitle — 80% of the screen wide, two 26-point lines tall.
pub const CAPTION_HUD_WIDTH: HudWidth = HudWidth::ScreenShare(0.8);
pub const CAPTION_HUD_HEIGHT: f64 = 134.0;

// The clamps bound what any presentation may ask for; a size outside them
// would be shown smaller than its stylesheet lays out, and silently so.
const HUD_MIN_WIDTH: f64 = 128.0;
// Stream and caption windows are a share of the screen; this bounds only a
// screen wider than any real one.
const HUD_MAX_WIDTH: f64 = 4096.0;
const HUD_MIN_HEIGHT: f64 = 56.0;
/// Tallest any presentation asks for: a two-line caption is about 133.
const HUD_MAX_HEIGHT: f64 = 160.0;

#[derive(Clone, Copy)]
struct HudLayout {
    width: HudWidth,
    height: f64,
}

/// Desired logical content size and placement for the current presentation.
///
/// Single source of truth for repositioning. The desired size must never be
/// read back from the window: on Windows, tao's `WM_DPICHANGED` handling can
/// permanently distort the physical size when the window crosses monitors
/// with different scale factors, and treating that distorted size as the
/// desired one would freeze the error in place (refresh would compute the
/// same wrong size forever).
static DESIRED_LAYOUT: std::sync::Mutex<HudLayout> = std::sync::Mutex::new(HudLayout {
    width: STREAM_HUD_WIDTH,
    height: STREAM_HUD_HEIGHT,
});

fn desired_layout() -> HudLayout {
    *DESIRED_LAYOUT
        .lock()
        .expect("DESIRED_LAYOUT mutex poisoned")
}

/// Distance from the screen's bottom edge to the window's, in logical points.
///
/// The capsule sits `HUD_SHADOW_PAD` above the window's bottom edge, so this
/// leaves `HUD_CAPSULE_GAP` between the capsule and the work area. A work
/// area too short to hold the window as well gives up the gap, not the window.
fn hud_bottom_margin(reserved_bottom: f64, work_height: f64, height: f64) -> f64 {
    let gap = HUD_CAPSULE_GAP - HUD_SHADOW_PAD;
    reserved_bottom + gap.min((work_height - height).max(0.0))
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

    let layout = desired_layout();
    let builder =
        WebviewWindowBuilder::new(app, "hud", WebviewUrl::App("src/hud/index.html".into()))
            .title("VoiceX HUD")
            // Only the screen decides the width; `position_hud_window` sets it
            // below, before the window is ever shown.
            .inner_size(HUD_MIN_WIDTH, layout.height)
            .resizable(false)
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .visible(false);

    // Every presentation draws its capsule inside a transparent window, with
    // the capsule's own CSS shadow. A native shadow would outline the whole
    // window, empty margin included.
    // Windows: composition transparency is a create-time WebView2 flag.
    // The user setting only changes CSS alpha on top of it — recreating the
    // window on toggle would flash and could race a live recording.
    // `shadow(false)` also avoids the 1px white border Tauri draws on
    // undecorated Windows windows.
    #[cfg(target_os = "macos")]
    let builder = builder.transparent(true).shadow(false);

    #[cfg(target_os = "windows")]
    let builder = builder
        .transparent(true)
        .shadow(false)
        .background_color(tauri::window::Color(0, 0, 0, 0));

    let window = builder
        .build()
        .map_err(|e| HudError::CreateFailed(e.to_string()))?;

    // Windows: let clicks fall through. The window is wider than the capsule
    // in it, and would otherwise swallow clicks on whatever is underneath
    // without any visible reason. Tauri implements this with `WS_EX_LAYERED`;
    // reading captions have run with it on the transparent window since they
    // shipped. macOS does the same in `configure_macos_hud`.
    #[cfg(target_os = "windows")]
    window
        .set_ignore_cursor_events(true)
        .map_err(|e| HudError::PlatformConfigFailed(e.to_string()))?;

    // Position at the bottom center of the screen under the cursor
    position_hud_window(&window, layout)?;
    apply_platform_hud_appearance(app, &window, transparent)?;

    log::info!("HUD window created");
    Ok(())
}

/// Apply the current overlay appearance to an already-created HUD.
///
/// The setting makes the capsule's fill translucent, in CSS, on every
/// platform. The window itself is transparent and keeps full opacity: fading
/// it would fade the text and the waveform along with the fill.
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
    configure_macos_hud(window)?;
    #[cfg(not(target_os = "macos"))]
    let _ = window;

    app.emit_to(
        "hud",
        "hud:appearance",
        serde_json::json!({ "transparent": transparent }),
    )
    .map_err(|e| HudError::PlatformConfigFailed(e.to_string()))?;
    Ok(())
}

fn reposition_hud_window(window: &tauri::WebviewWindow) -> Result<(), HudError> {
    position_hud_window(window, desired_layout())
}

fn position_hud_window(window: &tauri::WebviewWindow, layout: HudLayout) -> Result<(), HudError> {
    let width = layout.width;
    let height = layout.height.clamp(HUD_MIN_HEIGHT, HUD_MAX_HEIGHT);

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
    width: HudWidth,
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

            let frame = screen.frame();
            let work_area = screen.visibleFrame();
            let width = width
                .resolve(frame.size.width)
                .clamp(HUD_MIN_WIDTH, HUD_MAX_WIDTH);
            let x = frame.origin.x + (frame.size.width - width) / 2.0;
            // A bottom Dock raises visibleFrame's bottom edge; a screen with
            // no bottom Dock has no reserved strip to leave beneath the HUD.
            let bottom_margin = hud_bottom_margin(
                work_area.origin.y - frame.origin.y,
                work_area.size.height,
                height,
            );
            let bottom_y = frame.origin.y + bottom_margin;

            let current = ns_win.frame();
            let size_changed = (current.size.width - width).abs() > 0.5
                || (current.size.height - height).abs() > 0.5;
            let position_changed =
                (current.origin.x - x).abs() > 0.5 || (current.origin.y - bottom_y).abs() > 0.5;

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
    width: HudWidth,
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
    let width = width
        .resolve(monitor.size().width as f64 / scale)
        .clamp(HUD_MIN_WIDTH, HUD_MAX_WIDTH);
    let target_w = (width * scale).round();
    let target_h = (height * scale).round();

    let monitor_size = monitor.size();
    let origin_x = monitor.position().x as f64;
    let origin_y = monitor.position().y as f64;
    let screen_bottom = origin_y + monitor_size.height as f64;
    let work_area = monitor.work_area();
    let work_bottom = work_area.position.y as f64 + work_area.size.height as f64;
    let bottom_margin = hud_bottom_margin(
        (screen_bottom - work_bottom) / scale,
        work_area.size.height as f64 / scale,
        height,
    ) * scale;

    let x = (origin_x + (monitor_size.width as f64 - target_w) / 2.0).clamp(
        origin_x,
        origin_x + (monitor_size.width as f64 - target_w).max(0.0),
    );
    let y = (screen_bottom - target_h - bottom_margin).clamp(
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

/// Size the window for a presentation, on the screen the cursor is on.
pub fn set_hud_size(app: &AppHandle, width: HudWidth, height: f64) -> Result<(), HudError> {
    let layout = {
        let mut layout = DESIRED_LAYOUT
            .lock()
            .expect("DESIRED_LAYOUT mutex poisoned");
        layout.width = width;
        layout.height = height;
        *layout
    };

    if let Some(window) = app.get_webview_window("hud") {
        position_hud_window(&window, layout)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn configure_macos_hud(window: &tauri::WebviewWindow) -> Result<(), HudError> {
    use objc2_app_kit::{NSWindow, NSWindowCollectionBehavior};

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
            }
        })
        .map_err(|e| HudError::PlatformConfigFailed(format!("{e:?}")))?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capsule_sits_the_same_gap_above_each_screens_work_area() {
        for (reserved_bottom, work_height) in [(0.0, 900.0), (80.0, 820.0), (48.0, 1032.0)] {
            let window_bottom = hud_bottom_margin(reserved_bottom, work_height, 134.0);
            let capsule_bottom = window_bottom + HUD_SHADOW_PAD;
            assert_eq!(capsule_bottom - reserved_bottom, HUD_CAPSULE_GAP);
        }
    }

    #[test]
    fn a_short_work_area_gives_up_the_gap_not_the_window() {
        assert_eq!(hud_bottom_margin(80.0, 100.0, 98.0), 82.0);
        assert_eq!(hud_bottom_margin(80.0, 90.0, 98.0), 80.0);
    }

    /// A presentation asking for more than the clamp allows would be shown
    /// smaller than its stylesheet lays out, and the clamp never says so.
    #[test]
    fn every_presentation_size_survives_the_clamp() {
        for screen_width in [1280.0, 1440.0, 1728.0, 2560.0, 3840.0] {
            for (width, height) in [
                (STREAM_HUD_WIDTH, STREAM_HUD_HEIGHT),
                (BATCH_HUD_WIDTH, BATCH_HUD_HEIGHT),
                (CAPTION_HUD_WIDTH, CAPTION_HUD_HEIGHT),
            ] {
                let width = width.resolve(screen_width);
                assert_eq!(width.clamp(HUD_MIN_WIDTH, HUD_MAX_WIDTH), width);
                assert_eq!(height.clamp(HUD_MIN_HEIGHT, HUD_MAX_HEIGHT), height);
            }
        }
    }

    #[test]
    fn a_screen_share_leaves_the_shadow_margin_outside_the_capsule() {
        assert_eq!(HudWidth::ScreenShare(0.5).resolve(1512.0), 756.0 + 2.0 * HUD_SHADOW_PAD);
        assert_eq!(HudWidth::Points(480.0).resolve(1512.0), 480.0);
    }
}
