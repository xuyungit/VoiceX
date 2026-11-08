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
//!
//! Every hotkey whose main key the hook swallows needs it: the reading keys
//! and the dictation key alike (Ctrl + Alt + Win + Space by default — a bare
//! Win tap there opens the Start menu, and the transcript is pasted into it).

/// The mask key's virtual-key code, as the hook sees it (`Key::Unknown`).
pub const MASK_KEY_CODE: u32 = 0xE8;

const ALT: u32 = 0x0800;
const META: u32 = 0x0100;

/// Whether swallowing a hotkey held with `modifiers` would leave an Alt or Win
/// tap. Control and Shift taps do nothing on their own.
pub fn needs_mask(modifiers: u32) -> bool {
    modifiers & (ALT | META) != 0
}

/// Sends the mask key from a thread of its own.
///
/// The hook callback must never call `SendInput` itself. Windows can run the
/// low-level hooks for injected input before `SendInput` returns, and on the
/// hook thread that means re-entering the hook while the callback is still
/// running. That fits the field report this replaced: the first Alt/Win
/// reading press never reached the reading controller, and every hotkey was
/// dead until the app restarted. From here the injected keys reach the hook
/// as ordinary input from another thread.
///
/// The request is made while the hotkey's own press is being swallowed, and
/// this thread wakes within a millisecond or so — well before a hand lets go
/// of the modifier, which is the only deadline the mask has.
#[cfg(target_os = "windows")]
pub struct Injector {
    requests: std::sync::mpsc::Sender<()>,
}

#[cfg(target_os = "windows")]
impl Injector {
    pub fn spawn() -> Self {
        let (requests, pending) = std::sync::mpsc::channel::<()>();
        let spawned = std::thread::Builder::new()
            .name("voicex-menu-mask".to_string())
            .spawn(move || {
                while pending.recv().is_ok() {
                    send();
                }
            });
        if let Err(err) = spawned {
            // Hotkeys still work; Alt or Win taps are just not masked.
            log::error!("Menu mask thread not started; Alt/Win taps will not be masked: {err}");
        }
        Self { requests }
    }

    /// Ask for one mask press. A channel send: safe inside the hook callback.
    pub fn request(&self) {
        let _ = self.requests.send(());
    }
}

/// Inject one press and release of the mask key. Never from the hook thread;
/// see [`Injector`].
#[cfg(target_os = "windows")]
fn send() {
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
        // Not worth failing the hotkey over; the action itself still runs.
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
