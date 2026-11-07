//! The drag-to-authorize helper (macOS).
//!
//! Opens the System Settings privacy page for a permission and floats a small
//! window below the System Settings window. The window shows the VoiceX icon;
//! pressing it starts an AppKit file drag of `VoiceX.app`, which the
//! Accessibility and Input Monitoring lists accept like "+" → choose app. A
//! watcher thread closes the helper and brings VoiceX back once the
//! permission is granted.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_graphics::window::{
    copy_window_info, kCGNullWindowID, kCGWindowBounds, kCGWindowLayer,
    kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly, kCGWindowOwnerPID,
};
use objc2_app_kit::NSRunningApplication;
use objc2_foundation::NSString;
use serde::Serialize;
use tauri::{AppHandle, Emitter, LogicalPosition, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

use super::{PermissionKind, PermissionState};

pub const WINDOW_LABEL: &str = "permission-guide";
const WIDTH: f64 = 420.0;
const HEIGHT: f64 = 160.0;
/// Space between the System Settings window and the helper.
const GAP: f64 = 12.0;
/// Keep the helper this far inside the screen edges.
const SCREEN_MARGIN: f64 = 8.0;
const POLL_INTERVAL: Duration = Duration::from_millis(400);
/// How long to wait for System Settings to put its window on screen before
/// showing the helper at the bottom of the screen instead.
const SETTINGS_WINDOW_WAIT: Duration = Duration::from_secs(3);
const SYSTEM_SETTINGS_BUNDLE_ID: &str = "com.apple.systempreferences";
const DRAG_ICON: &[u8] = include_bytes!("../../icons/64x64.png");

/// Bumped by every open and close, so a superseded watcher stops.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static CURRENT_KIND: Mutex<Option<PermissionKind>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuideInfo {
    pub kind: Option<PermissionKind>,
    /// What the drag carries.
    pub app_path: String,
    /// False for a dev build run from `target/`: macOS then charges the
    /// permission to the terminal that launched it, not to this binary.
    pub is_app_bundle: bool,
}

pub fn info() -> GuideInfo {
    let (app_path, is_app_bundle) = app_path();
    GuideInfo {
        kind: *CURRENT_KIND.lock().unwrap_or_else(|e| e.into_inner()),
        app_path: app_path.display().to_string(),
        is_app_bundle,
    }
}

/// Open the privacy page for `kind` and float the drag helper beside it.
pub fn open(app: &AppHandle, kind: PermissionKind) -> Result<(), String> {
    if kind == PermissionKind::Microphone {
        // The Microphone list has no "+": an app gets onto it only through
        // the system prompt, so there is nothing to drag.
        return Err("The microphone cannot be granted by dragging".to_string());
    }
    // The main window would cover System Settings: pressing the helper icon
    // activates VoiceX, and AppKit then orders the main window above the
    // privacy list the icon has to be dropped on. It comes back on `close`.
    if let Some(main) = app.get_webview_window("main") {
        if let Err(err) = main.hide() {
            log::warn!("Failed to hide main window for the permission guide: {err}");
        }
    }
    app.opener()
        .open_url(super::macos::settings_url(kind), None::<&str>)
        .map_err(|e| format!("Failed to open System Settings: {e}"))?;

    *CURRENT_KIND.lock().unwrap_or_else(|e| e.into_inner()) = Some(kind);
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

    if let Some(existing) = app.get_webview_window(WINDOW_LABEL) {
        // Same window, new permission: the page re-reads `info()`.
        let _ = existing.emit("permission-guide:changed", ());
    } else {
        WebviewWindowBuilder::new(
            app,
            WINDOW_LABEL,
            WebviewUrl::App("src/permission-guide/index.html".into()),
        )
        .title("VoiceX")
        .inner_size(WIDTH, HEIGHT)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .focused(false)
        .visible(false)
        .build()
        .map_err(|e| format!("Failed to create permission guide window: {e}"))?;
    }

    let app = app.clone();
    thread::spawn(move || watch(app, kind, generation));
    log::info!("Permission guide opened for {kind:?}");
    Ok(())
}

