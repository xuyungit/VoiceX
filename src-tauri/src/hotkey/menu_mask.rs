//! Windows: masking the bare Alt or Win tap a swallowed hotkey leaves behind.
//!
//! The hook eats a reading hotkey's main key, so everything downstream sees
//! only the modifiers go down and come back up with nothing in between. To
//! Windows that is a tap: a bare Alt tap activates the focused window's menu
//! bar (or Office's ribbon key tips), a bare Win tap opens the Start menu.
//! Either takes the keyboard away from the application whose selection is
//! about to be read — the Start menu even becomes the foreground window — and
//! the copy fallback's Ctrl+C then goes to the wrong place.
//!
//! The remedy is the one AutoHotkey ships as its "menu mask key": while the
//! modifier is still down, inject a key nothing acts on, so the modifier is no
//! longer alone. Virtual key 0xE8 is unassigned.

/// The mask key's virtual-key code, as the hook sees it (`Key::Unknown`).
pub const MASK_KEY_CODE: u32 = 0xE8;

const ALT: u32 = 0x0800;
const META: u32 = 0x0100;

/// Whether swallowing a hotkey held with `modifiers` would leave an Alt or Win
/// tap. Control and Shift taps do nothing on their own.
pub fn needs_mask(modifiers: u32) -> bool {
    modifiers & (ALT | META) != 0
}

/// Inject one press and release of the mask key.
///
/// Called from inside the keyboard hook, while the hotkey's own press is
/// being swallowed: that is the one moment guaranteed to come before the
/// modifier's release. `SendInput` only queues the events — they reach the
/// hook (which lets them through untouched) after this callback returns.
#[cfg(target_os = "windows")]
pub fn send() {
    use std::mem::size_of;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    };

    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: MASK_KEY_CODE as u16,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [key(0), key(KEYEVENTF_KEYUP)];
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    if sent as usize != inputs.len() {
        // Not worth failing the hotkey over; the read itself still runs.
        log::warn!("Menu mask key not sent ({sent} of {} events)", inputs.len());
    }
}

#[cfg(test)]
mod tests {
    use super::needs_mask;

    #[test]
    fn only_alt_and_win_taps_need_masking() {
        assert!(needs_mask(0x0800), "Alt");
        assert!(needs_mask(0x0100), "Win");
        assert!(needs_mask(0x0800 | 0x0100), "Alt + Win");
        assert!(
            needs_mask(0x1000 | 0x0800 | 0x0100),
            "Ctrl + Alt + Win, the default reading chord"
        );
        assert!(needs_mask(0x1000 | 0x0100), "Ctrl + Win");
        assert!(!needs_mask(0x1000), "Ctrl");
        assert!(!needs_mask(0x0200 | 0x1000), "Ctrl + Shift");
        assert!(!needs_mask(0));
    }
}
