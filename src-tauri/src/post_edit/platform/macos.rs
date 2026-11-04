use std::ffi::c_void;
use std::sync::Arc;
use std::time::Instant;

#[cfg(test)]
use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{
    CFEqual, CFGetTypeID, CFRange, CFRelease, CFType, CFTypeID, CFTypeRef, TCFType,
};
use core_foundation::number::{CFNumber, CFNumberRef};
use core_foundation::runloop::{
    kCFRunLoopDefaultMode, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRemoveSource,
    CFRunLoopRunInMode, CFRunLoopSourceRef,
};
use core_foundation::string::{CFString, CFStringRef};
use objc2::rc::autoreleasepool;

use super::{EventSignal, OwnedSnapshot, ReadError, DESTROYED, FOCUS_CHANGED, TEXT_CHANGED};

type ElementRef = crate::selection::macos::ax::AXUIElementRef;
type ObserverRef = *const c_void;
const MAX_UNITS: isize = 16_384;
const AX_SUCCESS: i32 = 0;
const AX_NO_VALUE: i32 = -25212;
const AX_DISABLED: i32 = -25211;
const AX_VALUE_CF_RANGE: u32 = 4;
#[cfg(test)]
pub(super) static BODY_READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXObserverCreate(
        pid: i32,
        callback: extern "C" fn(ObserverRef, ElementRef, CFStringRef, *mut c_void),
        observer: *mut ObserverRef,
    ) -> i32;
    fn AXObserverGetRunLoopSource(observer: ObserverRef) -> CFRunLoopSourceRef;
    fn AXObserverAddNotification(
        observer: ObserverRef,
        element: ElementRef,
        name: CFStringRef,
        context: *mut c_void,
    ) -> i32;
    fn AXObserverRemoveNotification(
        observer: ObserverRef,
        element: ElementRef,
        name: CFStringRef,
    ) -> i32;
    fn AXUIElementCreateApplication(pid: i32) -> ElementRef;
    fn AXUIElementGetTypeID() -> CFTypeID;
    #[cfg(test)]
    fn AXUIElementCopyAttributeNames(element: ElementRef, names: *mut CFArrayRef) -> i32;
    fn AXUIElementGetPid(element: ElementRef, pid: *mut i32) -> i32;
    fn AXUIElementSetMessagingTimeout(element: ElementRef, seconds: f32) -> i32;
    fn AXUIElementCopyAttributeValue(
        element: ElementRef,
        name: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementIsAttributeSettable(
        element: ElementRef,
        name: CFStringRef,
        settable: *mut u8,
    ) -> i32;
    fn AXUIElementCopyParameterizedAttributeValue(
        element: ElementRef,
        name: CFStringRef,
        parameter: CFTypeRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXValueGetTypeID() -> CFTypeID;
    fn AXValueGetValue(value: CFTypeRef, kind: u32, out: *mut c_void) -> bool;
    fn AXValueCreate(kind: u32, value: *const c_void) -> CFTypeRef;
}

#[cfg(test)]
pub(super) fn fixture_subrole_diagnostic(pid: u32) -> Result<String, ReadError> {
    let element = focused(pid)?;
    string_attribute(&element, "AXSubrole")
}

#[cfg(test)]
pub(super) fn fixture_role_diagnostic(pid: u32) -> Result<String, ReadError> {
    let element = focused(pid)?;
    string_attribute(&element, "AXRole")
}

#[cfg(test)]
pub(super) fn fixture_capabilities(pid: u32) -> Result<Vec<String>, ReadError> {
    let focused = focused(pid)?;
    let mut raw = std::ptr::null();
    status(unsafe { AXUIElementCopyAttributeNames(focused.0, &mut raw) })?;
    let names: CFArray<CFString> = unsafe { CFArray::wrap_under_create_rule(raw) };
    Ok(names.iter().map(|name| name.to_string()).collect())
}

#[cfg(test)]
pub(super) fn metadata_probe(pid: u32) -> Result<(), ReadError> {
    // The last focused node may be inspected while the app is in the
    // background. This probe requests metadata only, never text or selection.
    let app = Element(unsafe { AXUIElementCreateApplication(pid as i32) });
    status(unsafe { AXUIElementSetMessagingTimeout(app.0, 0.1) })?;
    let value = attribute(&app, "AXFocusedUIElement")?;
    if unsafe { CFGetTypeID(value.as_CFTypeRef()) } != unsafe { AXUIElementGetTypeID() } {
        return Err(ReadError::Unsupported);
    }
    let node =
        Element(unsafe { core_foundation::base::CFRetain(value.as_CFTypeRef()) } as ElementRef);
    status(unsafe { AXUIElementSetMessagingTimeout(node.0, 0.1) })?;
    let mut raw = std::ptr::null();
    status(unsafe { AXUIElementCopyAttributeNames(node.0, &mut raw) })?;
    let names: CFArray<CFString> = unsafe { CFArray::wrap_under_create_rule(raw) };
    let names: Vec<String> = names.iter().map(|name| name.to_string()).collect();
    println!(
        "foreground={}, role={:?}, subrole={:?}, chromium_node={}, editable_ancestor={}, value={}",
        same_frontmost(pid).is_ok(),
        string_attribute(&node, "AXRole"),
        string_attribute(&node, "AXSubrole"),
        names.iter().any(|s| s == "ChromeAXNodeId"),
        names.iter().any(|s| s == "AXEditableAncestor"),
        names.iter().any(|s| s == "AXValue")
    );
    Ok(())
}

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn IsSecureEventInputEnabled() -> bool;
}

