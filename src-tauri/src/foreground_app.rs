use std::path::Path;

use serde::Serialize;
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

#[cfg(target_os = "windows")]
mod windows {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
        UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
    };

    use crate::foreground_app::{process_name_from_path, ForegroundAppInfo};

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
