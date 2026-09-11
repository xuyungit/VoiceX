//! Thin, self-declared bindings over the Accessibility C API.
//!
//! Only the handful of `ApplicationServices` entry points the selection reader
//! needs are declared here; AX handles never escape this module.
//!
//! Every call must happen on the selection worker thread (see
//! [`crate::selection`]) — the AX API is single-threaded.

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFGetTypeID, CFRelease, CFTypeID, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::{CFString, CFStringRef};

#[allow(non_camel_case_types)]
type AXError = i32;

const K_AX_ERROR_SUCCESS: AXError = 0;
const K_AX_ERROR_ATTRIBUTE_UNSUPPORTED: AXError = -25205;
const K_AX_ERROR_NO_VALUE: AXError = -25212;
const K_AX_ERROR_API_DISABLED: AXError = -25211;
const K_AX_ERROR_NOT_IMPLEMENTED: AXError = -25208;
/// Not an `AXError`: our own marker for "the attribute answered with something
/// that is not a string". Outside the API's allocated range on purpose.
const WRONG_TYPE_STATUS: AXError = 1;

#[repr(C)]
struct __AXUIElement {
    _private: [u8; 0],
}
type AXUIElementRef = *const __AXUIElement;

#[allow(non_snake_case)]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> AXError;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementCopyAttributeNames(element: AXUIElementRef, names: *mut CFArrayRef) -> AXError;
    fn AXUIElementCopyParameterizedAttributeValue(
        element: AXUIElementRef,
        parameterized_attribute: CFStringRef,
        parameter: CFTypeRef,
        result: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementGetTypeID() -> CFTypeID;
}

#[allow(non_snake_case)]
#[link(name = "Carbon", kind = "framework")]
extern "C" {
    /// True while any process has enabled secure keyboard entry (password
    /// fields, Terminal's "Secure Keyboard Entry", …).
    fn IsSecureEventInputEnabled() -> bool;
}

/// Whether the system is currently in secure keyboard entry.
///
/// This is a **capability check, not a privacy judgement** (plan §3.4): the
/// window server drops synthetic key events in that state, so the Cmd-C of the
/// clipboard fallback would never reach the application. It gates only the
/// copy path, and only so the caller can fail immediately with a code that
/// explains itself instead of sitting out the copy timeout and reporting a
/// silent copy. It says nothing about whether the selected text is sensitive —
/// the Accessibility path is unaffected and still reads normally.
pub fn secure_event_input_enabled() -> bool {
    unsafe { IsSecureEventInputEnabled() }
}

/// Applications known to need [`enable_manual_accessibility`], by bundle id.
///
/// An allow-list, not a rule for Electron as a class: Chrome serves
/// `AXSelectedText` with no intervention at all, and so does at least one other
/// Electron app measured in plan §5.4. Writing the attribute changes the target
/// application's state, so it is done only where it has been shown to be both
/// necessary and sufficient.
const MANUAL_ACCESSIBILITY_APPS: [&str; 2] = ["com.microsoft.VSCode", "com.vscodium.codium"];

pub fn needs_manual_accessibility(bundle_id: Option<&str>) -> bool {
    bundle_id.is_some_and(|id| MANUAL_ACCESSIBILITY_APPS.contains(&id))
}

/// Ask an Electron application to build its accessibility tree.
///
/// Electron exposes nothing to the Accessibility API until `AXManualAccessibility`
/// is set, which is why a cold VS Code answers `AXFocusedUIElement` with nothing
/// and the read falls through to the clipboard. Avoiding that fallback is worth
/// something on its own: it is the only path that touches what the user copied.
///
/// Returns whether the attribute was accepted. Deliberately noisy in the log —
/// this modifies another application's state, and a change we made should never
/// have to be inferred later.
pub fn enable_manual_accessibility(pid: i32) -> bool {
    let app = unsafe { AXUIElementCreateApplication(pid) };
    if app.is_null() {
        return false;
    }
    let app = AxElement(app);

    let attribute = CFString::new("AXManualAccessibility");
    let value = CFBoolean::true_value();
    let status = unsafe {
        AXUIElementSetAttributeValue(app.0, attribute.as_concrete_TypeRef(), value.as_CFTypeRef())
    };

    if status == K_AX_ERROR_SUCCESS {
        log::info!("Enabled AXManualAccessibility on pid {pid}");
        true
    } else {
        // Not an error worth failing the read over: the clipboard fallback
        // still works, which is exactly where we were headed anyway.
        log::debug!("AXManualAccessibility refused by pid {pid}: status {status}");
        false
    }
}