/// Close the helper and bring back the main window `open` hid.
pub fn close(app: &AppHandle) {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    *CURRENT_KIND.lock().unwrap_or_else(|e| e.into_inner()) = None;
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        if let Err(err) = window.close() {
            log::warn!("Failed to close permission guide: {err}");
        }
    }
    crate::tray::show_main_window(app);
}

/// Start dragging `VoiceX.app` out of the helper window. Must be called while
/// the mouse button is down (the page calls it on `mousedown`).
pub fn start_drag(window: tauri::WebviewWindow) -> Result<(), String> {
    let (path, _) = app_path();
    let source = window.clone();
    window
        .run_on_main_thread(move || {
            let notify = source.clone();
            let result = drag::start_drag(
                &source,
                drag::DragItem::Files(vec![path]),
                drag::Image::Raw(DRAG_ICON.to_vec()),
                move |result, _| {
                    log::info!("Permission guide drag ended: {result:?}");
                    if matches!(result, drag::DragResult::Dropped) {
                        let _ = notify.emit("permission-guide:dropped", ());
                    }
                },
                drag::Options::default(),
            );
            if let Err(err) = result {
                log::error!("Permission guide drag failed to start: {err}");
            }
        })
        .map_err(|e| e.to_string())
}

/// The `.app` bundle this process runs from, or the bare executable for a
/// dev build. The bool says which.
fn app_path() -> (PathBuf, bool) {
    let exe = std::env::current_exe().unwrap_or_default();
    match exe
        .ancestors()
        .find(|p| p.extension().is_some_and(|ext| ext == "app"))
    {
        Some(bundle) => (bundle.to_path_buf(), true),
        None => (exe, false),
    }
}