struct Element(ElementRef);
impl Drop for Element {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0 as CFTypeRef) }
        }
    }
}

pub(super) struct NativeReader {
    target: Option<(u64, u64, u32, Element)>,
    observer: Option<Subscription>,
    events: Arc<EventSignal>,
}

struct Context {
    events: Arc<EventSignal>,
    generation: u64,
    target: Element,
}
struct Subscription {
    observer: ObserverRef,
    registrations: Vec<(Element, CFString)>,
    _context: Box<Context>,
    text_supported: bool,
}

extern "C" fn notification(
    _: ObserverRef,
    element: ElementRef,
    name: CFStringRef,
    raw: *mut c_void,
) {
    // Callback and teardown run only on the owning native worker.
    let context = unsafe { &*(raw as *const Context) };
    let name = unsafe { CFString::wrap_under_get_rule(name) }.to_string();
    if name == "AXFocusedUIElementChanged"
        && unsafe { CFEqual(element as CFTypeRef, context.target.0 as CFTypeRef) } != 0
    {
        return;
    }
    let flags = match name.as_str() {
        "AXValueChanged" => TEXT_CHANGED,
        "AXUIElementDestroyed" => DESTROYED,
        _ => FOCUS_CHANGED,
    };
    context.events.notify(context.generation, flags);
}

impl Subscription {
    fn new(
        pid: u32,
        element: &Element,
        generation: u64,
        events: Arc<EventSignal>,
    ) -> Result<Self, ReadError> {
        let mut observer = std::ptr::null();
        status(unsafe { AXObserverCreate(pid as i32, notification, &mut observer) })?;
        let mut subscription = Self {
            observer,
            registrations: Vec::new(),
            _context: Box::new(Context {
                events,
                generation,
                target: Element(
                    unsafe { core_foundation::base::CFRetain(element.0 as CFTypeRef) }
                        as ElementRef,
                ),
            }),
            text_supported: false,
        };
        let app = Element(unsafe { AXUIElementCreateApplication(pid as i32) });
        if app.0.is_null() {
            return Err(ReadError::Unsupported);
        }
        status(unsafe { AXUIElementSetMessagingTimeout(app.0, 0.1) })?;
        for (target, name) in [
            (element, "AXValueChanged"),
            (element, "AXUIElementDestroyed"),
            (&app, "AXFocusedUIElementChanged"),
            (&app, "AXApplicationDeactivated"),
        ] {
            let name = CFString::new(name);
            let code = unsafe {
                AXObserverAddNotification(
                    observer,
                    target.0,
                    name.as_concrete_TypeRef(),
                    &mut *subscription._context as *mut Context as *mut c_void,
                )
            };
            if code == AX_SUCCESS {
                if name.to_string() == "AXValueChanged" {
                    subscription.text_supported = true;
                }
                subscription.registrations.push((
                    Element(
                        unsafe { core_foundation::base::CFRetain(target.0 as CFTypeRef) }
                            as ElementRef,
                    ),
                    name,
                ));
            } else if code != -25207 && code != -25208 {
                return Err(ReadError::Native(code));
            }
        }
        unsafe {
            CFRunLoopAddSource(
                CFRunLoopGetCurrent(),
                AXObserverGetRunLoopSource(observer),
                kCFRunLoopDefaultMode,
            )
        };
        Ok(subscription)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        unsafe {
            CFRunLoopRemoveSource(
                CFRunLoopGetCurrent(),
                AXObserverGetRunLoopSource(self.observer),
                kCFRunLoopDefaultMode,
            )
        };
        for (element, name) in &self.registrations {
            unsafe {
                AXObserverRemoveNotification(self.observer, element.0, name.as_concrete_TypeRef());
            }
        }
        unsafe { CFRelease(self.observer) };
    }
}

