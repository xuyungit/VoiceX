use std::ffi::c_void;
use std::sync::Arc;
use std::time::Instant;

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{
    CFEqual, CFGetTypeID, CFRange, CFRelease, CFType, CFTypeID, CFTypeRef, TCFType,
};
use core_foundation::number::{CFNumber, CFNumberRef};
use core_foundation::runloop::{
    kCFRunLoopDefaultMode, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRemoveSource,
    CFRunLoopRunInMode, CFRunLoopSourceRef,
};
use core_foundation::string::{CFString, CFStringGetCharacters, CFStringRef};
use objc2::rc::autoreleasepool;

use super::window::ReadWindow;
use super::{EventSignal, OwnedSnapshot, ReadError, DESTROYED, FOCUS_CHANGED, TEXT_CHANGED};

type ElementRef = crate::selection::macos::ax::AXUIElementRef;
type ObserverRef = *const c_void;
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
    fn AXUIElementCopyParameterizedAttributeNames(
        element: ElementRef,
        names: *mut CFArrayRef,
    ) -> i32;
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
    fn AXTextMarkerRangeGetTypeID() -> CFTypeID;
    fn AXTextMarkerRangeCopyStartMarker(range: CFTypeRef) -> CFTypeRef;
    fn AXTextMarkerRangeCopyEndMarker(range: CFTypeRef) -> CFTypeRef;
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

