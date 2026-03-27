use serde::Serialize;

#[derive(Clone, Serialize)]
pub struct HotkeyPermissionStatus {
    pub platform: String,
    pub accessibility: bool,
    pub input_monitoring: bool,
}

impl HotkeyPermissionStatus {
    pub fn detect() -> Self {
        platform::status()
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::HotkeyPermissionStatus;

    #[allow(non_snake_case)]
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn CGPreflightListenEventAccess() -> bool;
    }

    pub fn status() -> HotkeyPermissionStatus {
        HotkeyPermissionStatus {
            platform: "macos".to_string(),
            accessibility: unsafe { AXIsProcessTrusted() },
            input_monitoring: unsafe { CGPreflightListenEventAccess() },
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::HotkeyPermissionStatus;

    pub fn status() -> HotkeyPermissionStatus {
        HotkeyPermissionStatus {
            platform: std::env::consts::OS.to_string(),
            accessibility: true,
            input_monitoring: true,
        }
    }
}