impl NativeReader {
    pub(super) fn clear(&mut self) {
        self.observer = None;
        self.target = None;
    }
    pub(super) fn new() -> Self {
        Self {
            target: None,
            observer: None,
            events: Arc::new(EventSignal::default()),
        }
    }
    pub(super) fn set_events(&mut self, events: Arc<EventSignal>) {
        self.events = events;
    }
    pub(super) fn watching(&self) -> bool {
        self.observer.is_some()
    }
    pub(super) fn pump(&mut self) {
        autoreleasepool(|_| unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.0, true as u8);
        });
    }

    pub(super) fn capture(
        &mut self,
        generation: u64,
        expected_pid: u32,
    ) -> Result<OwnedSnapshot, ReadError> {
        autoreleasepool(|_| {
            self.clear();
            let focused = focused(expected_pid)?;
            let mut pid = 0;
            status(unsafe { AXUIElementGetPid(focused.0, &mut pid) })?;
            if pid <= 0 || pid as u32 != expected_pid {
                return Err(ReadError::TargetChanged);
            }
            check_control(&focused)?;
            self.observer = match Subscription::new(
                expected_pid,
                &focused,
                generation,
                Arc::clone(&self.events),
            ) {
                Ok(subscription) if !subscription.registrations.is_empty() => Some(subscription),
                Ok(_) | Err(ReadError::Native(-25207 | -25208)) => None,
                Err(error) => return Err(error),
            };
            self.target = Some((generation, super::target_token(), expected_pid, focused));
            self.read_inner(generation)
        })
    }

    pub(super) fn read(&mut self, generation: u64) -> Result<OwnedSnapshot, ReadError> {
        autoreleasepool(|_| self.read_inner(generation))
    }

    fn read_inner(&self, generation: u64) -> Result<OwnedSnapshot, ReadError> {
        let (target_generation, token, pid, element) =
            self.target.as_ref().ok_or(ReadError::TargetChanged)?;
        if *target_generation != generation {
            return Err(ReadError::StaleGeneration);
        }
        same_focus(*pid, element)?;
        check_control(element)?;
        let composition_before = composition_state(element)?;
        if composition_before == super::CompositionState::Active {
            return Err(ReadError::CompositionActive);
        }
        let count_value = attribute(element, "AXNumberOfCharacters")?;
        if unsafe { CFGetTypeID(count_value.as_CFTypeRef()) } != CFNumber::type_id() {
            return Err(ReadError::Unsupported);
        }
        let count =
            unsafe { CFNumber::wrap_under_get_rule(count_value.as_CFTypeRef() as CFNumberRef) }
                .to_i64()
                .ok_or(ReadError::Unsupported)?;
        if count < 0 || count > MAX_UNITS as i64 {
            return Err(ReadError::TooLarge);
        }
        let selection = attribute(element, "AXSelectedTextRange")?;
        if unsafe { CFGetTypeID(selection.as_CFTypeRef()) } != unsafe { AXValueGetTypeID() } {
            return Err(ReadError::Unsupported);
        }
        let mut range = CFRange {
            location: 0,
            length: 0,
        };
        if !unsafe {
            AXValueGetValue(
                selection.as_CFTypeRef(),
                AX_VALUE_CF_RANGE,
                &mut range as *mut _ as *mut c_void,
            )
        } {
            return Err(ReadError::Unsupported);
        }
        if range.location < 0
            || range.length < 0
            || range
                .location
                .checked_add(range.length)
                .is_none_or(|end| end > count as isize)
        {
            return Err(ReadError::Unsupported);
        }
        let requested = CFRange {
            location: 0,
            length: count as isize,
        };
        let parameter =
            unsafe { AXValueCreate(AX_VALUE_CF_RANGE, &requested as *const _ as *const c_void) };
        if parameter.is_null() {
            return Err(ReadError::Unsupported);
        }
        let parameter = unsafe { CFType::wrap_under_create_rule(parameter) };
        let name = CFString::new("AXStringForRange");
        let mut raw: CFTypeRef = std::ptr::null();
        #[cfg(test)]
        BODY_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let result = unsafe {
            AXUIElementCopyParameterizedAttributeValue(
                element.0,
                name.as_concrete_TypeRef(),
                parameter.as_CFTypeRef(),
                &mut raw,
            )
        };
        if result != AX_SUCCESS {
            if !raw.is_null() {
                unsafe { CFRelease(raw) };
            }
            return status(result).and(Err(ReadError::NoValue));
        }
        if raw.is_null() {
            return Err(ReadError::NoValue);
        }
        let value = unsafe { CFType::wrap_under_create_rule(raw) };
        if unsafe { CFGetTypeID(value.as_CFTypeRef()) } != CFString::type_id() {
            return Err(ReadError::Unsupported);
        }
        let text = unsafe { CFString::wrap_under_get_rule(value.as_CFTypeRef() as CFStringRef) }
            .to_string();
        if text.encode_utf16().count() != count as usize {
            return Err(ReadError::Unsupported);
        }
        same_focus(*pid, element)?;
        check_control(element)?;
        let composition_after = composition_state(element)?;
        if composition_after == super::CompositionState::Active {
            return Err(ReadError::CompositionActive);
        }
        let mut process_id = 0;
        status(unsafe { AXUIElementGetPid(element.0, &mut process_id) })?;
        if process_id <= 0 {
            return Err(ReadError::Unsupported);
        }
        Ok(OwnedSnapshot {
            target: *token,
            process_id: process_id as u32,
            text,
            selection_utf16: range.location as usize..(range.location + range.length) as usize,
            read_at: Instant::now(),
            event_driven: self.observer.as_ref().is_some_and(|s| s.text_supported),
            composition: if composition_before == composition_after {
                composition_before
            } else {
                super::CompositionState::Unknown
            },
        })
    }
}