/// Owned `AXUIElementRef`. Releases on drop.
struct AxElement(AXUIElementRef);

/// Any owned CoreFoundation object we never inspect, only hand back to the
/// API that produced it (the opaque `AXTextMarkerRange`). Releases on drop.
struct OwnedCfType(CFTypeRef);

impl Drop for OwnedCfType {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

impl Drop for AxElement {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0 as CFTypeRef) };
        }
    }
}

/// What an attribute read produced. `Unsupported` and `Empty` are deliberately
/// distinct: the former means "ask the next layer", the latter means "this
/// control is authoritative and nothing is selected".
///
/// Each variant carries the raw `AXError` it came from. Several distinct codes
/// collapse into `Unsupported`, and telling them apart is the whole point of
/// the diagnostic: "this role has no such attribute" and "the element went
/// stale mid-read" both fall back to Copy, but only the first one means the
/// Accessibility path is genuinely a dead end for that application.
pub enum AttributeRead {
    Text(String),
    Empty(AXError),
    Unsupported(AXError),
    ApiDisabled,
}

impl AttributeRead {
    /// Stable token for the structured log. Part of the harness contract.
    pub fn kind(&self) -> &'static str {
        match self {
            AttributeRead::Text(_) => "text",
            AttributeRead::Empty(_) => "empty",
            AttributeRead::Unsupported(_) => "unsupported",
            AttributeRead::ApiDisabled => "api_disabled",
        }
    }

    pub fn status(&self) -> AXError {
        match self {
            AttributeRead::Text(_) => K_AX_ERROR_SUCCESS,
            AttributeRead::Empty(status) | AttributeRead::Unsupported(status) => *status,
            AttributeRead::ApiDisabled => K_AX_ERROR_API_DISABLED,
        }
    }
}

/// Which selection-related attributes the focused element advertises.
///
/// Answers, in one AX round-trip, the question phase 1 turns on: whether the
/// range-reading layer has anything to read. An element that does not list
/// `AXSelectedTextRange` cannot serve it, and a WebKit web area that lists
/// `AXSelectedTextMarkerRange` instead needs the marker API rather than the
/// standard one (plan §5).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SelectionAttributes {
    /// None when the element refused to enumerate its attributes at all.
    pub enumerated: bool,
    pub selected_text: bool,
    pub selected_text_range: bool,
    pub selected_text_marker_range: bool,
    pub value: bool,
}

impl SelectionAttributes {
    fn from_names(names: &[String]) -> Self {
        let has = |needle: &str| names.iter().any(|name| name == needle);
        Self {
            enumerated: true,
            selected_text: has("AXSelectedText"),
            selected_text_range: has("AXSelectedTextRange"),
            selected_text_marker_range: has("AXSelectedTextMarkerRange"),
            value: has("AXValue"),
        }
    }
}

pub struct FocusedElement {
    element: AxElement,
    role: Option<String>,
    subrole: Option<String>,
}

impl FocusedElement {
    /// Read the system-wide focused UI element.
    ///
    /// `Ok(None)` means the API answered but there is no focused element (for
    /// example nothing is frontmost); `Err(true)` signals the API is disabled,
    /// i.e. the accessibility permission is missing.
    pub fn read() -> Result<Option<Self>, ApiDisabled> {
        let system_wide = unsafe { AXUIElementCreateSystemWide() };
        if system_wide.is_null() {
            return Ok(None);
        }
        let system_wide = AxElement(system_wide);

        let attribute = CFString::new("AXFocusedUIElement");
        let mut value: CFTypeRef = std::ptr::null();
        let status = unsafe {
            AXUIElementCopyAttributeValue(
                system_wide.0,
                attribute.as_concrete_TypeRef(),
                &mut value,
            )
        };

        match status {
            K_AX_ERROR_SUCCESS if !value.is_null() => {}
            K_AX_ERROR_API_DISABLED => return Err(ApiDisabled),
            _ => {
                if !value.is_null() {
                    unsafe { CFRelease(value) };
                }
                return Ok(None);
            }
        }

        // Refuse to reinterpret the pointer unless CoreFoundation agrees it is
        // an AXUIElement.
        if unsafe { CFGetTypeID(value) } != unsafe { AXUIElementGetTypeID() } {
            unsafe { CFRelease(value) };
            return Ok(None);
        }

        let element = AxElement(value as AXUIElementRef);
        let role = copy_string_attribute(&element, "AXRole");
        let subrole = copy_string_attribute(&element, "AXSubrole");

        Ok(Some(Self {
            element,
            role,
            subrole,
        }))
    }

