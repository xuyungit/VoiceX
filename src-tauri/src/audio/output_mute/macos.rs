//! Core Audio: the main-element mute switch on the output scope of the
//! default output device.
//!
//! Self-declared bindings for the few HAL entry points needed; the selectors
//! are the FourCC codes from `AudioHardware.h` / `AudioHardwareBase.h`.
//! Devices are addressed by UID and resolved to an `AudioObjectID` on every
//! call, since the id of a device that reconnects is not the one it had.

use std::ffi::c_void;
use std::mem::size_of;

use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};

use super::{Backend, DeviceMute, OutputDevice};

type AudioObjectID = u32;
type OSStatus = i32;

#[repr(C)]
struct AudioObjectPropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

const fn four_cc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

const SYSTEM_OBJECT: AudioObjectID = 1;
const UNKNOWN_OBJECT: AudioObjectID = 0;
const ELEMENT_MAIN: u32 = 0;
const SCOPE_GLOBAL: u32 = four_cc(b"glob");
const SCOPE_OUTPUT: u32 = four_cc(b"outp");
const DEFAULT_OUTPUT_DEVICE: u32 = four_cc(b"dOut");
const TRANSLATE_UID_TO_DEVICE: u32 = four_cc(b"uidd");
const DEVICE_UID: u32 = four_cc(b"uid ");
const OBJECT_NAME: u32 = four_cc(b"lnam");
const DEVICE_MUTE: u32 = four_cc(b"mute");

const fn global(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        selector,
        scope: SCOPE_GLOBAL,
        element: ELEMENT_MAIN,
    }
}

const MUTE: AudioObjectPropertyAddress = AudioObjectPropertyAddress {
    selector: DEVICE_MUTE,
    scope: SCOPE_OUTPUT,
    element: ELEMENT_MAIN,
};

#[allow(non_snake_case)]
#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectHasProperty(
        object: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
    ) -> u8;
    fn AudioObjectIsPropertySettable(
        object: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
        out_settable: *mut u8,
    ) -> OSStatus;
    fn AudioObjectGetPropertyData(
        object: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        io_data_size: *mut u32,
        out_data: *mut c_void,
    ) -> OSStatus;
    fn AudioObjectSetPropertyData(
        object: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        data_size: u32,
        data: *const c_void,
    ) -> OSStatus;
}

/// HAL errors are mostly FourCCs (`'!dev'`, `'who?'`); show them that way.
fn describe(status: OSStatus) -> String {
    let bytes = status.to_be_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        format!("OSStatus '{}'", String::from_utf8_lossy(&bytes))
    } else {
        format!("OSStatus {}", status)
    }
}

fn check(status: OSStatus, what: &str) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!("{} failed: {}", what, describe(status)))
    }
}

fn read_u32(
    object: AudioObjectID,
    address: &AudioObjectPropertyAddress,
    what: &str,
) -> Result<u32, String> {
    let mut value: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            address,
            0,
            std::ptr::null(),
            &mut size,
            (&mut value as *mut u32).cast(),
        )
    };
    check(status, what)?;
    Ok(value)
}

/// For the properties that hand back a retained `CFString` (UID, name).
fn read_string(
    object: AudioObjectID,
    address: &AudioObjectPropertyAddress,
    what: &str,
) -> Result<String, String> {
    let mut value: CFStringRef = std::ptr::null();
    let mut size = size_of::<CFStringRef>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            address,
            0,
            std::ptr::null(),
            &mut size,
            (&mut value as *mut CFStringRef).cast(),
        )
    };
    check(status, what)?;
    if value.is_null() {
        return Err(format!("{} returned nothing", what));
    }
    Ok(unsafe { CFString::wrap_under_create_rule(value) }.to_string())
}

/// `UNKNOWN_OBJECT` when no connected device has this UID.
fn device_for_uid(uid: &str) -> Result<AudioObjectID, String> {
    let uid = CFString::new(uid);
    let uid_ref = uid.as_concrete_TypeRef();
    let mut device: AudioObjectID = UNKNOWN_OBJECT;
    let mut size = size_of::<AudioObjectID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &global(TRANSLATE_UID_TO_DEVICE),
            size_of::<CFStringRef>() as u32,
            (&uid_ref as *const CFStringRef).cast(),
            &mut size,
            (&mut device as *mut AudioObjectID).cast(),
        )
    };
    check(status, "translating the device UID")?;
    Ok(device)
}

pub struct PlatformBackend;

impl PlatformBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Backend for PlatformBackend {
    fn default_output(&mut self) -> Result<Option<OutputDevice>, String> {
        let device = read_u32(
            SYSTEM_OBJECT,
            &global(DEFAULT_OUTPUT_DEVICE),
            "reading the default output device",
        )?;
        if device == UNKNOWN_OBJECT {
            return Ok(None);
        }
        let id = read_string(device, &global(DEVICE_UID), "reading the device UID")?;
        let name = read_string(device, &global(OBJECT_NAME), "reading the device name")
            .unwrap_or_else(|_| id.clone());
        Ok(Some(OutputDevice { id, name }))
    }

    fn read_mute(&mut self, id: &str) -> Result<DeviceMute, String> {
        let device = device_for_uid(id)?;
        if device == UNKNOWN_OBJECT {
            return Ok(DeviceMute::Absent);
        }
        if unsafe { AudioObjectHasProperty(device, &MUTE) } == 0 {
            return Ok(DeviceMute::Unsupported);
        }
        let mut settable: u8 = 0;
        check(
            unsafe { AudioObjectIsPropertySettable(device, &MUTE, &mut settable) },
            "checking the mute switch",
        )?;
        if settable == 0 {
            return Ok(DeviceMute::Unsupported);
        }
        let muted = read_u32(device, &MUTE, "reading the mute switch")? != 0;
        Ok(DeviceMute::Settable { muted })
    }

    fn set_mute(&mut self, id: &str, muted: bool) -> Result<(), String> {
        let device = device_for_uid(id)?;
        if device == UNKNOWN_OBJECT {
            return Err("the device is not connected".to_string());
        }
        let value = u32::from(muted);
        check(
            unsafe {
                AudioObjectSetPropertyData(
                    device,
                    &MUTE,
                    0,
                    std::ptr::null(),
                    size_of::<u32>() as u32,
                    (&value as *const u32).cast(),
                )
            },
            "setting the mute switch",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_cc_matches_the_sdk_values() {
        // From the macOS SDK headers, via coreaudio-sys.
        assert_eq!(DEFAULT_OUTPUT_DEVICE, 1682929012);
        assert_eq!(DEVICE_MUTE, 1836414053);
        assert_eq!(SCOPE_OUTPUT, 1869968496);
        assert_eq!(TRANSLATE_UID_TO_DEVICE, 1969841252);
        assert_eq!(describe(four_cc(b"!dev") as i32), "OSStatus '!dev'");
    }

    #[test]
    #[ignore = "talks to this machine's Core Audio; writes back the mute state it read (inaudible)"]
    fn live_default_output_round_trip() {
        let mut backend = PlatformBackend::new();
        let device = backend
            .default_output()
            .unwrap()
            .expect("no default output device");
        let mute = backend.read_mute(&device.id).unwrap();
        println!(
            "default output: {} ({}) -> {:?}",
            device.name, device.id, mute
        );
        if let DeviceMute::Settable { muted } = mute {
            backend.set_mute(&device.id, muted).unwrap();
            assert_eq!(backend.read_mute(&device.id).unwrap(), mute);
        }
        assert_eq!(
            backend.read_mute("voicex-no-such-device").unwrap(),
            DeviceMute::Absent
        );
    }
}
