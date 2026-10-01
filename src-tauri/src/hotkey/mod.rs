//! Hotkey management module

mod config;
mod manager;
// Only used on Windows; compiled everywhere so its test runs on every machine.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod menu_mask;
mod permissions;

pub use config::HotkeyConfiguration;
pub use manager::{HotkeyManager, ReadSelectionStatus};
pub use permissions::HotkeyPermissionStatus;
