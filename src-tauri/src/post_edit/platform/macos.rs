use std::cell::Cell;
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

thread_local! {
    static ATTRIBUTE_QUERIES: Cell<u64> = const { Cell::new(0) };
    #[cfg(test)]
    static FRAMEWORK_CHECKS: Cell<u64> = const { Cell::new(0) };
    #[cfg(test)]
    static CAPABILITY_CHECKS: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
#[derive(Debug)]
pub(super) struct ReadMeasurement {
    pub operation: &'static str,
    pub attribute_queries: u64,
    pub elapsed_us: u128,
    pub framework_checks: u64,
    pub capability_checks: u64,
}
#[cfg(test)]
static MEASUREMENTS: std::sync::Mutex<Vec<ReadMeasurement>> = std::sync::Mutex::new(Vec::new());
#[cfg(test)]
pub(super) fn take_measurements() -> Vec<ReadMeasurement> {
    std::mem::take(&mut *MEASUREMENTS.lock().unwrap())
}

fn attribute_query() {
    ATTRIBUTE_QUERIES.with(|count| count.set(count.get() + 1));
}

fn measure_queries<T>(
    operation: &'static str,
    read: impl FnOnce() -> Result<T, ReadError>,
) -> Result<T, ReadError> {
    let before = ATTRIBUTE_QUERIES.with(Cell::get);
    #[cfg(test)]
    let (framework_before, capabilities_before) = (
        FRAMEWORK_CHECKS.with(Cell::get),
        CAPABILITY_CHECKS.with(Cell::get),
    );
    let at = Instant::now();
    let result = read();
    #[cfg(test)]
    MEASUREMENTS.lock().unwrap().push(ReadMeasurement {
        operation,
        attribute_queries: ATTRIBUTE_QUERIES.with(Cell::get) - before,
        elapsed_us: at.elapsed().as_micros(),
        framework_checks: FRAMEWORK_CHECKS.with(Cell::get) - framework_before,
        capability_checks: CAPABILITY_CHECKS.with(Cell::get) - capabilities_before,
    });
    log::debug!(
        "Post-edit AX operation: operation={}, attribute_queries={}, elapsed_us={}, outcome={:?}",
        operation,
        ATTRIBUTE_QUERIES.with(Cell::get) - before,
        at.elapsed().as_micros(),
        result.as_ref().map(|_| ()).map_err(|e| *e)
    );
    result
}

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
    target: Option<Target>,
    window: Option<ReadWindow>,
    observer: Option<Subscription>,
    events: Arc<EventSignal>,
}

struct Target {
    generation: u64,
    token: u64,
    pid: u32,
    application: Element,
    element: Element,
    kind: TextKind,
    composition: CompositionReader,
    chromium_text: bool,
}

#[derive(Clone, Copy)]
enum TextKind {
    Control,
    WebArea,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompositionReader {
    NativeRange,
    WebMarkers,
    Both,
    Unknown,
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
        measure_queries("capture", || {
            autoreleasepool(|_| {
                self.clear();
                let (application, chromium) = capture_application(expected_pid)?;
                let focused =
                    resolve_editable_focus(focused_raw(&application, expected_pid)?, expected_pid)?;
                let mut pid = 0;
                status(unsafe { AXUIElementGetPid(focused.0, &mut pid) })?;
                if pid <= 0 || pid as u32 != expected_pid {
                    return Err(ReadError::TargetChanged);
                }
                let kind = check_control(&focused, chromium)?;
                let composition = probe_composition_reader(&focused)?;
                let chromium_text = chromium && verified_chromium_web_node(&focused)?;
                self.observer = match Subscription::new(
                    expected_pid,
                    &focused,
                    generation,
                    Arc::clone(&self.events),
                ) {
                    Ok(subscription) if !subscription.registrations.is_empty() => {
                        Some(subscription)
                    }
                    Ok(_) | Err(ReadError::Native(-25207 | -25208)) => None,
                    Err(error) => return Err(error),
                };
                self.target = Some(Target {
                    generation,
                    token: super::target_token(),
                    pid: expected_pid,
                    application,
                    element: focused,
                    kind,
                    composition,
                    chromium_text,
                });
                self.read_inner(generation, lookbehind)
            })
        })
    }

