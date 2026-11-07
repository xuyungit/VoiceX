//! OS privacy permissions VoiceX depends on: what is granted, where the user
//! grants the rest, and (macOS) the drag-to-authorize helper window.
//!
//! See `docs/permission-onboarding-2026-11-07.md`.

#[cfg(target_os = "macos")]
pub mod guide;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionKind {
    Microphone,
    Accessibility,
    InputMonitoring,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionState {
    Granted,
    /// Never asked: the system prompt is still available (macOS microphone).
    NotDetermined,
    /// Declined, switched off, or blocked by policy; only Settings can fix it.
    Denied,
    /// The platform has no such permission.
    NotApplicable,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsSnapshot {
    pub platform: &'static str,
    pub microphone: PermissionState,
    pub accessibility: PermissionState,
    pub input_monitoring: PermissionState,
    /// Whether the global hotkey hook is installed. False with every grant in
    /// place means the system still refuses the hook and a restart is needed.
    pub hotkey_listener_active: bool,
}

pub fn snapshot(hotkey_listener_active: bool) -> PermissionsSnapshot {
    PermissionsSnapshot {
        platform: std::env::consts::OS,
        microphone: state(PermissionKind::Microphone),
        accessibility: state(PermissionKind::Accessibility),
        input_monitoring: state(PermissionKind::InputMonitoring),
        hotkey_listener_active,
    }
}

pub fn state(kind: PermissionKind) -> PermissionState {
    #[cfg(target_os = "macos")]
    return macos::state(kind);
    #[cfg(target_os = "windows")]
    return match kind {
        PermissionKind::Microphone => windows::microphone_state(),
        PermissionKind::Accessibility | PermissionKind::InputMonitoring => {
            PermissionState::NotApplicable
        }
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = kind;
        PermissionState::NotApplicable
    }
}

/// The system settings page where `kind` is granted.
pub fn settings_url(kind: PermissionKind) -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    return Some(macos::settings_url(kind));
    #[cfg(target_os = "windows")]
    return match kind {
        PermissionKind::Microphone => Some("ms-settings:privacy-microphone"),
        PermissionKind::Accessibility | PermissionKind::InputMonitoring => None,
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = kind;
        None
    }
}

/// Show the system microphone prompt. Only macOS has one an app can trigger,
/// and only while the state is `NotDetermined`.
pub fn request_microphone() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return macos::request_microphone();
    #[cfg(not(target_os = "macos"))]
    Err("The microphone prompt is only available on macOS".to_string())
}