fn watch(app: AppHandle, kind: PermissionKind, generation: u64) {
    let started = Instant::now();
    let mut shown = false;
    let mut last_target: Option<Rect> = None;
    loop {
        thread::sleep(POLL_INTERVAL);
        if GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        let Some(window) = app.get_webview_window(WINDOW_LABEL) else {
            return;
        };

        if super::state(kind) == PermissionState::Granted {
            log::info!("{kind:?} granted; closing permission guide");
            close(&app);
            let _ = app.emit("permissions:changed", ());
            return;
        }

        // Follow the System Settings window as the user moves it.
        let target = system_settings_frame();
        if target.is_some() && target != last_target {
            last_target = target;
            if let Some(target) = target {
                position(&window, Some(target));
            }
        }
        if !shown && (target.is_some() || started.elapsed() >= SETTINGS_WINDOW_WAIT) {
            if target.is_none() {
                log::warn!("System Settings window not found; showing permission guide at the bottom of the screen");
                position(&window, None);
            }
            if let Err(err) = window.show() {
                log::error!("Failed to show permission guide: {err}");
            }
            shown = true;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Rect {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// Where the helper goes: centred below `target`, or over its bottom edge
/// when there is no room below it on `screen`. Coordinates are global
/// logical points with a top-left origin.
fn placement(target: Rect, screen: Rect, width: f64, height: f64) -> (f64, f64) {
    let min_x = screen.x + SCREEN_MARGIN;
    let max_x = (screen.x + screen.width - width - SCREEN_MARGIN).max(min_x);
    let x = (target.x + (target.width - width) / 2.0).clamp(min_x, max_x);

    let below = target.y + target.height + GAP;
    let screen_bottom = screen.y + screen.height - SCREEN_MARGIN;
    let y = if below + height <= screen_bottom {
        below
    } else {
        (target.y + target.height - height - GAP).max(screen.y + SCREEN_MARGIN)
    };
    (x, y)
}

fn position(window: &tauri::WebviewWindow, target: Option<Rect>) {
    let screens: Vec<Rect> = window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let scale = m.scale_factor();
            Rect {
                x: m.position().x as f64 / scale,
                y: m.position().y as f64 / scale,
                width: m.size().width as f64 / scale,
                height: m.size().height as f64 / scale,
            }
        })
        .collect();
    let Some(primary) = screens.first().copied() else {
        log::warn!("No monitors reported; leaving permission guide where it is");
        return;
    };

    let (x, y) = match target {
        Some(target) => {
            let (cx, cy) = (target.x + target.width / 2.0, target.y + target.height / 2.0);
            let screen = screens
                .iter()
                .copied()
                .find(|s| s.contains(cx, cy))
                .unwrap_or(primary);
            placement(target, screen, WIDTH, HEIGHT)
        }
        // No System Settings window: bottom centre of the main screen.
        None => placement(
            Rect {
                x: primary.x,
                y: primary.y,
                width: primary.width,
                height: primary.height - HEIGHT - 4.0 * GAP,
            },
            primary,
            WIDTH,
            HEIGHT,
        ),
    };
    if let Err(err) = window.set_position(LogicalPosition::new(x, y)) {
        log::warn!("Failed to position permission guide: {err}");
    }
}

/// Frame of the largest on-screen System Settings window. Window bounds and
/// owner pids are readable without the Screen Recording permission.
fn system_settings_frame() -> Option<Rect> {
    let pids: Vec<i64> = NSRunningApplication::runningApplicationsWithBundleIdentifier(
        &NSString::from_str(SYSTEM_SETTINGS_BUNDLE_ID),
    )
    .iter()
    .map(|app| app.processIdentifier() as i64)
    .collect();
    if pids.is_empty() {
        return None;
    }

    let windows = copy_window_info(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID,
    )?;
    let (owner_key, layer_key, bounds_key) = unsafe {
        (
            CFString::wrap_under_get_rule(kCGWindowOwnerPID),
            CFString::wrap_under_get_rule(kCGWindowLayer),
            CFString::wrap_under_get_rule(kCGWindowBounds),
        )
    };

    windows
        .iter()
        .filter_map(|item| {
            let info: CFDictionary<CFString, CFType> =
                unsafe { CFDictionary::wrap_under_get_rule(*item as CFDictionaryRef) };
            let number = |key: &CFString| {
                info.find(key)
                    .and_then(|value| value.downcast::<CFNumber>())
            };
            let pid = number(&owner_key)?.to_i64()?;
            let layer = number(&layer_key)?.to_i64()?;
            if layer != 0 || !pids.contains(&pid) {
                return None;
            }
            let bounds = info.find(&bounds_key)?.downcast::<CFDictionary>()?;
            let bounds: CFDictionary<CFString, CFType> =
                unsafe { CFDictionary::wrap_under_get_rule(bounds.as_concrete_TypeRef()) };
            let field = |name: &'static str| {
                bounds
                    .find(CFString::from_static_string(name))
                    .and_then(|value| value.downcast::<CFNumber>())
                    .and_then(|n| n.to_f64())
            };
            Some(Rect {
                x: field("X")?,
                y: field("Y")?,
                width: field("Width")?,
                height: field("Height")?,
            })
        })
        .max_by(|a, b| (a.width * a.height).total_cmp(&(b.width * b.height)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect { x: 0.0, y: 0.0, width: 1440.0, height: 900.0 };

    #[test]
    fn sits_centred_below_the_settings_window_when_there_is_room() {
        let target = Rect { x: 400.0, y: 100.0, width: 700.0, height: 500.0 };
        assert_eq!(placement(target, SCREEN, 380.0, 140.0), (560.0, 612.0));
    }

    #[test]
    fn overlaps_the_bottom_edge_when_the_window_reaches_the_screen_bottom() {
        let target = Rect { x: 400.0, y: 200.0, width: 700.0, height: 680.0 };
        assert_eq!(placement(target, SCREEN, 380.0, 140.0), (560.0, 728.0));
    }

    #[test]
    fn stays_on_screen_next_to_a_window_at_the_edge() {
        let target = Rect { x: 1300.0, y: 100.0, width: 700.0, height: 300.0 };
        let (x, _) = placement(target, SCREEN, 380.0, 140.0);
        assert_eq!(x, 1440.0 - 380.0 - SCREEN_MARGIN);
    }
}
