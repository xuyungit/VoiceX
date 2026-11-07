//! macOS privacy (TCC) state and System Settings deep links.

use block2::RcBlock;
use objc2::msg_send;
use objc2::runtime::{AnyClass, Bool};
use objc2_foundation::NSString;

use super::{PermissionKind, PermissionState};
use crate::hotkey::HotkeyPermissionStatus;

#[link(name = "AVFoundation", kind = "framework")]
extern "C" {
    static AVMediaTypeAudio: &'static NSString;
}

// AVAuthorizationStatus
const AV_NOT_DETERMINED: isize = 0;
const AV_RESTRICTED: isize = 1;
const AV_DENIED: isize = 2;
const AV_AUTHORIZED: isize = 3;

pub fn state(kind: PermissionKind) -> PermissionState {
    let granted = |ok: bool| {
        if ok {
            PermissionState::Granted
        } else {
            PermissionState::Denied
        }
    };
    match kind {
        PermissionKind::Microphone => microphone_state(),
        PermissionKind::Accessibility => granted(HotkeyPermissionStatus::detect().accessibility),
        PermissionKind::InputMonitoring => {
            granted(HotkeyPermissionStatus::detect().input_monitoring)
        }
    }
}

pub fn settings_url(kind: PermissionKind) -> &'static str {
    match kind {
        PermissionKind::Microphone => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
        }
        PermissionKind::Accessibility => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
        }
        PermissionKind::InputMonitoring => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
        }
    }
}

fn capture_device_class() -> &'static AnyClass {
    AnyClass::get(c"AVCaptureDevice").expect("AVFoundation is linked, so AVCaptureDevice exists")
}

fn microphone_state() -> PermissionState {
    let status: isize = unsafe {
        msg_send![capture_device_class(), authorizationStatusForMediaType: AVMediaTypeAudio]
    };
    match status {
        AV_AUTHORIZED => PermissionState::Granted,
        AV_NOT_DETERMINED => PermissionState::NotDetermined,
        AV_RESTRICTED | AV_DENIED => PermissionState::Denied,
        other => {
            log::warn!("Unknown AVAuthorizationStatus {other}; treating microphone as denied");
            PermissionState::Denied
        }
    }
}

pub fn request_microphone() -> Result<(), String> {
    let handler = RcBlock::new(|granted: Bool| {
        log::info!("Microphone prompt answered: granted={}", granted.as_bool());
    });
    unsafe {
        let _: () = msg_send![
            capture_device_class(),
            requestAccessForMediaType: AVMediaTypeAudio,
            completionHandler: &*handler
        ];
    }
    Ok(())
}