    pub(super) fn read(&mut self, generation: u64) -> Result<OwnedSnapshot, ReadError> {
        measure_queries("read", || {
            autoreleasepool(|_| self.read_inner(generation, super::window::CONTEXT_UNITS))
        })
    }

    pub(super) fn reframe(
        &mut self,
        generation: u64,
        target: u64,
        lookbehind: usize,
    ) -> Result<OwnedSnapshot, ReadError> {
        measure_queries("reframe", || {
            autoreleasepool(|_| {
                let stored = self.target.as_ref().ok_or(ReadError::TargetChanged)?;
                if stored.generation != generation {
                    return Err(ReadError::StaleGeneration);
                }
                if stored.token != target {
                    return Err(ReadError::TargetChanged);
                }
                self.window = None;
                // read_inner checks the same retained native element before text.
                self.read_inner(generation, lookbehind)
            })
        })
    }

    fn read_inner(
        &mut self,
        generation: u64,
        lookbehind: usize,
    ) -> Result<OwnedSnapshot, ReadError> {
        let target = self.target.as_ref().ok_or(ReadError::TargetChanged)?;
        if target.generation != generation {
            return Err(ReadError::StaleGeneration);
        }
        let element = &target.element;
        same_focus(&target.application, target.pid, element)?;
        check_live_control(element, target.kind)?;
        let composition_before = composition_state(element, target.composition)?;
        if composition_before == super::CompositionState::Active {
            return Err(ReadError::CompositionActive);
        }
        let local = match target.kind {
            TextKind::WebArea => read_web_area(element, &mut self.window, lookbehind)?,
            TextKind::Control => read_text_control(element, &mut self.window, lookbehind)?,
        };
        same_focus(&target.application, target.pid, element)?;
        check_live_control(element, target.kind)?;
        let composition_after = composition_state(element, target.composition)?;
        if composition_after == super::CompositionState::Active {
            return Err(ReadError::CompositionActive);
        }
        if target.chromium_text
            && super::empty_caret_prefix(&local.text, &local.selection_utf16)
            && non_editable_text_only(element)?
        {
            return Err(ReadError::FieldCleared);
        }
        Ok(OwnedSnapshot {
            target: target.token,
            process_id: target.pid,
            text: local.text,
            selection_utf16: local.selection_utf16,
            read_at: Instant::now(),
            event_driven: self.observer.as_ref().is_some_and(|s| s.text_supported),
            composition: if composition_before == composition_after {
                composition_before
            } else {
                super::CompositionState::Unknown
            },
            reset_detection: target.chromium_text,
        })
    }
}

