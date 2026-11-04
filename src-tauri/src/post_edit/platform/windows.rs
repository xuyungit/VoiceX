use std::time::Instant;
use windows::core::Interface;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationElement, IUIAutomationTextPattern,
    TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start, UIA_EditControlTypeId,
    UIA_TextPatternId,
};

use super::{OwnedSnapshot, ReadError};

const MAX_UNITS: usize = 16_384;

pub(super) struct NativeReader {
    automation: Result<IUIAutomation, i32>,
    initialized: bool,
    target: Option<(u64, u64, IUIAutomationElement)>,
}

impl NativeReader {
    pub(super) fn clear(&mut self) {
        self.target = None;
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
        }
    }

    pub(super) fn capture(
        &mut self,
        generation: u64,
        expected_pid: u32,
    ) -> Result<OwnedSnapshot, ReadError> {
        self.target = None;
        let automation = self
            .automation
            .as_ref()
            .map_err(|e| ReadError::Native(*e))?;
        let element = unsafe { automation.GetFocusedElement() }.map_err(native)?;
        if unsafe { element.CurrentProcessId() }.map_err(native)? as u32 != expected_pid {
            return Err(ReadError::TargetChanged);
        }
        check_control(&element)?;
        self.target = Some((generation, super::target_token(), element));
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
        let pattern =
            unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }
                .map_err(native)?;
        let document = unsafe { pattern.DocumentRange() }.map_err(native)?;
        // GetText is bounded at the UIA provider; never request an unlimited document.
        let text = bounded_text(
            unsafe { document.GetText((MAX_UNITS + 1) as i32) }
                .map_err(native)?
                .to_string(),
        )?;
        let selections = unsafe { pattern.GetSelection() }.map_err(native)?;
        if unsafe { selections.Length() }.map_err(native)? != 1 {
            return Err(ReadError::Unsupported);
        }
        let selected = unsafe { selections.GetElement(0) }.map_err(native)?;
        let prefix_start = unsafe { document.Clone() }.map_err(native)?;
        unsafe {
            prefix_start.MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &selected,
                TextPatternRangeEndpoint_Start,
            )
        }
        .map_err(native)?;
        let prefix_end = unsafe { document.Clone() }.map_err(native)?;
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
        same_focus(automation, element)?;
        check_control(element)?;
        Ok(OwnedSnapshot {
            target: *token,
            process_id: unsafe { element.CurrentProcessId() }.map_err(native)? as u32,
            text,
            selection_utf16: start_text.encode_utf16().count()..end_text.encode_utf16().count(),
            read_at: Instant::now(),
            event_driven: false,
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
