#![allow(improper_ctypes_definitions)]
use crate::macos::common::*;
use crate::rdev::{Event, GrabError};
use cocoa::base::nil;
use cocoa::foundation::NSAutoreleasePool;
use core_graphics::event::{CGEventTapLocation, CGEventType};
use lazy_static::lazy_static;
use std::os::raw::c_void;
use std::sync::Mutex;

lazy_static! {
    static ref GLOBAL_CALLBACK: Mutex<Option<Box<dyn FnMut(Event) -> Option<Event> + Send>>> =
        Mutex::new(None);
}

#[link(name = "Cocoa", kind = "framework")]
extern "C" {}

unsafe extern "C" fn raw_callback(
    _proxy: CGEventTapProxy,
    _type: CGEventType,
    cg_event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    if let Some(event) = convert(_type, &cg_event) {
        if let Ok(mut guard) = GLOBAL_CALLBACK.lock() {
            if let Some(callback) = guard.as_mut() {
                if callback(event).is_none() {
                    cg_event.set_type(CGEventType::Null);
                }
            }
        }
    }
    cg_event
}

pub fn grab<T>(callback: T) -> Result<(), GrabError>
where
    T: FnMut(Event) -> Option<Event> + Send + 'static,
{
    unsafe {
        if let Ok(mut guard) = GLOBAL_CALLBACK.lock() {
            *guard = Some(Box::new(callback));
        } else {
            return Err(GrabError::EventTapError);
        }
        let _pool = NSAutoreleasePool::new(nil);
        let tap = CGEventTapCreate(
            CGEventTapLocation::HID, // HID, Session, AnnotatedSession,
            kCGHeadInsertEventTap,
            CGEventTapOption::Default,
            kCGEventMaskForAllEvents,
            raw_callback,
            nil,
        );
        if tap.is_null() {
            return Err(GrabError::EventTapError);
        }
        let _loop = CFMachPortCreateRunLoopSource(nil, tap, 0);
        if _loop.is_null() {
            return Err(GrabError::LoopSourceError);
        }

        let current_loop = CFRunLoopGetCurrent();
        CFRunLoopAddSource(current_loop, _loop, kCFRunLoopCommonModes);

        CGEventTapEnable(tap, true);
        CFRunLoopRun();
    }
    Ok(())
}
