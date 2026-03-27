#![allow(clippy::upper_case_acronyms)]
use crate::macos::keycodes::code_from_key;
use crate::rdev::{EventType, Key, KeyboardState};
use core_foundation::base::{CFRelease, OSStatus};
use core_foundation::string::UniChar;
use core_foundation_sys::data::{CFDataGetBytePtr, CFDataRef};
use std::convert::TryInto;
use std::ffi::c_void;
use std::os::raw::c_uint;

type TISInputSourceRef = *mut c_void;
type ModifierState = u32;
type UniCharCount = usize;

type OptionBits = c_uint;
const KUC_KEY_TRANSLATE_DEAD_KEYS_BIT: OptionBits = 1 << 31;
const KUC_KEY_ACTION_DOWN: u16 = 0;
#[allow(dead_code)]
const NSEVENT_MODIFIER_FLAG_CAPS_LOCK: u64 = 1 << 16;
#[allow(dead_code)]
const NSEVENT_MODIFIER_FLAG_SHIFT: u64 = 1 << 17;
#[allow(dead_code)]
const NSEVENT_MODIFIER_FLAG_CONTROL: u64 = 1 << 18;
#[allow(dead_code)]
const NSEVENT_MODIFIER_FLAG_OPTION: u64 = 1 << 19;
#[allow(dead_code)]
const NSEVENT_MODIFIER_FLAG_COMMAND: u64 = 1 << 20;
const BUF_LEN: usize = 4;

#[cfg(target_os = "macos")]
#[link(name = "Cocoa", kind = "framework")]
#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> TISInputSourceRef;
    fn TISCopyCurrentKeyboardInputSource() -> TISInputSourceRef;
    fn TISGetInputSourceProperty(source: TISInputSourceRef, property: *mut c_void) -> CFDataRef;
    fn UCKeyTranslate(
        layout: *const u8,
        code: u16,
        key_action: u16,
        modifier_state: u32,
        keyboard_type: u32,
        key_translate_options: OptionBits,
        dead_key_state: *mut u32,
        max_length: UniCharCount,
        actual_length: *mut UniCharCount,
        unicode_string: *mut [UniChar; BUF_LEN],
    ) -> OSStatus;
    fn LMGetKbdType() -> u32;
    static kTISPropertyUnicodeKeyLayoutData: *mut c_void;

}

pub struct Keyboard {
    dead_state: u32,
    shift: bool,
    caps_lock: bool,
}
impl Keyboard {
    pub fn new() -> Option<Keyboard> {
        Some(Keyboard {
            dead_state: 0,
            shift: false,
            caps_lock: false,
        })
    }

    fn modifier_state(&self) -> ModifierState {
        if self.caps_lock || self.shift {
            2
        } else {
            0
        }
    }

pub(crate) unsafe fn string_from_code(
        &mut self,
        code: u32,
        modifier_state: ModifierState,
    ) -> Option<String> {
        let mut keyboard = TISCopyCurrentKeyboardInputSource();
        let mut layout = TISGetInputSourceProperty(keyboard, kTISPropertyUnicodeKeyLayoutData);

        if layout.is_null() {
            // TISGetInputSourceProperty returns NULL when using CJK input methods,
            // using TISCopyCurrentKeyboardLayoutInputSource to fix it.
            keyboard = TISCopyCurrentKeyboardLayoutInputSource();
            layout = TISGetInputSourceProperty(keyboard, kTISPropertyUnicodeKeyLayoutData);
            if layout.is_null() {
                return None;
            }
        }
        let layout_ptr = CFDataGetBytePtr(layout);

        let mut buff = [0_u16; BUF_LEN];
        let kb_type = LMGetKbdType();
        let mut length = 0;
        let _retval = UCKeyTranslate(
            layout_ptr,
            code.try_into().ok()?,
            KUC_KEY_ACTION_DOWN,
            modifier_state,
            kb_type,
            KUC_KEY_TRANSLATE_DEAD_KEYS_BIT,
            &mut self.dead_state,                 // deadKeyState
            BUF_LEN,                              // max string length
            &mut length as *mut UniCharCount,     // actual string length
            &mut buff as *mut [UniChar; BUF_LEN], // unicode string
        );
        CFRelease(keyboard);

        String::from_utf16(&buff[..length]).ok()
    }
}

#[allow(dead_code, clippy::identity_op)]
pub unsafe fn flags_to_state(flags: u64) -> ModifierState {
    let has_alt = flags & NSEVENT_MODIFIER_FLAG_OPTION;
    let has_caps_lock = flags & NSEVENT_MODIFIER_FLAG_CAPS_LOCK;
    let has_control = flags & NSEVENT_MODIFIER_FLAG_CONTROL;
    let has_shift = flags & NSEVENT_MODIFIER_FLAG_SHIFT;
    let has_meta = flags & NSEVENT_MODIFIER_FLAG_COMMAND;
    let mut modifier = 0;
    if has_alt != 0 {
        modifier += 1 << 3;
    }
    if has_caps_lock != 0 {
        modifier += 1 << 1;
    }
    if has_control != 0 {
        modifier += 1 << 4;
    }
    if has_shift != 0 {
        modifier += 1 << 1;
    }
    if has_meta != 0 {
        modifier += 1 << 0;
    }
    modifier
}

impl KeyboardState for Keyboard {
    fn add(&mut self, event_type: &EventType) -> Option<String> {
        match event_type {
            EventType::KeyPress(key) => match key {
                Key::ShiftLeft | Key::ShiftRight => {
                    self.shift = true;
                    None
                }
                Key::CapsLock => {
                    self.caps_lock = !self.caps_lock;
                    None
                }
                key => {
                    let code = code_from_key(*key)?;
                    unsafe { self.string_from_code(code.into(), self.modifier_state()) }
                }
            },
            EventType::KeyRelease(key) => match key {
                Key::ShiftLeft | Key::ShiftRight => {
                    self.shift = false;
                    None
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.dead_state = 0;
        self.shift = false;
        self.caps_lock = false;
    }
}