#[cfg(test)]
pub(super) fn fixture_webkit_read(
    pid: u32,
    caret: usize,
    password: bool,
) -> Result<(String, std::ops::Range<usize>), ReadError> {
    // Called only with the PID of the test's freshly spawned synthetic fixture.
    // A supplied caret replaces foreground selection acquisition, so this is
    // an AX range-I/O test, not evidence about real foreground focus behavior.
    let app = Element(unsafe { AXUIElementCreateApplication(pid as i32) });
    status(unsafe { AXUIElementSetMessagingTimeout(app.0, 0.1) })?;
    let windows = attribute(&app, "AXWindows")?;
    let windows: CFArray<CFType> =
        unsafe { CFArray::wrap_under_get_rule(windows.as_CFTypeRef() as CFArrayRef) };
    let mut nodes: Vec<Element> = windows
        .iter()
        .map(
            |v| Element(unsafe { core_foundation::base::CFRetain(v.as_CFTypeRef()) } as ElementRef),
        )
        .collect();
    for _ in 0..64 {
        let Some(node) = nodes.pop() else {
            break;
        };
        status(unsafe { AXUIElementSetMessagingTimeout(node.0, 0.1) })?;
        let role = string_attribute(&node, "AXRole")?;
        if (password && role == "AXTextField") || (!password && role == "AXWebArea") {
            let node = resolve_editable_focus(node, pid)?;
            check_control(&node)?;
            let first = attribute(&node, "AXStartTextMarker")?;
            let last = attribute(&node, "AXEndTextMarker")?;
            let count = native_index(&parameter_attribute(
                &node,
                "AXLengthForTextMarkerRange",
                marker_range(&node, &first, &last)?.as_CFTypeRef(),
            )?)?;
            let marker = parameter_attribute(
                &node,
                "AXTextMarkerForIndex",
                CFNumber::from(caret as i64).as_CFTypeRef(),
            )?;
            let selected = marker_range(&node, &marker, &marker)?;
            let local = read_web_area_selection(
                &node,
                &mut None,
                super::window::CONTEXT_UNITS,
                count,
                &selected,
            )?;
            return Ok((local.text, local.selection_utf16));
        }
        if let Ok(children) = attribute(&node, "AXChildren") {
            let children: CFArray<CFType> =
                unsafe { CFArray::wrap_under_get_rule(children.as_CFTypeRef() as CFArrayRef) };
            nodes.extend(children.iter().map(|v| {
                Element(unsafe { core_foundation::base::CFRetain(v.as_CFTypeRef()) } as ElementRef)
            }));
        }
    }
    Err(ReadError::NoValue)
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
    window: Option<ReadWindow>,
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
        self.window = None;
    }
    pub(super) fn new() -> Self {
        Self {
            target: None,
            window: None,
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
        lookbehind: usize,
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
            self.read_inner(generation, lookbehind)
        })
    }

    pub(super) fn read(&mut self, generation: u64) -> Result<OwnedSnapshot, ReadError> {
        autoreleasepool(|_| self.read_inner(generation, super::window::CONTEXT_UNITS))
    }

    fn read_inner(
        &mut self,
        generation: u64,
        lookbehind: usize,
    ) -> Result<OwnedSnapshot, ReadError> {
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
        let local = if string_attribute(element, "AXRole")? == "AXWebArea" {
            read_web_area(element, &mut self.window, lookbehind)?
        } else {
            read_text_control(element, &mut self.window, lookbehind)?
        };
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
            text: local.text,
            selection_utf16: local.selection_utf16,
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

struct LocalText {
    text: String,
    selection_utf16: std::ops::Range<usize>,
}

fn read_text_control(
    element: &Element,
    window_slot: &mut Option<ReadWindow>,
    lookbehind: usize,
) -> Result<LocalText, ReadError> {
    let count_value = attribute(element, "AXNumberOfCharacters")?;
    if unsafe { CFGetTypeID(count_value.as_CFTypeRef()) } != CFNumber::type_id() {
        return Err(ReadError::Unsupported);
    }
    let count = unsafe { CFNumber::wrap_under_get_rule(count_value.as_CFTypeRef() as CFNumberRef) }
        .to_i64()
        .ok_or(ReadError::Unsupported)?;
    if count < 0 || count > isize::MAX as i64 {
        return Err(ReadError::Unsupported);
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
    let selection = range.location as usize..(range.location + range.length) as usize;
    if window_slot.is_none() {
        *window_slot = Some(ReadWindow::new(
            count as usize,
            selection.clone(),
            lookbehind,
        )?);
    }
    let window = window_slot.as_mut().unwrap();
    let mut bounds = window.range(count as usize)?;
    if selection.start < bounds.start || selection.end > bounds.end {
        return Err(ReadError::WindowChanged);
    }
    let requested = CFRange {
        location: bounds.start as isize,
        length: bounds.len() as isize,
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
    let native_text = unsafe { CFString::wrap_under_get_rule(value.as_CFTypeRef() as CFStringRef) };
    if native_text.char_len() != bounds.len() as isize {
        return Err(ReadError::Unsupported);
    }
    let mut units = vec![0u16; bounds.len()];
    unsafe {
        CFStringGetCharacters(
            native_text.as_concrete_TypeRef(),
            CFRange {
                location: 0,
                length: units.len() as isize,
            },
            units.as_mut_ptr(),
        );
    }
    let text = window.decode(&units, &mut bounds)?;
    if selection.start < bounds.start || selection.end > bounds.end {
        return Err(ReadError::WindowChanged);
    }
    let after_count = attribute(element, "AXNumberOfCharacters")?;
    if unsafe { CFGetTypeID(after_count.as_CFTypeRef()) } != CFNumber::type_id()
        || unsafe { CFNumber::wrap_under_get_rule(after_count.as_CFTypeRef() as CFNumberRef) }
            .to_i64()
            != Some(count)
    {
        return Err(ReadError::WindowChanged);
    }
    window.check(&text)?;
    Ok(LocalText {
        text,
        selection_utf16: selection.start - bounds.start..selection.end - bounds.start,
    })
}

fn native_index(value: &CFType) -> Result<usize, ReadError> {
    if unsafe { CFGetTypeID(value.as_CFTypeRef()) } != CFNumber::type_id() {
        return Err(ReadError::Unsupported);
    }
    let index = unsafe { CFNumber::wrap_under_get_rule(value.as_CFTypeRef() as CFNumberRef) }
        .to_i64()
        .filter(|i| *i >= 0 && *i < isize::MAX as i64)
        .ok_or(ReadError::NoValue)?;
    Ok(index as usize)
}

fn marker_range(element: &Element, start: &CFType, end: &CFType) -> Result<CFType, ReadError> {
    let markers = CFArray::from_CFTypes(&[start.clone(), end.clone()]);
    parameter_attribute(
        element,
        "AXTextMarkerRangeForUnorderedTextMarkers",
        markers.as_CFTypeRef(),
    )
}

/// Inspect only metadata on the caret/selection path. A web page focus must
/// never bypass a protected descendant's role before any text is requested.
fn check_marker_path(root: &Element, marker: &CFType) -> Result<(), ReadError> {
    let owner = parameter_attribute(root, "AXUIElementForTextMarker", marker.as_CFTypeRef())?;
    if unsafe { CFGetTypeID(owner.as_CFTypeRef()) } != unsafe { AXUIElementGetTypeID() } {
        return Err(ReadError::SecurityUnknown);
    }
    let node =
        Element(unsafe { core_foundation::base::CFRetain(owner.as_CFTypeRef()) } as ElementRef);
    check_element_path(root, node)
}

fn check_element_path(root: &Element, mut node: Element) -> Result<(), ReadError> {
    let mut expected_pid = 0;
    status(unsafe { AXUIElementGetPid(root.0, &mut expected_pid) })?;
    for _ in 0..32 {
        status(unsafe { AXUIElementSetMessagingTimeout(node.0, 0.1) })?;
        if unsafe { CFEqual(root.0 as CFTypeRef, node.0 as CFTypeRef) } != 0 {
            return Ok(());
        }
        let mut pid = 0;
        status(unsafe { AXUIElementGetPid(node.0, &mut pid) })?;
        if pid != expected_pid {
            return Err(ReadError::TargetChanged);
        }
        if string_attribute(&node, "AXRole")? == "AXSecureTextField" {
            return Err(ReadError::SecureInput);
        }
        match string_attribute(&node, "AXSubrole") {
            Ok(role) if role == "AXSecureTextField" => return Err(ReadError::SecureInput),
            Ok(_) | Err(ReadError::NoValue | ReadError::Native(-25205)) => {}
            Err(error) => return Err(error),
        }
        let parent = attribute(&node, "AXParent")?;
        if unsafe { CFGetTypeID(parent.as_CFTypeRef()) } != unsafe { AXUIElementGetTypeID() } {
            return Err(ReadError::SecurityUnknown);
        }
        node = Element(
            unsafe { core_foundation::base::CFRetain(parent.as_CFTypeRef()) } as ElementRef,
        );
    }
    Err(ReadError::SecurityUnknown)
}

fn marker_text_units(
    element: &Element,
    start: &CFType,
    end: &CFType,
) -> Result<Vec<u16>, ReadError> {
    let range = marker_range(element, start, end)?;
    #[cfg(test)]
    BODY_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let text = parameter_attribute(element, "AXStringForTextMarkerRange", range.as_CFTypeRef())?;
    if unsafe { CFGetTypeID(text.as_CFTypeRef()) } != CFString::type_id() {
        return Err(ReadError::Unsupported);
    }
    let text = unsafe { CFString::wrap_under_get_rule(text.as_CFTypeRef() as CFStringRef) };
    if text.char_len() < 0 || text.char_len() as usize > super::window::MAX_UNITS {
        return Err(ReadError::TooLarge);
    }
    let mut units = vec![0; text.char_len() as usize];
    unsafe {
        CFStringGetCharacters(
            text.as_concrete_TypeRef(),
            CFRange {
                location: 0,
                length: units.len() as isize,
            },
            units.as_mut_ptr(),
        );
    }
    Ok(units)
}

fn read_web_area(
    element: &Element,
    window_slot: &mut Option<ReadWindow>,
    lookbehind: usize,
) -> Result<LocalText, ReadError> {
    let first = attribute(element, "AXStartTextMarker")?;
    let last = attribute(element, "AXEndTextMarker")?;
    let document = marker_range(element, &first, &last)?;
    let count = native_index(&parameter_attribute(
        element,
        "AXLengthForTextMarkerRange",
        document.as_CFTypeRef(),
    )?)?;
    if count == 0 {
        if window_slot.is_none() {
            *window_slot = Some(ReadWindow::new(0, 0..0, lookbehind)?);
        }
        return Ok(LocalText {
            text: String::new(),
            selection_utf16: 0..0,
        });
    }
    let selected = attribute(element, "AXSelectedTextMarkerRange")?;
    read_web_area_selection(element, window_slot, lookbehind, count, &selected)
}

fn read_web_area_selection(
    element: &Element,
    window_slot: &mut Option<ReadWindow>,
    lookbehind: usize,
    count: usize,
    selected: &CFType,
) -> Result<LocalText, ReadError> {
    if unsafe { CFGetTypeID(selected.as_CFTypeRef()) } != unsafe { AXTextMarkerRangeGetTypeID() } {
        return Err(ReadError::Unsupported);
    }
    let start = unsafe { AXTextMarkerRangeCopyStartMarker(selected.as_CFTypeRef()) };
    if start.is_null() {
        return Err(ReadError::NoValue);
    }
    let start = unsafe { CFType::wrap_under_create_rule(start) };
    let end = unsafe { AXTextMarkerRangeCopyEndMarker(selected.as_CFTypeRef()) };
    if end.is_null() {
        return Err(ReadError::NoValue);
    }
    let end = unsafe { CFType::wrap_under_create_rule(end) };
    check_marker_path(element, &start)?;
    check_marker_path(element, &end)?;
    let selection = native_index(&parameter_attribute(
        element,
        "AXRelativeIndexForTextMarker",
        start.as_CFTypeRef(),
    )?)?
        ..native_index(&parameter_attribute(
            element,
            "AXRelativeIndexForTextMarker",
            end.as_CFTypeRef(),
        )?)?;
    if window_slot.is_none() {
        *window_slot = Some(ReadWindow::new(count, selection.clone(), lookbehind)?);
    }
    let window = window_slot.as_mut().unwrap();
    let mut bounds = window.range(count)?;
    if selection.start > selection.end
        || selection.start < bounds.start
        || selection.end > bounds.end
    {
        return Err(ReadError::WindowChanged);
    }
    let marker_at = |index: usize| {
        parameter_attribute(
            element,
            "AXTextMarkerForIndex",
            CFNumber::from(index as i64).as_CFTypeRef(),
        )
    };
    let window_start = marker_at(bounds.start)?;
    let window_end = marker_at(bounds.end)?;
    let units = marker_text_units(element, &window_start, &window_end)?;
    let text = window.decode(&units, &mut bounds)?;
    if selection.start < bounds.start || selection.end > bounds.end {
        return Err(ReadError::WindowChanged);
    }
    let window_start = marker_at(bounds.start)?;
    let prefix_start = String::from_utf16(&marker_text_units(element, &window_start, &start)?)
        .map_err(|_| ReadError::WindowChanged)?;
    let prefix_end = String::from_utf16(&marker_text_units(element, &window_start, &end)?)
        .map_err(|_| ReadError::WindowChanged)?;
    if !text.starts_with(&prefix_start)
        || !text.starts_with(&prefix_end)
        || !prefix_end.starts_with(&prefix_start)
    {
        return Err(ReadError::WindowChanged);
    }
    let after_count = native_index(&parameter_attribute(
        element,
        "AXLengthForTextMarkerRange",
        marker_range(
            element,
            &attribute(element, "AXStartTextMarker")?,
            &attribute(element, "AXEndTextMarker")?,
        )?
        .as_CFTypeRef(),
    )?)?;
    if count != after_count {
        return Err(ReadError::WindowChanged);
    }
    window.check(&text)?;
    Ok(LocalText {
        text,
        selection_utf16: prefix_start.encode_utf16().count()..prefix_end.encode_utf16().count(),
    })
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
    let frontmost = frontmost_app(pid)?;
    let application = unsafe { AXUIElementCreateApplication(pid as i32) };
    if application.is_null() {
        return Err(ReadError::Unsupported);
    }
    let application = Element(application);
    status(unsafe { AXUIElementSetMessagingTimeout(application.0, 0.1) })?;
    if has_electron_framework(&frontmost) {
        // Electron's supported lightweight activation path: querying the app's
        // role requests native AX APIs. Focus alone can stay an empty proxy.
        // This does not toggle the full screen-reader mode or scan other fields.
        string_attribute(&application, "AXRole")?;
    }
    let value = attribute(&application, "AXFocusedUIElement")?;
    if unsafe { CFGetTypeID(value.as_CFTypeRef()) } != unsafe { AXUIElementGetTypeID() } {
        return Err(ReadError::Unsupported);
    }
    let retained = unsafe { core_foundation::base::CFRetain(value.as_CFTypeRef()) };
    let element = Element(retained as ElementRef);
    status(unsafe { AXUIElementSetMessagingTimeout(element.0, 0.1) })?;
    same_frontmost(pid)?;
    resolve_editable_focus(element, pid)
}

fn reject_protected(element: &Element) -> Result<(), ReadError> {
    if string_attribute(element, "AXRole")? == "AXSecureTextField" {
        return Err(ReadError::SecureInput);
    }
    match string_attribute(element, "AXSubrole") {
        Ok(role) if role == "AXSecureTextField" => Err(ReadError::SecureInput),
        Ok(_) | Err(ReadError::NoValue | ReadError::Native(-25205)) => Ok(()),
        Err(error) => Err(error),
    }
}

fn resolve_editable_focus(element: Element, pid: u32) -> Result<Element, ReadError> {
    reject_protected(&element)?;
    let ancestor = match attribute(&element, "AXHighestEditableAncestor") {
        Ok(value) => value,
        Err(ReadError::NoValue | ReadError::Native(-25205)) => return Ok(element),
        Err(error) => return Err(error),
    };
    if unsafe { CFGetTypeID(ancestor.as_CFTypeRef()) } != unsafe { AXUIElementGetTypeID() } {
        return Err(ReadError::Unsupported);
    }
    let ancestor =
        Element(unsafe { core_foundation::base::CFRetain(ancestor.as_CFTypeRef()) } as ElementRef);
    status(unsafe { AXUIElementSetMessagingTimeout(ancestor.0, 0.1) })?;
    let mut owner = 0;
    status(unsafe { AXUIElementGetPid(ancestor.0, &mut owner) })?;
    if owner as u32 != pid {
        return Err(ReadError::TargetChanged);
    }
    reject_protected(&ancestor)?;
    check_element_path(&ancestor, element)?;
    Ok(ancestor)
}

fn same_frontmost(pid: u32) -> Result<(), ReadError> {
    frontmost_app(pid).map(|_| ())
}

fn frontmost_app(
    pid: u32,
) -> Result<objc2::rc::Retained<objc2_app_kit::NSRunningApplication>, ReadError> {
    let current = objc2_app_kit::NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .ok_or(ReadError::TargetChanged)?;
    if current.processIdentifier() as u32 != pid {
        return Err(ReadError::TargetChanged);
    }
    Ok(current)
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
    let web_area = role == "AXWebArea" && verified_webkit_text_node(element)?;
    if role != "AXTextField" && role != "AXTextArea" && !web_area {
        return Err(ReadError::Unsupported);
    }
    match string_attribute(element, "AXSubrole") {
        Ok(subrole) if subrole == "AXSecureTextField" => return Err(ReadError::SecureInput),
        // AXSubrole is optional in the AX role contract. An explicit
        // AttributeUnsupported means the generic role has no specialization;
        // NoValue, wrong types, and transient errors do not establish that.
        Ok(_) | Err(ReadError::Native(-25205)) => {}
        Err(ReadError::NoValue)
            if web_area
                || verified_chromium_web_node(element)?
                || verified_webkit_text_node(element)? => {}
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
/// carry this contract. Require a verified Chromium provider, a live node ID,
/// and a self-identifying editable ancestor before interpreting nil as absent.
/// No accessibility flags, focus, selection or clipboard are changed here.
fn uses_chromium_web_contract(bundle_id: Option<&str>, electron_framework: bool) -> bool {
    electron_framework || matches!(bundle_id, Some("com.google.Chrome" | "com.openai.codex"))
}

fn webkit_text_contract(marker_is_valid: bool, parameters: &[String]) -> bool {
    marker_is_valid
        && [
            "AXTextMarkerForIndex",
            "AXTextMarkerIsValid",
            "AXStringForRange",
        ]
        .iter()
        .all(|required| parameters.iter().any(|name| name == required))
}

fn parameter_attribute(
    element: &Element,
    name: &str,
    parameter: CFTypeRef,
) -> Result<CFType, ReadError> {
    let name = CFString::new(name);
    let mut value: CFTypeRef = std::ptr::null();
    let code = unsafe {
        AXUIElementCopyParameterizedAttributeValue(
            element.0,
            name.as_concrete_TypeRef(),
            parameter,
            &mut value,
        )
    };
    if code != AX_SUCCESS {
        if !value.is_null() {
            unsafe { CFRelease(value) };
        }
        return status(code).and(Err(ReadError::NoValue));
    }
    if value.is_null() {
        return Err(ReadError::NoValue);
    }
    Ok(unsafe { CFType::wrap_under_create_rule(value) })
}

/// WebKit ordinary text controls return a nil subrole; protected fields return
/// AXSecureTextField. Prove the live marker/range contract without app names or
/// field text. An advertised attribute alone is insufficient.
fn verified_webkit_text_node(element: &Element) -> Result<bool, ReadError> {
    let mut raw = std::ptr::null();
    let code = unsafe { AXUIElementCopyParameterizedAttributeNames(element.0, &mut raw) };
    if code == -25205 {
        return Ok(false);
    }
    status(code)?;
    if raw.is_null() {
        return Ok(false);
    }
    let names: CFArray<CFString> = unsafe { CFArray::wrap_under_create_rule(raw) };
    let names = names
        .iter()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    if !webkit_text_contract(true, &names) {
        return Ok(false);
    }
    let zero = CFNumber::from(0i32);
    let marker = parameter_attribute(element, "AXTextMarkerForIndex", zero.as_CFTypeRef())?;
    let valid = parameter_attribute(element, "AXTextMarkerIsValid", marker.as_CFTypeRef())?;
    if unsafe { CFGetTypeID(valid.as_CFTypeRef()) }
        != core_foundation::boolean::CFBoolean::type_id()
    {
        return Ok(false);
    }
    let valid = unsafe {
        core_foundation::boolean::CFBoolean::wrap_under_get_rule(valid.as_CFTypeRef() as _)
    };
    Ok(webkit_text_contract(bool::from(valid), &names))
}

fn has_electron_framework(app: &objc2_app_kit::NSRunningApplication) -> bool {
    app.bundleURL()
        .and_then(|url| url.path())
        .is_some_and(|path| {
            std::path::Path::new(&path.to_string())
                .join("Contents/Frameworks/Electron Framework.framework")
                .is_dir()
        })
}

fn verified_chromium_web_node(element: &Element) -> Result<bool, ReadError> {
    let mut pid = 0;
    status(unsafe { AXUIElementGetPid(element.0, &mut pid) })?;
    let app = frontmost_app(pid as u32)?;
    let bundle_id = app.bundleIdentifier().map(|id| id.to_string());
    if !uses_chromium_web_contract(bundle_id.as_deref(), has_electron_framework(&app)) {
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
    use super::{uses_chromium_web_contract, webkit_text_contract};

    #[test]
    fn webkit_requires_a_valid_marker_and_the_full_range_contract() {
        let names = [
            "AXTextMarkerForIndex",
            "AXTextMarkerIsValid",
            "AXStringForRange",
        ]
        .map(String::from);
        assert!(webkit_text_contract(true, &names));
        assert!(!webkit_text_contract(false, &names));
        for missing in 0..names.len() {
            let subset: Vec<_> = names
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != missing)
                .map(|(_, name)| name.clone())
                .collect();
            assert!(!webkit_text_contract(true, &subset));
        }
    }

    #[test]
    fn chromium_contract_requires_a_known_browser_or_an_installed_electron_framework() {
        assert!(uses_chromium_web_contract(Some("com.google.Chrome"), false));
        assert!(uses_chromium_web_contract(Some("com.openai.codex"), false));
        for id in [
            None,
            Some("com.openai.chat"),
            Some("com.microsoft.VSCode"),
            Some("unknown.electron.app"),
        ] {
            assert!(!uses_chromium_web_contract(id, false));
        }
        for id in [
            "com.anysphere.sand",
            "com.tencent.workbuddy.mac",
            "com.deepseek.dsh",
            "unknown.electron.app",
        ] {
            assert!(!uses_chromium_web_contract(Some(id), false));
            assert!(uses_chromium_web_contract(Some(id), true));
        }
    }
}
