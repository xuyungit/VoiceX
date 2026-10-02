use std::path::Path;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForegroundAppInfo {
    pub platform: String,
    pub display_name: Option<String>,
    pub process_name: Option<String>,
    pub bundle_id: Option<String>,
    pub executable_path: Option<String>,
    pub process_id: u32,
    pub is_self: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TextInjectionAppOverride {
    pub platform: String,
    pub app_name: String,
    pub match_kind: String,
    pub match_value: String,
    pub mode: String,
    /// Keep the injected text on the clipboard after pasting instead of
    /// restoring the previous content. Only meaningful in pasteboard mode,
    /// and only intended for targets that bridge the clipboard over a
    /// variable-latency channel (e.g. remote-desktop clients), where a timed
    /// restore can race the bridge and is not safe to perform. Defaults to
    /// false: ordinary apps get the backup → paste → restore behavior.
    #[serde(default)]
    pub skip_clipboard_restore: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecentTargetApp {
    pub platform: String,
    pub app_name: String,
    pub match_kind: String,
    pub match_value: String,
    pub display_name: Option<String>,
    pub process_name: Option<String>,
    pub bundle_id: Option<String>,
    pub executable_path: Option<String>,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppMatchCandidate {
    platform: String,
    match_kind: String,
    match_value: String,
}

pub fn detect_foreground_app(app: &AppHandle) -> Result<ForegroundAppInfo, String> {
    #[cfg(target_os = "macos")]
    {
        return macos::detect_foreground_app(app);
    }

    #[cfg(target_os = "windows")]
    {
        return windows::detect_foreground_app();
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = app;
        Err("Foreground app detection is not supported on this platform".to_string())
    }
}

fn process_name_from_path(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
}

fn normalize_platform(platform: &str) -> String {
    platform.trim().to_ascii_lowercase()
}

fn normalize_match_value(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn all_match_candidates(info: &ForegroundAppInfo) -> Vec<AppMatchCandidate> {
    let mut candidates = Vec::new();
    let platform = normalize_platform(&info.platform);

    if let Some(bundle_id) = info.bundle_id.as_deref() {
        let normalized = normalize_match_value(bundle_id);
        if !normalized.is_empty() {
            candidates.push(AppMatchCandidate {
                platform: platform.clone(),
                match_kind: "bundle_id".to_string(),
                match_value: normalized,
            });
        }
    }

    if let Some(executable_path) = info.executable_path.as_deref() {
        let normalized = normalize_match_value(executable_path);
        if !normalized.is_empty() {
            candidates.push(AppMatchCandidate {
                platform: platform.clone(),
                match_kind: "executable_path".to_string(),
                match_value: normalized,
            });
        }
    }

    if let Some(process_name) = info.process_name.as_deref() {
        let normalized = normalize_match_value(process_name);
        if !normalized.is_empty() {
            candidates.push(AppMatchCandidate {
                platform,
                match_kind: "process_name".to_string(),
                match_value: normalized,
            });
        }
    }

    candidates
}

impl ForegroundAppInfo {
    pub fn to_recent_target_app(&self) -> Option<RecentTargetApp> {
        let candidate = all_match_candidates(self).into_iter().next()?;
        let app_name = self
            .display_name
            .clone()
            .or_else(|| self.process_name.clone())
            .unwrap_or_else(|| candidate.match_value.clone());

        Some(RecentTargetApp {
            platform: self.platform.clone(),
            app_name,
            match_kind: candidate.match_kind,
            match_value: candidate.match_value,
            display_name: self.display_name.clone(),
            process_name: self.process_name.clone(),
            bundle_id: self.bundle_id.clone(),
            executable_path: self.executable_path.clone(),
            last_seen_at: chrono::Utc::now().to_rfc3339(),
        })
    }
}

pub fn match_text_injection_override<'a>(
    app: &ForegroundAppInfo,
    overrides: &'a [TextInjectionAppOverride],
) -> Option<&'a TextInjectionAppOverride> {
    let candidates = all_match_candidates(app);
    overrides.iter().rev().find(|override_item| {
        let override_platform = normalize_platform(&override_item.platform);
        let override_value = normalize_match_value(&override_item.match_value);
        candidates.iter().any(|candidate| {
            candidate.platform == override_platform
                && candidate.match_kind == override_item.match_kind
                && candidate.match_value == override_value
        })
    })
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::mpsc::sync_channel;

    use objc2_app_kit::NSWorkspace;
    use tauri::AppHandle;

    use crate::foreground_app::{process_name_from_path, ForegroundAppInfo};

    pub fn detect_foreground_app(app: &AppHandle) -> Result<ForegroundAppInfo, String> {
        let (tx, rx) = sync_channel(1);
        app.run_on_main_thread(move || {
            let _ = tx.send(read_frontmost_app());
        })
        .map_err(|err| format!("Failed to query frontmost app on the main thread: {err}"))?;

        rx.recv()
            .map_err(|err| format!("Failed to receive frontmost app info: {err}"))?
    }

    fn read_frontmost_app() -> Result<ForegroundAppInfo, String> {
        let workspace = NSWorkspace::sharedWorkspace();
        let app = workspace
            .frontmostApplication()
            .ok_or_else(|| "No frontmost macOS application is available".to_string())?;

        let display_name = app.localizedName().map(|name| name.to_string());
        let bundle_id = app.bundleIdentifier().map(|bundle| bundle.to_string());
        let executable_path = app
            .executableURL()
            .and_then(|url| url.path())
            .map(|path| path.to_string());
        let process_name = executable_path
            .as_deref()
            .and_then(process_name_from_path)
            .or_else(|| display_name.clone());
        let process_id = app.processIdentifier() as u32;

        Ok(ForegroundAppInfo {
            platform: "macOS".to_string(),
            display_name,
            process_name,
            bundle_id,
            executable_path,
            process_id,
            is_self: process_id == std::process::id(),
        })
    }
}

/// Windows: start remembering the last window the user worked in, for
/// [`reactivate_window_behind_tray_menu`]. Call once at startup.
#[cfg(target_os = "windows")]
pub fn track_user_foreground() {
    windows::track_user_foreground();
}

/// Windows: give the foreground back to the window that had it before the
/// tray menu was opened, and wait until it has taken keyboard focus back.
///
/// Opening the notification-area menu takes the foreground away from the
/// user's window twice over — the click activates the taskbar, then the menu
/// owner (VoiceX's hidden tray window) is brought to the front so the menu
/// closes on an outside click — and Windows keeps no record of the window
/// before that. Hence the tracker. Returns how long the handover took.
#[cfg(target_os = "windows")]
pub fn reactivate_window_behind_tray_menu() -> Result<std::time::Duration, String> {
    windows::reactivate_window_behind_tray_menu()
}

/// Window classes that take the foreground on the way to the tray menu
/// without being anywhere the user could have selected text: the taskbar
/// that hosts the notification area, the taskbar on other monitors, and the
/// hidden-icons flyout (Windows 10, then Windows 11).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const TRAY_HOST_WINDOW_CLASSES: [&str; 4] = [
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "NotifyIconOverflowWindow",
    "TopLevelWindowForOverflowXamlIsland",
];

/// Whether a foreground change to a window of `class` is part of opening the
/// tray menu rather than the user switching windows.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn is_tray_host_window_class(class: &str) -> bool {
    TRAY_HOST_WINDOW_CLASSES.contains(&class)
}

