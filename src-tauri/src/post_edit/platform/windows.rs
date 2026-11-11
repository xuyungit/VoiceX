use std::time::Instant;
use windows::core::{Interface, BSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Variant::{VariantClear, VT_BOOL, VT_UNKNOWN};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationElement,
    IUIAutomationTextEditPattern, IUIAutomationTextPattern, IUIAutomationTextRange,
    TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start, TextUnit_Character,
    UIA_EditControlTypeId, UIA_IsReadOnlyAttributeId, UIA_TextEditPatternId, UIA_TextPatternId,
};

use super::{OwnedSnapshot, ReadError};

use super::window::{EdgeGuards, CONTEXT_UNITS, MAX_UNITS};

pub(super) struct NativeReader {
    automation: Result<IUIAutomation, i32>,
    initialized: bool,
    target: Option<(u64, u64, IUIAutomationElement)>,
    window: Option<IUIAutomationTextRange>,
    clipped_edges: (bool, bool),
    guards: EdgeGuards,
}

impl NativeReader {
    pub(super) fn clear(&mut self) {
        self.target = None;
        self.window = None;
        self.clipped_edges = (false, false);
        self.guards = EdgeGuards::default();
    }
    pub(super) fn new() -> Self {
        let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        let automation = if initialized {
            (|| -> windows::core::Result<IUIAutomation> {
                let automation: IUIAutomation =
                    unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }?;
                let bounded = automation.cast::<IUIAutomation2>()?;
                unsafe {
                    bounded.SetConnectionTimeout(200)?;
                    bounded.SetTransactionTimeout(200)?;
                }
                Ok(automation)
            })()
            .map_err(|e| e.code().0)
        } else {
            Err(-1)
        };
        Self {
            automation,
            initialized,
            target: None,
            window: None,
            clipped_edges: (false, false),
            guards: EdgeGuards::default(),
        }
    }

    pub(super) fn capture(
        &mut self,
        generation: u64,
        expected_pid: u32,
        lookbehind: usize,
    ) -> Result<OwnedSnapshot, ReadError> {
        self.clear();
        let automation = self
            .automation
            .as_ref()
            .map_err(|e| ReadError::Native(*e))?;
        let element = unsafe { automation.GetFocusedElement() }.map_err(native)?;
        if unsafe { element.CurrentProcessId() }.map_err(native)? as u32 != expected_pid {
            return Err(ReadError::TargetChanged);
        }
        check_control(&element)?;
        if composition_state(&element) == super::CompositionState::Active {
            return Err(ReadError::CompositionActive);
        }
        let (window, clipped_edges) = local_window(&element, lookbehind)?;
        self.clipped_edges = clipped_edges;
        self.window = Some(window);
        self.target = Some((generation, super::target_token(), element));
        self.read(generation)
    }

    pub(super) fn reframe(
        &mut self,
        generation: u64,
        target: u64,
        lookbehind: usize,
    ) -> Result<OwnedSnapshot, ReadError> {
        let automation = self
            .automation
            .as_ref()
            .map_err(|e| ReadError::Native(*e))?;
        let (stored_generation, token, element) =
            self.target.as_ref().ok_or(ReadError::TargetChanged)?;
        if *stored_generation != generation {
            return Err(ReadError::StaleGeneration);
        }
        if *token != target {
            return Err(ReadError::TargetChanged);
        }
        same_focus(automation, element)?;
        check_control(element)?;
        if composition_state(element) == super::CompositionState::Active {
            return Err(ReadError::CompositionActive);
        }
        let (window, clipped_edges) = local_window(element, lookbehind)?;
        self.window = Some(window);
        self.clipped_edges = clipped_edges;
        self.guards = EdgeGuards::default();
        self.read(generation)
    }

    pub(super) fn read(&mut self, generation: u64) -> Result<OwnedSnapshot, ReadError> {
        let automation = self
            .automation
            .as_ref()
            .map_err(|e| ReadError::Native(*e))?;
        let (target_generation, token, element) =
            self.target.as_ref().ok_or(ReadError::TargetChanged)?;
        if *target_generation != generation {
            return Err(ReadError::StaleGeneration);
        }
        same_focus(automation, element)?;
        check_control(element)?;
        let composition_before = composition_state(element);
        if composition_before == super::CompositionState::Active {
            return Err(ReadError::CompositionActive);
        }
        let pattern =
            unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }
                .map_err(native)?;
        let document = unsafe { pattern.DocumentRange() }.map_err(native)?;
        let window = self.window.as_ref().ok_or(ReadError::TargetChanged)?;
        // Keep true document edges attached, including a degenerate empty
        // field before injection. Clipped interior edges stay native anchors.
        unsafe {
            if !self.clipped_edges.0 {
                window
                    .MoveEndpointByRange(
                        TextPatternRangeEndpoint_Start,
                        &document,
                        TextPatternRangeEndpoint_Start,
                    )
                    .map_err(native)?;
            }
            if !self.clipped_edges.1 {
                window
                    .MoveEndpointByRange(
                        TextPatternRangeEndpoint_End,
                        &document,
                        TextPatternRangeEndpoint_End,
                    )
                    .map_err(native)?;
            }
        }
        // Read the retained local range, never the whole document or an
        // unbounded prefix from its beginning. UIA owns range endpoint updates.
        let text = bounded_text(
            unsafe { window.GetText((MAX_UNITS + 1) as i32) }
                .map_err(native)?
                .to_string(),
        )?;
        if text.is_empty()
            && unsafe {
                document.CompareEndpoints(
                    TextPatternRangeEndpoint_Start,
                    &document,
                    TextPatternRangeEndpoint_End,
                )
            }
            .map_err(native)?
                != 0
        {
            return Err(ReadError::WindowChanged);
        }
        let selections = unsafe { pattern.GetSelection() }.map_err(native)?;
        if unsafe { selections.Length() }.map_err(native)? != 1 {
            return Err(ReadError::Unsupported);
        }
        let selected = unsafe { selections.GetElement(0) }.map_err(native)?;
        if unsafe {
            selected.CompareEndpoints(
                TextPatternRangeEndpoint_Start,
                window,
                TextPatternRangeEndpoint_Start,
            )
        }
        .map_err(native)?
            < 0
            || unsafe {
                selected.CompareEndpoints(
                    TextPatternRangeEndpoint_End,
                    window,
                    TextPatternRangeEndpoint_End,
                )
            }
            .map_err(native)?
                > 0
        {
            return Err(ReadError::WindowChanged);
        }
        let prefix_start = unsafe { window.Clone() }.map_err(native)?;
        unsafe {
            prefix_start.MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &selected,
                TextPatternRangeEndpoint_Start,
            )
        }
        .map_err(native)?;
        let prefix_end = unsafe { window.Clone() }.map_err(native)?;
        unsafe {
            prefix_end.MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &selected,
                TextPatternRangeEndpoint_End,
            )
        }
        .map_err(native)?;
        let start_text = bounded_text(
            unsafe { prefix_start.GetText((MAX_UNITS + 1) as i32) }
                .map_err(native)?
                .to_string(),
        )?;
        let end_text = bounded_text(
            unsafe { prefix_end.GetText((MAX_UNITS + 1) as i32) }
                .map_err(native)?
                .to_string(),
        )?;
        if !text.starts_with(&start_text)
            || !text.starts_with(&end_text)
            || !end_text.starts_with(&start_text)
        {
            return Err(ReadError::TargetChanged);
        }
        self.guards
            .check(&text, self.clipped_edges.0, self.clipped_edges.1)?;
        same_focus(automation, element)?;
        check_control(element)?;
        let composition_after = composition_state(element);
        if composition_after == super::CompositionState::Active {
            return Err(ReadError::CompositionActive);
        }
        let selection_utf16 = start_text.encode_utf16().count()..end_text.encode_utf16().count();
        // Resets are only recognised in a whole, bounded field whose provider
        // reports per-range editability; otherwise the observation keeps its
        // wholesale-replacement guard.
        let reset_detection = self.clipped_edges == (false, false)
            && read_only_attribute(automation, window)? != ReadOnly::Unsupported;
        if reset_detection
            && super::empty_caret_prefix(&text, &selection_utf16)
            && !text.chars().all(super::super::range::is_blank)
        {
            // A mixed/unsupported attribute does not establish a reset. Require
            // a caret before an explicitly read-only text range, not after an
            // atomic mention or alongside ordinary editable text.
            // Exclude generated surrounding line breaks, whose editability can
            // differ from the hint. Search only the bounded, whole-field window.
            let needle = BSTR::from(text.trim_matches(super::super::range::is_blank));
            let mut raw = std::ptr::null_mut();
            // S_OK + null means that a concurrent edit removed the text. The
            // generated wrapper cannot represent that successful no-match.
            unsafe {
                (window.vtable().FindText)(
                    window.as_raw(),
                    std::mem::transmute_copy(&needle),
                    false.into(),
                    false.into(),
                    &mut raw,
                )
            }
            .ok()
            .map_err(native)?;
            if raw.is_null() {
                return Err(ReadError::WindowChanged);
            }
            let display = unsafe { IUIAutomationTextRange::from_raw(raw) };
            if read_only_attribute(automation, &display)? == ReadOnly::Yes {
                return Err(ReadError::FieldCleared);
            }
        }
        Ok(OwnedSnapshot {
            target: *token,
            process_id: unsafe { element.CurrentProcessId() }.map_err(native)? as u32,
            text,
            selection_utf16,
            read_at: Instant::now(),
            event_driven: false,
            composition: if composition_before == composition_after {
                composition_before
            } else {
                super::CompositionState::Unknown
            },
            reset_detection,
        })
    }
}

