//! Windows microphone privacy switches.
//!
//! Settings › Privacy & security › Microphone stores its switches under the
//! capability consent store. Desktop (unpackaged) apps like VoiceX cannot be
//! allowed one by one; three switches decide for all of them:
//!
//! - HKLM `…\microphone` `Value`: "Microphone access" for the whole device
//! - HKCU `…\microphone` `Value`: "Microphone access" for this user
//! - HKCU `…\microphone\NonPackaged` `Value`: "Let desktop apps access your
//!   microphone"
//!
//! Each holds "Allow" or "Deny"; a missing value means Windows' default, which
//! is allow.

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
};

use super::PermissionState;

const CONSENT_STORE: &str =
    r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";

pub fn microphone_state() -> PermissionState {
    let switches = [
        (HKEY_LOCAL_MACHINE, CONSENT_STORE.to_string()),
        (HKEY_CURRENT_USER, CONSENT_STORE.to_string()),
        (HKEY_CURRENT_USER, format!(r"{CONSENT_STORE}\NonPackaged")),
    ];
    for (root, path) in switches {
        if read_string(root, &path, "Value").as_deref() == Some("Deny") {
            return PermissionState::Denied;
        }
    }
    PermissionState::Granted
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn read_string(root: HKEY, subkey: &str, value: &str) -> Option<String> {
    let subkey = wide(subkey);
    let value = wide(value);
    let mut buffer = [0u16; 64];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    let status = unsafe {
        RegGetValueW(
            root,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buffer[..len]))
}