fn composition_state(element: &Element) -> Result<super::CompositionState, ReadError> {
    // AXWebConstants.h: non-null means provisional text, successful null means
    // no marked text. Unsupported/NoValue does not prove IME inactivity.
    let name = CFString::new("AXTextInputMarkedTextMarkerRange");
    let mut value = std::ptr::null();
    let code =
        unsafe { AXUIElementCopyAttributeValue(element.0, name.as_concrete_TypeRef(), &mut value) };
    let active = !value.is_null();
    if active {
        unsafe { CFRelease(value) };
    }
    match code {
        AX_SUCCESS => Ok(if active {
            super::CompositionState::Active
        } else {
            super::CompositionState::Inactive
        }),
        -25205 | -25212 => Ok(super::CompositionState::Unknown),
        code => status(code).map(|_| super::CompositionState::Unknown),
    }
}

fn status(code: i32) -> Result<(), ReadError> {
    match code {
        AX_SUCCESS => Ok(()),
        AX_NO_VALUE => Err(ReadError::NoValue),
        AX_DISABLED => Err(ReadError::PermissionDenied),
        _ => Err(ReadError::Native(code)),
    }
}

fn attribute(element: &Element, name: &str) -> Result<CFType, ReadError> {
    let name = CFString::new(name);
    let mut value: CFTypeRef = std::ptr::null();
    let result =
        unsafe { AXUIElementCopyAttributeValue(element.0, name.as_concrete_TypeRef(), &mut value) };
    if result != AX_SUCCESS {
        if !value.is_null() {
            unsafe { CFRelease(value) };
        }
        return status(result).and(Err(ReadError::NoValue));
    }
    if value.is_null() {
        return Err(ReadError::NoValue);
    }
    Ok(unsafe { CFType::wrap_under_create_rule(value) })
}

fn string_attribute(element: &Element, name: &str) -> Result<String, ReadError> {
    let value = attribute(element, name)?;
    if unsafe { CFGetTypeID(value.as_CFTypeRef()) } != CFString::type_id() {
        return Err(ReadError::Unsupported);
    }
    Ok(unsafe { CFString::wrap_under_get_rule(value.as_CFTypeRef() as CFStringRef) }.to_string())
}

fn focused(pid: u32) -> Result<Element, ReadError> {
    same_frontmost(pid)?;
    let application = unsafe { AXUIElementCreateApplication(pid as i32) };
    if application.is_null() {
        return Err(ReadError::Unsupported);
    }
    let application = Element(application);
    status(unsafe { AXUIElementSetMessagingTimeout(application.0, 0.1) })?;
    let value = attribute(&application, "AXFocusedUIElement")?;
    if unsafe { CFGetTypeID(value.as_CFTypeRef()) } != unsafe { AXUIElementGetTypeID() } {
        return Err(ReadError::Unsupported);
    }
    let retained = unsafe { core_foundation::base::CFRetain(value.as_CFTypeRef()) };
    let element = Element(retained as ElementRef);
    status(unsafe { AXUIElementSetMessagingTimeout(element.0, 0.1) })?;
    same_frontmost(pid)?;
    Ok(element)
}

