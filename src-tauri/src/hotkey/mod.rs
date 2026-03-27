//! Hotkey management module

mod config;
mod manager;
mod permissions;

pub use config::HotkeyConfiguration;
pub use manager::HotkeyManager;
pub use permissions::HotkeyPermissionStatus;