#[cfg(target_os = "windows")]
mod windows {
    use std::ptr::null_mut;
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use windows_sys::Win32::{
        Foundation::{CloseHandle, HWND},
        System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
        UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK},
        UI::WindowsAndMessaging::{
            DispatchMessageW, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, GetMessageW,
            GetWindowThreadProcessId, IsWindow, IsWindowVisible, SetForegroundWindow,
            TranslateMessage, EVENT_SYSTEM_FOREGROUND, GUITHREADINFO, MSG, WINEVENT_OUTOFCONTEXT,
        },
    };

    use crate::foreground_app::{
        is_tray_host_window_class, process_name_from_path, ForegroundAppInfo,
    };

    /// The last foreground window the user could have been working in, as an
    /// `HWND` value; 0 before the first one is seen.
    static LAST_USER_WINDOW: AtomicIsize = AtomicIsize::new(0);

    /// How long the handover may take: `SetForegroundWindow` returns before
    /// the target's own thread has processed its activation.
    const REACTIVATE_TIMEOUT: Duration = Duration::from_millis(500);
    const REACTIVATE_POLL: Duration = Duration::from_millis(10);

    pub fn track_user_foreground() {
        let spawned = thread::Builder::new()
            .name("voicex-foreground".to_string())
            .spawn(|| unsafe {
                remember(GetForegroundWindow());
                let hook = SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_FOREGROUND,
                    null_mut(),
                    Some(on_foreground_changed),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                );
                if hook.is_null() {
                    log::warn!("No foreground hook; tray-menu reads will miss the selection");
                    return;
                }
                // An out-of-context hook is delivered through this thread's
                // message queue, so the thread lives on pumping it.
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            });
        if let Err(err) = spawned {
            log::warn!("Could not start the foreground tracker: {err}");
        }
    }

    unsafe extern "system" fn on_foreground_changed(
        _hook: HWINEVENTHOOK,
        _event: u32,
        hwnd: HWND,
        _id_object: i32,
        _id_child: i32,
        _event_thread: u32,
        _event_time: u32,
    ) {
        remember(hwnd);
    }

    /// Record `hwnd` unless it is a stop on the way to the tray menu: the
    /// taskbar and the overflow flyout, or VoiceX's own tray window, which
    /// is never shown. VoiceX's main window is recorded like any other — a
    /// menu read from there ends as `focus_is_self`, as it does on macOS.
    unsafe fn remember(hwnd: HWND) {
        if hwnd.is_null() || IsWindowVisible(hwnd) == 0 {
            return;
        }
        let mut class = [0u16; 64];
        let len = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
        let class = String::from_utf16_lossy(&class[..len.max(0) as usize]);
        if is_tray_host_window_class(&class) {
            return;
        }
        LAST_USER_WINDOW.store(hwnd as isize, Ordering::SeqCst);
    }

    pub fn reactivate_window_behind_tray_menu() -> Result<Duration, String> {
        let started = Instant::now();
        let hwnd = LAST_USER_WINDOW.load(Ordering::SeqCst) as HWND;
        unsafe {
            if hwnd.is_null() || IsWindow(hwnd) == 0 {
                return Err("no window to return to".to_string());
            }
            // Allowed without the usual foreground lock: the menu click was
            // the last input and VoiceX's tray window holds the foreground.
            if GetForegroundWindow() != hwnd && SetForegroundWindow(hwnd) == 0 {
                return Err("SetForegroundWindow was refused".to_string());
            }

            let owner_thread = GetWindowThreadProcessId(hwnd, null_mut());
            let deadline = started + REACTIVATE_TIMEOUT;
            loop {
                let in_front = GetForegroundWindow() == hwnd;
                // The window puts keyboard focus back on its last focused
                // control while its own thread handles the activation; UI
                // Automation's focused element and a synthesized Ctrl+C both
                // go wherever that lands, so the read has to wait for it.
                if in_front && focus_restored(owner_thread, hwnd) {
                    return Ok(started.elapsed());
                }
                if Instant::now() >= deadline {
                    // In front but with no focused control reported: let the
                    // read see for itself what the window offers. Not in
                    // front: reading now would read some other window.
                    return if in_front {
                        Ok(started.elapsed())
                    } else {
                        Err("the window did not come to the front".to_string())
                    };
                }
                thread::sleep(REACTIVATE_POLL);
            }
        }
    }

    unsafe fn focus_restored(thread: u32, hwnd: HWND) -> bool {
        let mut info: GUITHREADINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        GetGUIThreadInfo(thread, &mut info) != 0
            && info.hwndActive == hwnd
            && !info.hwndFocus.is_null()
    }

    pub fn detect_foreground_app() -> Result<ForegroundAppInfo, String> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return Err("No foreground Windows window is available".to_string());
            }

            let mut process_id = 0u32;
            GetWindowThreadProcessId(hwnd, &mut process_id);
            if process_id == 0 {
                return Err("Failed to resolve the foreground process ID".to_string());
            }

            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id);
            if process.is_null() {
                return Err(format!(
                    "Failed to open the foreground process (pid={process_id})"
                ));
            }

            let mut buffer = vec![0u16; 4096];
            let mut size = buffer.len() as u32;
            let path_result = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                buffer.as_mut_ptr(),
                &mut size,
            );

            let close_result = CloseHandle(process);
            if close_result == 0 {
                log::warn!("Failed to close foreground process handle (pid={process_id})");
            }

            if path_result == 0 {
                return Err(format!(
                    "Failed to read the foreground process path (pid={process_id})"
                ));
            }

            let executable_path = String::from_utf16_lossy(&buffer[..size as usize]);
            let process_name = process_name_from_path(&executable_path);

            Ok(ForegroundAppInfo {
                platform: "Windows".to_string(),
                display_name: process_name.clone(),
                process_name,
                bundle_id: None,
                executable_path: Some(executable_path),
                process_id,
                is_self: process_id == std::process::id(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{match_text_injection_override, ForegroundAppInfo, TextInjectionAppOverride};

    fn sample_macos_app() -> ForegroundAppInfo {
        ForegroundAppInfo {
            platform: "macOS".to_string(),
            display_name: Some("Windows App".to_string()),
            process_name: Some("Windows App".to_string()),
            bundle_id: Some("com.microsoft.rdc.macos".to_string()),
            executable_path: Some(
                "/Applications/Windows App.app/Contents/MacOS/Windows App".to_string(),
            ),
            process_id: 42,
            is_self: false,
        }
    }

    #[test]
    fn recent_target_app_prefers_bundle_id_on_macos() {
        let app = sample_macos_app();
        let recent = app.to_recent_target_app().expect("recent target app");
        assert_eq!(recent.match_kind, "bundle_id");
        assert_eq!(recent.match_value, "com.microsoft.rdc.macos");
        assert_eq!(recent.app_name, "Windows App");
    }

    #[test]
    fn override_matching_supports_fallback_candidates() {
        let app = sample_macos_app();
        let overrides = vec![
            TextInjectionAppOverride {
                platform: "macOS".to_string(),
                app_name: "Windows App".to_string(),
                match_kind: "process_name".to_string(),
                match_value: "windows app".to_string(),
                mode: "pasteboard".to_string(),
                skip_clipboard_restore: false,
            },
            TextInjectionAppOverride {
                platform: "Windows".to_string(),
                app_name: "Terminal".to_string(),
                match_kind: "process_name".to_string(),
                match_value: "terminal".to_string(),
                mode: "typing".to_string(),
                skip_clipboard_restore: false,
            },
        ];

        let matched = match_text_injection_override(&app, &overrides).expect("matched override");
        assert_eq!(matched.app_name, "Windows App");
    }

    #[test]
    fn latest_duplicate_override_wins() {
        let app = sample_macos_app();
        let overrides = vec![
            TextInjectionAppOverride {
                platform: "macOS".to_string(),
                app_name: "Windows App".to_string(),
                match_kind: "bundle_id".to_string(),
                match_value: "com.microsoft.rdc.macos".to_string(),
                mode: "pasteboard".to_string(),
                skip_clipboard_restore: false,
            },
            TextInjectionAppOverride {
                platform: " macos ".to_string(),
                app_name: "Windows App".to_string(),
                match_kind: "bundle_id".to_string(),
                match_value: " COM.MICROSOFT.RDC.MACOS ".to_string(),
                mode: "typing".to_string(),
                skip_clipboard_restore: false,
            },
        ];

        let matched = match_text_injection_override(&app, &overrides).expect("matched override");
        assert_eq!(matched.mode, "typing");
    }

    #[test]
    fn override_skip_clipboard_restore_defaults_false_and_round_trips() {
        // Overrides persisted before the field existed deserialize with false.
        let legacy = serde_json::json!({
            "platform": "macOS",
            "appName": "Windows App",
            "matchKind": "bundle_id",
            "matchValue": "com.microsoft.rdc.macos",
            "mode": "pasteboard",
        });
        let parsed: TextInjectionAppOverride = serde_json::from_value(legacy).unwrap();
        assert!(!parsed.skip_clipboard_restore);

        // Explicit true survives a serialize/deserialize round trip.
        let with_flag = serde_json::json!({
            "platform": "macOS",
            "appName": "Windows App",
            "matchKind": "bundle_id",
            "matchValue": "com.microsoft.rdc.macos",
            "mode": "pasteboard",
            "skipClipboardRestore": true,
        });
        let parsed: TextInjectionAppOverride = serde_json::from_value(with_flag).unwrap();
        assert!(parsed.skip_clipboard_restore);
        let serialized = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            serialized.get("skipClipboardRestore").and_then(|v| v.as_bool()),
            Some(true)
        );
    }
}
