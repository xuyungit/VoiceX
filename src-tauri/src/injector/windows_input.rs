//! Windows-specific text injection using SendInput with KEYEVENTF_UNICODE
//! This provides precise control over Unicode character injection, including CJK punctuation.

use std::mem::size_of;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
};

/// Send a single Unicode character via SendInput with KEYEVENTF_UNICODE.
/// This sends both key-down and key-up events.
fn send_unicode_char(c: u16) -> bool {
    let mut inputs: [INPUT; 2] = unsafe { std::mem::zeroed() };

    // Key down event
    inputs[0].r#type = INPUT_KEYBOARD;
    inputs[0].Anonymous = INPUT_0 {
        ki: KEYBDINPUT {
            wVk: 0, // Must be 0 for KEYEVENTF_UNICODE
            wScan: c,
            dwFlags: KEYEVENTF_UNICODE,
            time: 0,
            dwExtraInfo: 0,
        },
    };

    // Key up event
    inputs[1].r#type = INPUT_KEYBOARD;
    inputs[1].Anonymous = INPUT_0 {
        ki: KEYBDINPUT {
            wVk: 0,
            wScan: c,
            dwFlags: KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
            time: 0,
            dwExtraInfo: 0,
        },
    };

    let sent = unsafe { SendInput(2, inputs.as_ptr(), size_of::<INPUT>() as i32) };
    sent == 2
}

/// Send a string of Unicode text via SendInput.
/// Each character is sent with proper key-down/key-up events.
/// Handles UTF-16 surrogate pairs for characters outside BMP.
pub fn send_unicode_text(text: &str) -> Result<(), String> {
    // Convert to UTF-16 for Windows API
    let utf16: Vec<u16> = text.encode_utf16().collect();

    for code_unit in utf16 {
        if !send_unicode_char(code_unit) {
            return Err(format!(
                "Failed to send character with code unit: {}",
                code_unit
            ));
        }
        // Small delay between characters to ensure proper processing
        std::thread::sleep(std::time::Duration::from_micros(500));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_chinese_punctuation_codes() {
        // Verify Chinese punctuation Unicode code points
        let text = "你好，世界！";
        let utf16: Vec<u16> = text.encode_utf16().collect();
        println!("UTF-16 codes: {:?}", utf16);
        // 你=0x4F60, 好=0x597D, ，=0x3002 (full-width comma), etc.
    }
}
