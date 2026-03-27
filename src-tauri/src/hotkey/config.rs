//! Hotkey configuration

use serde::{Deserialize, Serialize};

/// Hotkey configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HotkeyConfiguration {
    pub key_code: u32,
    pub modifiers: u32,
    pub uses_fn: bool,
}

impl HotkeyConfiguration {
    pub fn new(key_code: u32, modifiers: u32, uses_fn: bool) -> Self {
        Self {
            key_code,
            modifiers,
            uses_fn,
        }
    }

    pub fn with_uses_fn(key_code: u32, modifiers: u32, uses_fn: bool) -> Self {
        Self {
            key_code,
            modifiers,
            uses_fn,
        }
    }

    /// Default hotkey: Ctrl+Option+Cmd+Space
    pub fn default_primary() -> Self {
        // kVK_Space = 49, controlKey | optionKey | cmdKey
        Self {
            key_code: 49,
            modifiers: 0x1100 | 0x0800 | 0x0100, // control | option | cmd
            uses_fn: false,
        }
    }

    pub fn is_fn_only(&self) -> bool {
        self.uses_fn && self.key_code == 63 && self.modifiers == 0 // kVK_Function = 63
    }

    pub fn is_modifier_only(&self) -> bool {
        !self.uses_fn && self.modifiers == 0 && Self::is_modifier_only_key_code(self.key_code)
    }

    pub fn is_modifier_only_key_code(key_code: u32) -> bool {
        // Modifier key codes: Shift (56/60), Meta/Command/Win (55/54), Alt/Option (58/61), Control (59/62)
        matches!(key_code, 60 | 54 | 56 | 55 | 58 | 61 | 59 | 62)
    }

    pub fn modifiers_bits(&self) -> u32 {
        self.modifiers
    }

    /// Parse from storage format "keyCode|modifiers|usesFn"
    pub fn from_storage(value: &str) -> Option<Self> {
        let parts: Vec<&str> = value.split('|').collect();
        if parts.len() < 2 {
            return None;
        }

        let key_code = parts[0].parse().ok()?;
        let modifiers = parts[1].parse().ok()?;
        let uses_fn = parts.get(2).map(|s| *s == "1").unwrap_or(false);

        Some(Self {
            key_code,
            modifiers,
            uses_fn,
        })
    }

    /// Convert to storage format
    pub fn to_storage(&self) -> String {
        format!(
            "{}|{}|{}",
            self.key_code,
            self.modifiers,
            if self.uses_fn { 1 } else { 0 }
        )
    }

    /// Get display string
    pub fn display_string(&self) -> String {
        if self.is_fn_only() {
            return "Fn".to_string();
        }
        if self.is_modifier_only() {
            return self.key_name();
        }

        let mut parts = Vec::new();
        if self.uses_fn {
            parts.push("Fn".to_string());
        }
        if self.modifiers & 0x1000 != 0 {
            parts.push(Self::ctrl_display_name().to_string());
        }
        if self.modifiers & 0x0800 != 0 {
            parts.push(Self::alt_display_name().to_string());
        }
        if self.modifiers & 0x0200 != 0 {
            parts.push("Shift".to_string());
        }
        if self.modifiers & 0x0100 != 0 {
            parts.push(Self::meta_display_name().to_string());
        }
        parts.push(self.key_name());

        parts.join(" + ")
    }

    /// Platform-specific display name for Control key
    fn ctrl_display_name() -> &'static str {
        if cfg!(target_os = "macos") {
            "Control"
        } else {
            "Ctrl"
        }
    }

    /// Platform-specific display name for Alt/Option key
    fn alt_display_name() -> &'static str {
        if cfg!(target_os = "macos") {
            "Option"
        } else {
            "Alt"
        }
    }

    /// Platform-specific display name for Meta/Command/Win key
    fn meta_display_name() -> &'static str {
        if cfg!(target_os = "macos") {
            "Command"
        } else {
            "Win"
        }
    }

    fn key_name(&self) -> String {
        match self.key_code {
            49 => "Space".to_string(),
            36 => "Return".to_string(),
            48 => "Tab".to_string(),
            53 => "Escape".to_string(),
            51 => "Delete".to_string(),
            60 => "Right Shift".to_string(),
            54 => format!("Right {}", Self::meta_display_name()),
            55 => Self::meta_display_name().to_string(),
            58 => Self::alt_display_name().to_string(),
            61 => format!("Right {}", Self::alt_display_name()),
            59 => Self::ctrl_display_name().to_string(),
            62 => format!("Right {}", Self::ctrl_display_name()),
            56 => "Shift".to_string(),
            63 => "Fn".to_string(),
            code if (65..=90).contains(&code) => {
                // ASCII uppercase letter
                (code as u8 as char).to_string()
            }
            code if (48..=57).contains(&code) => (code as u8 as char).to_string(),
            code if code <= 50 => {
                // Letter/number keys - simplified
                format!("Key {}", code)
            }
            code => format!("Key {}", code),
        }
    }
}

impl Default for HotkeyConfiguration {
    fn default() -> Self {
        Self::default_primary()
    }
}