/// Chromium exposes inherited editability through whether a text node's
/// AXSelectedTextRange is settable. A contenteditable=false hint can occur in
/// AXStringForRange even though the editor contains no editable text. Inspect
/// metadata only, and require positive evidence for every text leaf. An
/// incomplete/large tree is not evidence of a reset.
fn non_editable_text_only(root: &Element) -> Result<bool, ReadError> {
    const MAX_NODES: usize = 64;
    let mut expected_pid = 0;
    status(unsafe { AXUIElementGetPid(root.0, &mut expected_pid) })?;
    let mut pending = vec![Element(
        unsafe { core_foundation::base::CFRetain(root.0 as CFTypeRef) } as ElementRef,
    )];
    let mut visited = 0;
    let mut saw_text = false;
    while let Some(node) = pending.pop() {
        visited += 1;
        if visited > MAX_NODES {
            return Ok(false);
        }
        // Children do not inherit the bounded timeout; a stalled renderer
        // would otherwise hold the single worker for the system default.
        status(unsafe { AXUIElementSetMessagingTimeout(node.0, 0.1) })?;
        let mut pid = 0;
        status(unsafe { AXUIElementGetPid(node.0, &mut pid) })?;
        if pid != expected_pid {
            return Ok(false);
        }
        let role = reject_protected(&node)?;
        if role == "AXStaticText" {
            let mut settable = 0u8;
            attribute_query();
            status(unsafe {
                AXUIElementIsAttributeSettable(
                    node.0,
                    CFString::new("AXSelectedTextRange").as_concrete_TypeRef(),
                    &mut settable,
                )
            })?;
            if settable == 0 {
                saw_text = true;
                continue;
            }
            // Editable text counts unless it is only a zero-width caret
            // anchor, which Slate keeps before the hint of an empty editor.
            match string_attribute(&node, "AXValue") {
                Ok(value) if value.chars().all(super::super::range::is_blank) => continue,
                Ok(_) | Err(ReadError::NoValue | ReadError::Native(-25205)) => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        if node.0 != root.0
            && matches!(
                role.as_str(),
                "AXTextField" | "AXTextArea" | "AXComboBox" | "AXWebArea"
            )
        {
            return Ok(false);
        }
        let children = match attribute(&node, "AXChildren") {
            Ok(children) => children,
            Err(ReadError::NoValue | ReadError::Native(-25205)) => return Ok(false),
            Err(error) => return Err(error),
        };
        if unsafe { CFGetTypeID(children.as_CFTypeRef()) } != CFArray::<CFType>::type_id() {
            return Err(ReadError::Unsupported);
        }
        let children: CFArray<CFType> =
            unsafe { CFArray::wrap_under_get_rule(children.as_CFTypeRef() as CFArrayRef) };
        if visited + pending.len() + children.len() as usize > MAX_NODES {
            return Ok(false);
        }
        for child in children.iter() {
            if unsafe { CFGetTypeID(child.as_CFTypeRef()) } != unsafe { AXUIElementGetTypeID() } {
                return Err(ReadError::Unsupported);
            }
            pending.push(Element(
                unsafe { core_foundation::base::CFRetain(child.as_CFTypeRef()) } as ElementRef,
            ));
        }
    }
    Ok(saw_text)
}

struct LocalText {
    text: String,
    selection_utf16: std::ops::Range<usize>,
}

fn read_selected_range(
    element: &Element,
    count: usize,
) -> Result<std::ops::Range<usize>, ReadError> {
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
    Ok(range.location as usize..(range.location + range.length) as usize)
}

fn read_text_control_value(
    element: &Element,
    window_slot: &mut Option<ReadWindow>,
    lookbehind: usize,
) -> Result<LocalText, ReadError> {
    #[cfg(test)]
    BODY_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let value = attribute(element, "AXValue")?;
    if unsafe { CFGetTypeID(value.as_CFTypeRef()) } != CFString::type_id() {
        return Err(ReadError::Unsupported);
    }
    let native_text = unsafe { CFString::wrap_under_get_rule(value.as_CFTypeRef() as CFStringRef) };
    if native_text.char_len() < 0 || native_text.char_len() as usize > super::window::MAX_UNITS {
        return Err(ReadError::TooLarge);
    }
    let count = native_text.char_len() as usize;
    let (selection, has_selection) = match read_selected_range(element, count) {
        Ok(range) => (range, true),
        Err(ReadError::NoValue | ReadError::Native(-25205)) => (0..0, count == 0),
        Err(error) => return Err(error),
    };
    if window_slot.is_none() {
        *window_slot = Some(ReadWindow::new(
            count,
            selection.clone(),
            if has_selection { lookbehind } else { count },
        )?);
    }
    let window = window_slot.as_mut().unwrap();
    let mut bounds = window.range(count)?;
    if has_selection && (selection.start < bounds.start || selection.end > bounds.end) {
        return Err(ReadError::WindowChanged);
    }
    let mut units = vec![0u16; bounds.len()];
    unsafe {
        CFStringGetCharacters(
            native_text.as_concrete_TypeRef(),
            CFRange {
                location: bounds.start as isize,
                length: bounds.len() as isize,
            },
            units.as_mut_ptr(),
        );
    }
    let text = window.decode(&units, &mut bounds)?;
    if has_selection && (selection.start < bounds.start || selection.end > bounds.end) {
        return Err(ReadError::WindowChanged);
    }
    window.check(&text)?;
    Ok(LocalText {
        text,
        selection_utf16: if has_selection {
            selection.start - bounds.start..selection.end - bounds.start
        } else {
            usize::MAX..0
        },
    })
}

fn read_text_control(
    element: &Element,
    window_slot: &mut Option<ReadWindow>,
    lookbehind: usize,
) -> Result<LocalText, ReadError> {
    let count_value = match attribute(element, "AXNumberOfCharacters") {
        Ok(value) => value,
        Err(ReadError::Native(-25205)) => {
            return read_text_control_value(element, window_slot, lookbehind);
        }
        Err(error) => return Err(error),
    };
    if unsafe { CFGetTypeID(count_value.as_CFTypeRef()) } != CFNumber::type_id() {
        return Err(ReadError::Unsupported);
    }
    let count = unsafe { CFNumber::wrap_under_get_rule(count_value.as_CFTypeRef() as CFNumberRef) }
        .to_i64()
        .ok_or(ReadError::Unsupported)?;
    if count < 0 || count > isize::MAX as i64 {
        return Err(ReadError::Unsupported);
    }
    let selection = read_selected_range(element, count as usize)?;
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
    attribute_query();
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
        if result == -25205 {
            return read_text_control_value(element, window_slot, lookbehind);
        }
        #[cfg(test)]
        BODY_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        return status(result).and(Err(ReadError::NoValue));
    }
    #[cfg(test)]
    BODY_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
    let collapsed = unsafe { CFEqual(start.as_CFTypeRef(), end.as_CFTypeRef()) } != 0;
    if !collapsed {
        check_marker_path(element, &end)?;
    }
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
    let prefix_end = if collapsed {
        prefix_start.clone()
    } else {
        String::from_utf16(&marker_text_units(element, &window_start, &end)?)
            .map_err(|_| ReadError::WindowChanged)?
    };
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

fn probe_composition_reader(element: &Element) -> Result<CompositionReader, ReadError> {
    let mut supported = [false; 2];
    for (index, reader) in [
        CompositionReader::NativeRange,
        CompositionReader::WebMarkers,
    ]
    .into_iter()
    .enumerate()
    {
        match read_composition(element, reader) {
            Ok(_) => supported[index] = true,
            Err(ReadError::Native(-25205)) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(match supported {
        [true, true] => CompositionReader::Both,
        [true, false] => CompositionReader::NativeRange,
        [false, true] => CompositionReader::WebMarkers,
        [false, false] => CompositionReader::Unknown,
    })
}

fn merge_composition(
    a: super::CompositionState,
    b: super::CompositionState,
) -> super::CompositionState {
    use super::CompositionState::*;
    match (a, b) {
        (Active, _) | (_, Active) => Active,
        (Inactive, _) | (_, Inactive) => Inactive,
        _ => Unknown,
    }
}

fn composition_state(
    element: &Element,
    reader: CompositionReader,
) -> Result<super::CompositionState, ReadError> {
    match read_composition(element, reader) {
        Err(ReadError::Native(-25205)) => Ok(super::CompositionState::Unknown),
        result => result,
    }
}

fn marked_range_state(location: isize, length: isize) -> super::CompositionState {
    if location < 0 || length < 0 || (location == isize::MAX && length != 0) {
        super::CompositionState::Unknown
    } else if length == 0 {
        super::CompositionState::Inactive
    } else {
        super::CompositionState::Active
    }
}

fn read_composition(
    element: &Element,
    reader: CompositionReader,
) -> Result<super::CompositionState, ReadError> {
    let name = CFString::new(match reader {
        CompositionReader::NativeRange => "AXTextInputMarkedRange",
        CompositionReader::WebMarkers => "AXTextInputMarkedTextMarkerRange",
        CompositionReader::Both => {
            return Ok(merge_composition(
                composition_state(element, CompositionReader::NativeRange)?,
                composition_state(element, CompositionReader::WebMarkers)?,
            ))
        }
        CompositionReader::Unknown => return Ok(super::CompositionState::Unknown),
    });
    attribute_query();
    let mut value = std::ptr::null();
    let code =
        unsafe { AXUIElementCopyAttributeValue(element.0, name.as_concrete_TypeRef(), &mut value) };
    let value = (!value.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(value) });
    match code {
        AX_SUCCESS if reader == CompositionReader::WebMarkers => {
            // AXWebConstants.h explicitly defines a successful null as idle.
            Ok(if value.is_some() {
                super::CompositionState::Active
            } else {
                super::CompositionState::Inactive
            })
        }
        AX_SUCCESS => {
            let Some(value) = value else {
                return Ok(super::CompositionState::Unknown);
            };
            if unsafe { CFGetTypeID(value.as_CFTypeRef()) } != unsafe { AXValueGetTypeID() } {
                return Err(ReadError::Unsupported);
            }
            let mut range = CFRange {
                location: 0,
                length: 0,
            };
            if !unsafe {
                AXValueGetValue(
                    value.as_CFTypeRef(),
                    AX_VALUE_CF_RANGE,
                    &mut range as *mut _ as *mut c_void,
                )
            } {
                return Err(ReadError::Unsupported);
            }
            Ok(marked_range_state(range.location, range.length))
        }
        -25212 => Ok(super::CompositionState::Unknown),
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
    attribute_query();
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

fn capture_application(pid: u32) -> Result<(Element, bool), ReadError> {
    let frontmost = frontmost_app(pid)?;
    let application = unsafe { AXUIElementCreateApplication(pid as i32) };
    if application.is_null() {
        return Err(ReadError::Unsupported);
    }
    let application = Element(application);
    status(unsafe { AXUIElementSetMessagingTimeout(application.0, 0.1) })?;
    let electron = has_electron_framework(&frontmost);
    let bundle = frontmost.bundleIdentifier().map(|id| id.to_string());
    let chromium = uses_chromium_web_contract(bundle.as_deref(), electron);
    if chromium {
        // Chromium/Electron's supported lightweight activation path: querying
        // the app's role requests native AX APIs. Focus alone can stay an empty
        // proxy. This does not toggle full screen-reader mode or scan fields.
        string_attribute(&application, "AXRole")?;
    }
    Ok((application, chromium))
}

fn focused_raw(application: &Element, pid: u32) -> Result<Element, ReadError> {
    same_frontmost(pid)?;
    let value = attribute(application, "AXFocusedUIElement")?;
    if unsafe { CFGetTypeID(value.as_CFTypeRef()) } != unsafe { AXUIElementGetTypeID() } {
        return Err(ReadError::Unsupported);
    }
    let retained = unsafe { core_foundation::base::CFRetain(value.as_CFTypeRef()) };
    let element = Element(retained as ElementRef);
    status(unsafe { AXUIElementSetMessagingTimeout(element.0, 0.1) })?;
    same_frontmost(pid)?;
    Ok(element)
}

#[cfg(test)]
fn focused(pid: u32) -> Result<Element, ReadError> {
    let (application, _) = capture_application(pid)?;
    resolve_editable_focus(focused_raw(&application, pid)?, pid)
}

fn reject_protected(element: &Element) -> Result<String, ReadError> {
    let role = string_attribute(element, "AXRole")?;
    if role == "AXSecureTextField" {
        return Err(ReadError::SecureInput);
    }
    match string_attribute(element, "AXSubrole") {
        Ok(role) if role == "AXSecureTextField" => Err(ReadError::SecureInput),
        Ok(_) | Err(ReadError::NoValue | ReadError::Native(-25205)) => Ok(role),
        Err(error) => Err(error),
    }
}

fn resolve_editable_focus(element: Element, pid: u32) -> Result<Element, ReadError> {
    reject_protected(&element)?;
    let ancestor = match attribute(&element, "AXHighestEditableAncestor") {
        Ok(value) => value,
        Err(ReadError::NoValue | ReadError::Native(-25205)) => {
            match attribute(&element, "AXEditableAncestor") {
                Ok(value) => value,
                Err(ReadError::NoValue | ReadError::Native(-25205)) => return Ok(element),
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    if unsafe { CFGetTypeID(ancestor.as_CFTypeRef()) } != unsafe { AXUIElementGetTypeID() } {
        return Err(ReadError::Unsupported);
    }
    if unsafe { CFEqual(ancestor.as_CFTypeRef(), element.0 as CFTypeRef) } != 0 {
        return Ok(element);
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

fn same_focus(application: &Element, pid: u32, element: &Element) -> Result<(), ReadError> {
    let raw = focused_raw(application, pid)?;
    // The common case needs no ancestor query or walk. The retained root's
    // secure role/subrole are checked immediately before/after the body read.
    if unsafe { CFEqual(element.0 as CFTypeRef, raw.0 as CFTypeRef) } != 0 {
        return Ok(());
    }
    let current = resolve_editable_focus(raw, pid)?;
    if unsafe { CFEqual(element.0 as CFTypeRef, current.0 as CFTypeRef) } == 0 {
        return Err(ReadError::TargetChanged);
    }
    Ok(())
}

fn check_control(element: &Element, chromium: bool) -> Result<TextKind, ReadError> {
    #[cfg(test)]
    CAPABILITY_CHECKS.with(|count| count.set(count.get() + 1));
    if unsafe { IsSecureEventInputEnabled() } {
        return Err(ReadError::SecureInput);
    }
    let role = string_attribute(element, "AXRole")?;
    if role == "AXSecureTextField" {
        return Err(ReadError::SecureInput);
    }
    let web_area = role == "AXWebArea" && verified_webkit_text_node(element)?;
    if !matches!(role.as_str(), "AXTextField" | "AXTextArea" | "AXComboBox") && !web_area {
        return Err(ReadError::Unsupported);
    }
    match string_attribute(element, "AXSubrole") {
        Ok(subrole) if subrole == "AXSecureTextField" => return Err(ReadError::SecureInput),
        // AXSubrole is optional in the AX role contract. An explicit
        // AttributeUnsupported (-25205) means the generic role has no
        // specialization; NoValue (-25212) requires a verified web/text node
        // contract or a verified non-DOM native text control so uninitialized
        // Chromium focus proxies remain blocked.
        Ok(_) | Err(ReadError::Native(-25205)) => {}
        Err(ReadError::NoValue)
            if web_area
                || (chromium && verified_chromium_web_node(element)?)
                || verified_webkit_text_node(element)?
                || (!chromium && verified_native_text_control(element)?) => {}
        Err(ReadError::NoValue | ReadError::Unsupported) => return Err(ReadError::SecurityUnknown),
        Err(error) => return Err(error),
    }
    check_editable(element)?;
    Ok(if web_area {
        TextKind::WebArea
    } else {
        TextKind::Control
    })
}

fn check_live_control(element: &Element, kind: TextKind) -> Result<(), ReadError> {
    if unsafe { IsSecureEventInputEnabled() } {
        return Err(ReadError::SecureInput);
    }
    let role = reject_protected(element)?;
    if !matches!(
        (kind, role.as_str()),
        (
            TextKind::Control,
            "AXTextField" | "AXTextArea" | "AXComboBox"
        ) | (TextKind::WebArea, "AXWebArea")
    ) {
        return Err(ReadError::TargetChanged);
    }
    check_editable(element)
}

fn check_editable(element: &Element) -> Result<(), ReadError> {
    for name in ["AXValue", "AXSelectedText"] {
        let attribute = CFString::new(name);
        let mut settable = 0u8;
        attribute_query();
        match status(unsafe {
            AXUIElementIsAttributeSettable(
                element.0,
                attribute.as_concrete_TypeRef(),
                &mut settable,
            )
        }) {
            Ok(()) if settable != 0 => return Ok(()),
            Ok(()) | Err(ReadError::NoValue | ReadError::Native(-25205)) => {}
            Err(error) => return Err(error),
        }
    }
    Err(ReadError::Unsupported)
}

/// Chromium's actual web nodes return a nil subrole for ordinary editors and
/// AXSecureTextField for protected fields. Its incomplete focus proxy does not
/// carry this contract. Require a verified Chromium provider, a live node ID,
/// and a self-identifying editable ancestor before interpreting nil as absent.
/// No accessibility flags, focus, selection or clipboard are changed here.
fn uses_chromium_web_contract(bundle_id: Option<&str>, electron_framework: bool) -> bool {
    if electron_framework {
        return true;
    }
    let Some(id) = bundle_id else {
        return false;
    };
    matches!(
        id,
        "com.google.Chrome"
            | "com.google.Chrome.beta"
            | "com.google.Chrome.dev"
            | "com.google.Chrome.canary"
            | "org.chromium.Chromium"
            | "com.microsoft.edgemac"
            | "com.microsoft.edgemac.Beta"
            | "com.microsoft.edgemac.Dev"
            | "com.microsoft.edgemac.Canary"
            | "company.thebrowser.Browser"
            | "company.thebrowser.dia"
            | "com.brave.Browser"
            | "com.brave.Browser.beta"
            | "com.brave.Browser.nightly"
            | "com.vivaldi.Vivaldi"
            | "com.operasoftware.Opera"
            | "com.openai.codex"
    )
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
    attribute_query();
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
    attribute_query();
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
    #[cfg(test)]
    FRAMEWORK_CHECKS.with(|count| count.set(count.get() + 1));
    app.bundleURL()
        .and_then(|url| url.path())
        .is_some_and(|path| {
            let frameworks = std::path::Path::new(&path.to_string()).join("Contents/Frameworks");
            [
                "Electron Framework.framework",
                "Chromium Embedded Framework.framework",
                "Chromium Framework.framework",
                "QtWebEngineCore.framework",
                "nwjs Framework.framework",
            ]
            .iter()
            .any(|name| frameworks.join(name).is_dir())
        })
}

/// A native NSAccessibilityElement text control without a subrole returns
/// AX_NO_VALUE for AXSubrole, whereas an uninitialized Chromium focus proxy in
/// an unknown Chromium container either supports ChromeAXNodeId / AXDOMClassList
/// or lacks a live native character count + selection range contract.
fn verified_native_text_control(element: &Element) -> Result<bool, ReadError> {
    for web_attr in ["ChromeAXNodeId", "AXDOMClassList"] {
        match attribute(element, web_attr) {
            Err(ReadError::Native(-25205)) => {}
            Ok(_) | Err(ReadError::NoValue | ReadError::Unsupported) => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    let count_value = match attribute(element, "AXNumberOfCharacters") {
        Ok(value) => value,
        Err(ReadError::NoValue | ReadError::Native(-25205) | ReadError::Unsupported) => {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    if unsafe { CFGetTypeID(count_value.as_CFTypeRef()) } != CFNumber::type_id() {
        return Ok(false);
    }
    let Some(count) =
        (unsafe { CFNumber::wrap_under_get_rule(count_value.as_CFTypeRef() as CFNumberRef) })
            .to_i64()
            .filter(|count| *count >= 0 && *count <= isize::MAX as i64)
    else {
        return Ok(false);
    };
    match read_selected_range(element, count as usize) {
        Ok(_) => Ok(true),
        Err(ReadError::NoValue | ReadError::Native(-25205) | ReadError::Unsupported) => Ok(false),
        Err(error) => Err(error),
    }
}

fn verified_chromium_web_node(element: &Element) -> Result<bool, ReadError> {
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
    fn native_marked_ranges_keep_unknown_separate_from_idle() {
        use super::super::CompositionState::*;
        assert_eq!(super::marked_range_state(isize::MAX, 0), Inactive);
        assert_eq!(super::marked_range_state(2, 5), Active);
        assert_eq!(super::marked_range_state(-1, 5), Unknown);
        assert_eq!(super::marked_range_state(isize::MAX, 5), Unknown);
        assert_eq!(super::merge_composition(Unknown, Unknown), Unknown);
        assert_eq!(super::merge_composition(Inactive, Unknown), Inactive);
        assert_eq!(super::merge_composition(Inactive, Active), Active);
    }

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
        for browser in [
            "com.google.Chrome",
            "com.google.Chrome.canary",
            "org.chromium.Chromium",
            "com.microsoft.edgemac",
            "company.thebrowser.Browser",
            "company.thebrowser.dia",
            "com.brave.Browser",
            "com.vivaldi.Vivaldi",
            "com.operasoftware.Opera",
            "com.openai.codex",
        ] {
            assert!(uses_chromium_web_contract(Some(browser), false));
        }
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