impl Drop for NativeReader {
    fn drop(&mut self) {
        self.target = None;
        // Release the COM client on this worker before leaving its apartment.
        self.automation = Err(-1);
        if self.initialized {
            unsafe { CoUninitialize() };
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadOnly {
    Yes,
    No,
    Mixed,
    Unsupported,
}

fn read_only_attribute(
    automation: &IUIAutomation,
    range: &IUIAutomationTextRange,
) -> Result<ReadOnly, ReadError> {
    let mut value =
        unsafe { range.GetAttributeValue(UIA_IsReadOnlyAttributeId) }.map_err(native)?;
    let inner = unsafe { &value.Anonymous.Anonymous };
    let attribute = if inner.vt == VT_BOOL {
        if unsafe { inner.Anonymous.boolVal }.as_bool() {
            ReadOnly::Yes
        } else {
            ReadOnly::No
        }
    } else if inner.vt == VT_UNKNOWN {
        // Mixed and not-supported are both COM sentinels, told apart by identity.
        let mixed = unsafe { automation.ReservedMixedAttributeValue() }.map_err(native)?;
        let sentinel = unsafe { &inner.Anonymous.punkVal };
        if sentinel
            .as_ref()
            .is_some_and(|value| value.as_raw() == mixed.as_raw())
        {
            ReadOnly::Mixed
        } else {
            ReadOnly::Unsupported
        }
    } else {
        ReadOnly::Unsupported
    };
    unsafe { VariantClear(&mut value) }.map_err(native)?;
    Ok(attribute)
}

fn local_window(
    element: &IUIAutomationElement,
    lookbehind: usize,
) -> Result<(IUIAutomationTextRange, (bool, bool)), ReadError> {
    let pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }
            .map_err(native)?;
    let selections = unsafe { pattern.GetSelection() }.map_err(native)?;
    if unsafe { selections.Length() }.map_err(native)? != 1 {
        return Err(ReadError::Unsupported);
    }
    let selected = unsafe { selections.GetElement(0) }.map_err(native)?;
    let window = unsafe { selected.Clone() }.map_err(native)?;
    let lookbehind = i32::try_from(lookbehind).map_err(|_| ReadError::TooLarge)?;
    unsafe {
        window
            .MoveEndpointByUnit(
                TextPatternRangeEndpoint_Start,
                TextUnit_Character,
                -lookbehind,
            )
            .map_err(native)?;
        window
            .MoveEndpointByUnit(
                TextPatternRangeEndpoint_End,
                TextUnit_Character,
                CONTEXT_UNITS as i32,
            )
            .map_err(native)?;
    }
    let document = unsafe { pattern.DocumentRange() }.map_err(native)?;
    let clipped_edges = (
        unsafe {
            window.CompareEndpoints(
                TextPatternRangeEndpoint_Start,
                &document,
                TextPatternRangeEndpoint_Start,
            )
        }
        .map_err(native)?
            > 0,
        unsafe {
            window.CompareEndpoints(
                TextPatternRangeEndpoint_End,
                &document,
                TextPatternRangeEndpoint_End,
            )
        }
        .map_err(native)?
            < 0,
    );
    Ok((window, clipped_edges))
}

fn composition_state(element: &IUIAutomationElement) -> super::CompositionState {
    let Ok(pattern) = (unsafe {
        element.GetCurrentPatternAs::<IUIAutomationTextEditPattern>(UIA_TextEditPatternId)
    }) else {
        return super::CompositionState::Unknown;
    };
    // The native contract permits S_OK + null. The generated interface return
    // type cannot represent that successful no-composition result.
    let mut raw = std::ptr::null_mut();
    let result = unsafe { (pattern.vtable().GetActiveComposition)(pattern.as_raw(), &mut raw) };
    let active = !raw.is_null();
    if active {
        drop(unsafe { windows::core::IUnknown::from_raw(raw) });
    }
    if result.is_err() {
        super::CompositionState::Unknown
    } else if active {
        super::CompositionState::Active
    } else {
        super::CompositionState::Inactive
    }
}

fn native(error: windows::core::Error) -> ReadError {
    ReadError::Native(error.code().0)
}

fn same_focus(automation: &IUIAutomation, element: &IUIAutomationElement) -> Result<(), ReadError> {
    let focused = unsafe { automation.GetFocusedElement() }.map_err(native)?;
    if !unsafe { automation.CompareElements(element, &focused) }
        .map_err(native)?
        .as_bool()
    {
        return Err(ReadError::TargetChanged);
    }
    Ok(())
}

fn check_control(element: &IUIAutomationElement) -> Result<(), ReadError> {
    // Any failure to establish the password property denies access to text.
    if unsafe { element.CurrentIsPassword() }
        .map_err(|_| ReadError::SecurityUnknown)?
        .as_bool()
    {
        return Err(ReadError::SecureInput);
    }
    if unsafe { element.CurrentControlType() }.map_err(native)? != UIA_EditControlTypeId {
        return Err(ReadError::Unsupported);
    }
    if !unsafe { element.CurrentIsKeyboardFocusable() }
        .map_err(native)?
        .as_bool()
    {
        return Err(ReadError::Unsupported);
    }
    Ok(())
}

fn bounded_text(text: String) -> Result<String, ReadError> {
    if text.encode_utf16().count() > MAX_UNITS {
        Err(ReadError::TooLarge)
    } else {
        Ok(text)
    }
}