    /// The focused element of one application, asked through its application
    /// element instead of the system-wide one.
    ///
    /// Test-only: the system-wide query needs a window-server session, which a
    /// test binary launched from a shell does not always have
    /// (`kAXErrorCannotComplete`), while the per-application query works from
    /// anywhere the process is trusted. The app itself keeps the system-wide
    /// read, which is what defines "the focused control" for a hotkey.
    #[cfg(test)]
    fn read_in_application(pid: i32) -> Option<Self> {
        let app = unsafe { AXUIElementCreateApplication(pid) };
        if app.is_null() {
            return None;
        }
        let app = AxElement(app);
        let attribute = CFString::new("AXFocusedUIElement");
        let mut value: CFTypeRef = std::ptr::null();
        let status =
            unsafe { AXUIElementCopyAttributeValue(app.0, attribute.as_concrete_TypeRef(), &mut value) };
        println!("application AXFocusedUIElement status={status}");
        if status != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }
        let element = AxElement(value as AXUIElementRef);
        let role = copy_string_attribute(&element, "AXRole");
        let subrole = copy_string_attribute(&element, "AXSubrole");
        Some(Self {
            element,
            role,
            subrole,
        })
    }

    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }

    pub fn subrole(&self) -> Option<&str> {
        self.subrole.as_deref()
    }

    pub fn selected_text(&self) -> AttributeRead {
        read_string_attribute(&self.element, "AXSelectedText")
    }

    /// Read the selection the WebKit way: `AXSelectedTextMarkerRange`, then
    /// the parameterized `AXStringForTextMarkerRange` to turn the opaque range
    /// into text.
    ///
    /// A web area answers `AXSelectedText` with `kAXErrorNoValue` whether or
    /// not anything is selected, so for WebKit content this is the only
    /// Accessibility read that says anything (plan §5.1). Only meaningful on an
    /// element whose attribute list includes the marker range; the caller
    /// checks [`SelectionAttributes::selected_text_marker_range`] first rather
    /// than paying for a round trip that every non-WebKit control refuses.
    ///
    /// The marker range is an opaque `AXTextMarkerRange` that only the owning
    /// process can interpret, so it is handed straight back without being
    /// looked at. Both steps report through [`AttributeRead`]: a missing range
    /// is `Empty` (nothing selected — WebKit answers `kAXErrorNoValue` for a
    /// collapsed selection), and a refusal at either step is `Unsupported` with
    /// the raw status.
    pub fn selected_text_via_marker_range(&self) -> AttributeRead {
        let range_name = CFString::new("AXSelectedTextMarkerRange");
        let mut range: CFTypeRef = std::ptr::null();
        let status = unsafe {
            AXUIElementCopyAttributeValue(
                self.element.0,
                range_name.as_concrete_TypeRef(),
                &mut range,
            )
        };
        if status != K_AX_ERROR_SUCCESS || range.is_null() {
            if !range.is_null() {
                unsafe { CFRelease(range) };
            }
            return match status {
                K_AX_ERROR_API_DISABLED => AttributeRead::ApiDisabled,
                K_AX_ERROR_NO_VALUE => AttributeRead::Empty(status),
                _ => AttributeRead::Unsupported(status),
            };
        }
        let range = OwnedCfType(range);

        let string_name = CFString::new("AXStringForTextMarkerRange");
        let mut value: CFTypeRef = std::ptr::null();
        let status = unsafe {
            AXUIElementCopyParameterizedAttributeValue(
                self.element.0,
                string_name.as_concrete_TypeRef(),
                range.0,
                &mut value,
            )
        };
        if status != K_AX_ERROR_SUCCESS || value.is_null() {
            if !value.is_null() {
                unsafe { CFRelease(value) };
            }
            return match status {
                K_AX_ERROR_API_DISABLED => AttributeRead::ApiDisabled,
                // The range exists but resolves to nothing; treat like an
                // empty string rather than a dead end.
                K_AX_ERROR_NO_VALUE => AttributeRead::Empty(status),
                _ => AttributeRead::Unsupported(status),
            };
        }
        if unsafe { CFGetTypeID(value) } != CFString::type_id() {
            unsafe { CFRelease(value) };
            return AttributeRead::Unsupported(WRONG_TYPE_STATUS);
        }

        let text = unsafe { CFString::wrap_under_create_rule(value as CFStringRef) }.to_string();
        if text.is_empty() {
            AttributeRead::Empty(K_AX_ERROR_SUCCESS)
        } else {
            AttributeRead::Text(text)
        }
    }

    /// Enumerate the element's attributes and report the selection-related ones.
    ///
    /// Diagnostic only — nothing in the read path branches on it. The full list
    /// goes to `debug` because it is long and only interesting when adding a new
    /// application to the harness.
    pub fn selection_attributes(&self) -> SelectionAttributes {
        let Some(names) = copy_attribute_names(&self.element) else {
            return SelectionAttributes::default();
        };
        log::debug!(
            "Focused element role={:?} advertises attributes: {}",
            self.role,
            names.join(",")
        );
        SelectionAttributes::from_names(&names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live read of whatever is frontmost, through the same layers the app uses.
    ///
    /// Ignored because it needs a frontmost application with a selection and
    /// the Accessibility permission for the test binary. Driven by
    /// `scripts/tts/marker_range_probe.sh`, which stages a Safari fixture and
    /// runs `cargo test -- --ignored live_marker_range_read --nocapture`.
    #[test]
    #[ignore]
    fn live_marker_range_read() {
        // Raw status first: `read()` folds every failure but API-disabled into
        // `None`, and "not trusted" versus "nothing focused" is the first thing
        // to know when this fails.
        extern "C" {
            fn AXIsProcessTrusted() -> bool;
        }
        println!("AXIsProcessTrusted={}", unsafe { AXIsProcessTrusted() });
        let system_wide = AxElement(unsafe { AXUIElementCreateSystemWide() });
        let attribute = CFString::new("AXFocusedUIElement");
        let mut value: CFTypeRef = std::ptr::null();
        let status = unsafe {
            AXUIElementCopyAttributeValue(system_wide.0, attribute.as_concrete_TypeRef(), &mut value)
        };
        println!("AXFocusedUIElement status={status} null={}", value.is_null());
        if !value.is_null() {
            unsafe { CFRelease(value) };
        }
        let element = match std::env::var("VOICEX_LIVE_PID") {
            Ok(pid) => FocusedElement::read_in_application(pid.parse().expect("pid"))
                .expect("no focused element in that application"),
            Err(_) => FocusedElement::read()
                .unwrap_or_else(|_| panic!("accessibility API disabled for the test binary"))
                .expect("no focused element"),
        };
        println!(
            "focused role={:?} subrole={:?}",
            element.role(),
            element.subrole()
        );
        let direct = element.selected_text();
        println!(
            "AXSelectedText: {} status={}",
            direct.kind(),
            direct.status()
        );
        let attributes = element.selection_attributes();
        println!("attributes: {attributes:?}");
        let marker = element.selected_text_via_marker_range();
        println!("marker range: {} status={}", marker.kind(), marker.status());
        if let AttributeRead::Text(text) = &marker {
            println!("marker text ({} chars): {:?}", text.chars().count(), text);
        }
        assert!(
            attributes.selected_text_marker_range,
            "the focused element does not advertise AXSelectedTextMarkerRange"
        );
        assert!(
            matches!(marker, AttributeRead::Text(_)),
            "marker range produced no text"
        );
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn manual_accessibility_is_an_allow_list_not_a_rule_for_electron() {
        // Writing this attribute changes the target application's state, so it
        // is limited to applications shown to need it. Chrome and at least one
        // other Electron app serve AXSelectedText untouched (plan §5.1, §5.4),
        // and poking those would be a side effect that buys nothing.
        assert!(needs_manual_accessibility(Some("com.microsoft.VSCode")));
        assert!(!needs_manual_accessibility(Some("com.google.Chrome")));
        assert!(!needs_manual_accessibility(Some(
            "com.anthropic.claudefordesktop"
        )));
        assert!(
            !needs_manual_accessibility(None),
            "unknown app: leave alone"
        );
    }

    #[test]
    fn attribute_probe_separates_standard_range_from_the_webkit_marker_range() {
        // A WebKit web area: it offers the marker API instead of the standard
        // range, which is exactly the case where phase 1's range layer would not
        // have helped (plan §5).
        let web_area = SelectionAttributes::from_names(&names(&[
            "AXRole",
            "AXSelectedTextMarkerRange",
            "AXValue",
        ]));
        assert!(web_area.enumerated);
        assert!(!web_area.selected_text);
        assert!(!web_area.selected_text_range);
        assert!(web_area.selected_text_marker_range);

        let text_area = SelectionAttributes::from_names(&names(&[
            "AXRole",
            "AXSelectedText",
            "AXSelectedTextRange",
        ]));
        assert!(text_area.selected_text);
        assert!(text_area.selected_text_range);
        assert!(!text_area.selected_text_marker_range);
    }

    #[test]
    fn an_element_that_refuses_enumeration_is_not_mistaken_for_one_with_no_attributes() {
        // Both report every flag false; only `enumerated` says whether that is a
        // fact about the element or a failed query.
        assert!(!SelectionAttributes::default().enumerated);
        assert!(SelectionAttributes::from_names(&names(&["AXRole"])).enumerated);
    }

    #[test]
    fn empty_keeps_the_status_that_produced_it() {
        // "Answered with an empty string" and "has no value" both read as empty
        // but are different facts about the control.
        assert_eq!(AttributeRead::Empty(K_AX_ERROR_SUCCESS).kind(), "empty");
        assert_eq!(AttributeRead::Empty(K_AX_ERROR_SUCCESS).status(), 0);
        assert_eq!(
            AttributeRead::Empty(K_AX_ERROR_NO_VALUE).status(),
            K_AX_ERROR_NO_VALUE
        );
    }

    #[test]
    fn unsupported_keeps_the_underlying_error_apart() {
        // Both fall back to Copy, but only the first means the role genuinely
        // does not implement the attribute.
        let unsupported = AttributeRead::Unsupported(K_AX_ERROR_ATTRIBUTE_UNSUPPORTED);
        let not_implemented = AttributeRead::Unsupported(K_AX_ERROR_NOT_IMPLEMENTED);
        assert_eq!(unsupported.kind(), not_implemented.kind());
        assert_ne!(unsupported.status(), not_implemented.status());
    }
}

fn copy_attribute_names(element: &AxElement) -> Option<Vec<String>> {
    let mut names: CFArrayRef = std::ptr::null();
    let status = unsafe { AXUIElementCopyAttributeNames(element.0, &mut names) };
    if status != K_AX_ERROR_SUCCESS || names.is_null() {
        if !names.is_null() {
            unsafe { CFRelease(names as CFTypeRef) };
        }
        return None;
    }

    let array: CFArray<CFString> = unsafe { CFArray::wrap_under_create_rule(names) };
    Some(array.iter().map(|name| name.to_string()).collect())
}

pub struct ApiDisabled;

fn copy_string_attribute(element: &AxElement, attribute: &str) -> Option<String> {
    match read_string_attribute(element, attribute) {
        AttributeRead::Text(text) => Some(text),
        _ => None,
    }
}

fn read_string_attribute(element: &AxElement, attribute: &str) -> AttributeRead {
    let name = CFString::new(attribute);
    let mut value: CFTypeRef = std::ptr::null();
    let status =
        unsafe { AXUIElementCopyAttributeValue(element.0, name.as_concrete_TypeRef(), &mut value) };

    if status != K_AX_ERROR_SUCCESS || value.is_null() {
        if !value.is_null() {
            unsafe { CFRelease(value) };
        }
        return match status {
            K_AX_ERROR_API_DISABLED => AttributeRead::ApiDisabled,
            K_AX_ERROR_NO_VALUE => AttributeRead::Empty(status),
            K_AX_ERROR_ATTRIBUTE_UNSUPPORTED | K_AX_ERROR_NOT_IMPLEMENTED => {
                AttributeRead::Unsupported(status)
            }
            // Anything else (invalid element, cannot complete, timeout) means we
            // learned nothing about this control, so let the next layer try.
            _ => AttributeRead::Unsupported(status),
        };
    }

    if unsafe { CFGetTypeID(value) } != CFString::type_id() {
        unsafe { CFRelease(value) };
        // The attribute exists but is not a string. Reported as unsupported so
        // the next layer runs; the sentinel status keeps it distinguishable from
        // a real AX error in the log.
        return AttributeRead::Unsupported(WRONG_TYPE_STATUS);
    }

    // wrap_under_create_rule takes ownership of the +1 retain from Copy…
    let text = unsafe { CFString::wrap_under_create_rule(value as CFStringRef) }.to_string();
    if text.is_empty() {
        // Success, but the control reports an empty selection — a different fact
        // from `kAXErrorNoValue`, and worth telling apart in the diagnostic.
        AttributeRead::Empty(K_AX_ERROR_SUCCESS)
    } else {
        AttributeRead::Text(text)
    }
}