fn same_frontmost(pid: u32) -> Result<(), ReadError> {
    let current = objc2_app_kit::NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .ok_or(ReadError::TargetChanged)?;
    if current.processIdentifier() as u32 != pid {
        return Err(ReadError::TargetChanged);
    }
    Ok(())
}

fn same_focus(pid: u32, element: &Element) -> Result<(), ReadError> {
    let current = focused(pid)?;
    if unsafe { CFEqual(element.0 as CFTypeRef, current.0 as CFTypeRef) } == 0 {
        return Err(ReadError::TargetChanged);
    }
    Ok(())
}

fn check_control(element: &Element) -> Result<(), ReadError> {
    if unsafe { IsSecureEventInputEnabled() } {
        return Err(ReadError::SecureInput);
    }
    let role = string_attribute(element, "AXRole")?;
    if role == "AXSecureTextField" {
        return Err(ReadError::SecureInput);
    }
    if role != "AXTextField" && role != "AXTextArea" {
        return Err(ReadError::Unsupported);
    }
    match string_attribute(element, "AXSubrole") {
        Ok(subrole) if subrole == "AXSecureTextField" => return Err(ReadError::SecureInput),
        // AXSubrole is optional in the AX role contract. An explicit
        // AttributeUnsupported means the generic role has no specialization;
        // NoValue, wrong types, and transient errors do not establish that.
        Ok(_) | Err(ReadError::Native(-25205)) => {}
        Err(ReadError::NoValue) if verified_chromium_web_node(element)? => {}
        Err(ReadError::NoValue | ReadError::Unsupported) => return Err(ReadError::SecurityUnknown),
        Err(error) => return Err(error),
    }
    let attribute = CFString::new("AXValue");
    let mut settable = 0u8;
    status(unsafe {
        AXUIElementIsAttributeSettable(element.0, attribute.as_concrete_TypeRef(), &mut settable)
    })?;
    if settable == 0 {
        return Err(ReadError::Unsupported);
    }
    Ok(())
}

/// Chromium's actual web nodes return a nil subrole for ordinary editors and
/// AXSecureTextField for protected fields. Its incomplete focus proxy does not
/// carry this contract. Require an explicitly adapted provider, a live node ID,
/// and a self-identifying editable ancestor before interpreting nil as absent.
/// No accessibility flags, focus, selection or clipboard are changed here.
fn uses_chromium_web_contract(bundle_id: Option<&str>) -> bool {
    matches!(bundle_id, Some("com.google.Chrome" | "com.openai.codex"))
}

fn verified_chromium_web_node(element: &Element) -> Result<bool, ReadError> {
    let mut pid = 0;
    status(unsafe { AXUIElementGetPid(element.0, &mut pid) })?;
    let app = objc2_app_kit::NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .ok_or(ReadError::TargetChanged)?;
    if app.processIdentifier() != pid {
        return Err(ReadError::TargetChanged);
    }
    let bundle_id = app.bundleIdentifier().map(|id| id.to_string());
    if !uses_chromium_web_contract(bundle_id.as_deref()) {
        return Ok(false);
    }
    let node = match attribute(element, "ChromeAXNodeId") {
        Ok(node) => node,
        Err(ReadError::NoValue | ReadError::Native(-25205)) => return Ok(false),
        Err(error) => return Err(error),
    };
    if unsafe { CFGetTypeID(node.as_CFTypeRef()) } != CFString::type_id() {
        return Ok(false);
    }
    let id =
        unsafe { CFString::wrap_under_get_rule(node.as_CFTypeRef() as CFStringRef) }.to_string();
    if id.parse::<i64>().is_err() {
        return Ok(false);
    }
    let ancestor = match attribute(element, "AXEditableAncestor") {
        Ok(ancestor) => ancestor,
        Err(ReadError::NoValue | ReadError::Native(-25205)) => return Ok(false),
        Err(error) => return Err(error),
    };
    Ok(
        unsafe { CFGetTypeID(ancestor.as_CFTypeRef()) } == unsafe { AXUIElementGetTypeID() }
            && unsafe { CFEqual(ancestor.as_CFTypeRef(), element.0 as CFTypeRef) } != 0,
    )
}

#[cfg(test)]
mod tests {
    use super::uses_chromium_web_contract;

    #[test]
    fn codex_uses_the_explicit_web_node_contract_without_allowing_all_embedded_browsers() {
        assert!(uses_chromium_web_contract(Some("com.google.Chrome")));
        assert!(uses_chromium_web_contract(Some("com.openai.codex")));
        for id in [
            None,
            Some("com.openai.chat"),
            Some("com.microsoft.VSCode"),
            Some("unknown.electron.app"),
        ] {
            assert!(!uses_chromium_web_contract(id));
        }
    }
}
