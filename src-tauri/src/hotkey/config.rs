//! Hotkey configuration
//!
//! # The key code space
//!
//! `key_code` is VoiceX's own numbering, not a platform keycode. The hook
//! produces it from rdev keys on every platform (`key_code_from_key` in
//! `manager.rs`), storage persists it ("keyCode|modifiers|usesFn"), and
//! `src/utils/hotkey.ts` and `scripts/tts/hotkey_env.py` read it back. The
//! ranges are disjoint, and the first two are frozen by stored settings:
//!
//! - special keys: their macOS virtual keycode — Return 36, Tab 48, Space 49,
//!   Delete 51, Escape 53, the modifier keys 54..=62, Fn 63;
//! - letters: ASCII `'A'..='Z'` (65..=90);
//! - digits: tagged ASCII, `DIGIT_KEY_CODE_TAG | '0'..='9'` (304..=313).
//!
//! Digits were plain ASCII (48..=57) once, which is the special keys' range:
//! '1' was Space, '5' was Escape, '7' was a modifier key. See
//! [`HotkeyConfiguration::migrate_legacy_digit_storage`].

use serde::{Deserialize, Serialize};

/// Lifts a digit's ASCII code clear of the special keys, the letters, and any
/// raw platform keycode the hook passes through for keys rdev has no name for
/// (macOS virtual keycodes and Windows VK codes are all below 0x100).
const DIGIT_KEY_CODE_TAG: u32 = 0x100;

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

    /// Default selected-text reading hotkey: Option+Command+R.
    ///
    /// Avoids the system's own "Speak selected text" shortcut (Option-Esc) so
    /// both can coexist.
    pub fn default_read_selection() -> Self {
        Self {
            key_code: 'R' as u32,
            modifiers: 0x0800 | 0x0100, // option | cmd
            uses_fn: false,
        }
    }

    /// Translate-and-read: next to the reading key, same modifiers, so the
    /// two are learned as a pair. Option-Command-T is unclaimed by macOS
    /// itself (Command-T alone is "new tab" nearly everywhere, which is why
    /// the Option is not optional).
    pub fn default_translate_selection() -> Self {
        Self {
            key_code: 'T' as u32,
            modifiers: 0x0800 | 0x0100, // option | cmd
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

    /// Key code of a digit key, `digit` being its value (0..=9).
    pub fn digit_key_code(digit: u8) -> u32 {
        assert!(digit <= 9, "not a digit: {digit}");
        DIGIT_KEY_CODE_TAG | (b'0' + digit) as u32
    }

    fn digit_of_key_code(key_code: u32) -> Option<char> {
        let ascii = key_code.checked_sub(DIGIT_KEY_CODE_TAG)?;
        char::from_u32(ascii).filter(char::is_ascii_digit)
    }

    /// Rewrite a stored binding recorded while digits were plain ASCII, or
    /// `None` when the value needs no rewriting.
    ///
    /// Only 50, 52 and 57 ('2', '4', '9') are rewritten: no other key produced
    /// them on either platform, so they can only be digits. The other seven —
    /// 48 Tab, 49 Space, 51 Delete, 53 Escape, 54/55/56 Right Command, Command,
    /// Shift — stay what they are. A binding recorded on '1' was stored as 49,
    /// the hook fired it on Space and '1' alike, and every surface displayed it
    /// as Space; the stored value does not say which key was pressed, so it
    /// keeps meaning the key the user has been shown all along.
    pub fn migrate_legacy_digit_storage(value: &str) -> Option<String> {
        let (key_code, rest) = value.split_once('|')?;
        let digit = match key_code {
            "50" => 2,
            "52" => 4,
            "57" => 9,
            _ => return None,
        };
        Some(format!("{}|{}", Self::digit_key_code(digit), rest))
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
            code => match Self::digit_of_key_code(code) {
                Some(digit) => digit.to_string(),
                None => format!("Key {}", code),
            },
        }
    }
}

impl Default for HotkeyConfiguration {
    fn default() -> Self {
        Self::default_primary()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_digit_binding_round_trips_through_storage_and_names_itself() {
        let cfg = HotkeyConfiguration::new(HotkeyConfiguration::digit_key_code(1), 0x0800, false);
        assert_eq!(cfg.to_storage(), "305|2048|0");
        assert_eq!(
            HotkeyConfiguration::from_storage("305|2048|0"),
            Some(cfg.clone())
        );
        assert!(cfg.display_string().ends_with(" + 1"));
    }

    #[test]
    fn legacy_digits_no_other_key_produced_are_rewritten() {
        for (legacy, migrated, name) in [
            ("50|2304|0", "306|2304|0", "2"),
            ("52|2304|0", "308|2304|0", "4"),
            ("57|4096|1", "313|4096|1", "9"),
        ] {
            let rewritten = HotkeyConfiguration::migrate_legacy_digit_storage(legacy);
            assert_eq!(rewritten.as_deref(), Some(migrated));
            let cfg = HotkeyConfiguration::from_storage(migrated).unwrap();
            assert_eq!(cfg.key_name(), name);
        }
    }

    #[test]
    fn legacy_codes_shared_with_a_special_key_stay_that_key() {
        for (stored, name) in [
            ("48|2304|0", "Tab"),
            ("49|6400|0", "Space"),
            ("51|2304|0", "Delete"),
            ("53|2304|0", "Escape"),
        ] {
            assert_eq!(
                HotkeyConfiguration::migrate_legacy_digit_storage(stored),
                None
            );
            assert_eq!(
                HotkeyConfiguration::from_storage(stored)
                    .unwrap()
                    .key_name(),
                name
            );
        }
        for modifier_key in ["54|0|0", "55|0|0", "56|0|0"] {
            assert_eq!(
                HotkeyConfiguration::migrate_legacy_digit_storage(modifier_key),
                None
            );
            assert!(HotkeyConfiguration::from_storage(modifier_key)
                .unwrap()
                .is_modifier_only());
        }
    }

    #[test]
    fn the_migration_leaves_current_values_alone() {
        // Letters, already-migrated digits, and anything that is not a binding.
        for stored in ["82|2304|0", "84|2304|0", "306|2304|0", "63|0|1", "", "50"] {
            assert_eq!(
                HotkeyConfiguration::migrate_legacy_digit_storage(stored),
                None
            );
        }
    }
}
