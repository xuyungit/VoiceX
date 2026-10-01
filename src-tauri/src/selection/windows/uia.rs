//! UI Automation: the Windows counterpart of the macOS Accessibility read.
//!
//! One question is asked — what `TextPattern::GetSelection` says on the
//! focused element — with one twist: the focused element is often not the one
//! that owns the text. In a browser it is a link or the page itself; in Word
//! it is the document but in a web view it can be a node several levels down.
//! So the read walks up the control view to the nearest ancestor that has the
//! pattern. UI Automation interfaces never leave this module.
//!
//! Every call happens on the selection worker thread, which joins the
//! multithreaded COM apartment once and keeps its UI Automation client for the
//! life of the process (see [`crate::selection`]).

use std::cell::RefCell;

use windows::core::{Interface, HRESULT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationElement, IUIAutomationTextPattern,
    UIA_TextPatternId,
};

use crate::selection::windows_rules::{control_type_name, join_selection_ranges};

/// How long UI Automation waits for a target to answer, per call. The
/// defaults (2 s to connect, 20 s per transaction) would let one hung
/// application hold the read — and with it the HUD's "preparing" — for half a
/// minute. A responsive provider answers in milliseconds.
const CONNECTION_TIMEOUT_MS: u32 = 1_000;
const TRANSACTION_TIMEOUT_MS: u32 = 1_500;

/// How far up the tree the pattern is looked for. A focused link in a web
/// page can sit a dozen levels below the document that owns the selection;
/// past this the element is not inside a text control at all, and each step
/// is a cross-process call.
const MAX_PATTERN_DEPTH: u32 = 32;

thread_local! {
    /// The worker's UI Automation client, created on first use.
    static AUTOMATION: RefCell<Option<IUIAutomation>> = const { RefCell::new(None) };
}

/// The worker thread's client. Joins the multithreaded apartment the first
/// time; the thread lives as long as the process, so the apartment is never
/// left.
fn automation() -> windows::core::Result<IUIAutomation> {
    AUTOMATION.with(|slot| {
        if let Some(automation) = slot.borrow().as_ref() {
            return Ok(automation.clone());
        }
        // S_FALSE ("already initialized") is success; a thread that is
        // already single-threaded comes back as RPC_E_CHANGED_MODE.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }?;
        let bounded = automation
            .cast::<IUIAutomation2>()
            .and_then(|automation| unsafe {
                automation.SetConnectionTimeout(CONNECTION_TIMEOUT_MS)?;
                automation.SetTransactionTimeout(TRANSACTION_TIMEOUT_MS)
            });
        if let Err(err) = bounded {
            // Reading still works, just with the long default timeouts.
            log::warn!("UI Automation timeouts not applied: {err}");
        }
        *slot.borrow_mut() = Some(automation.clone());
        Ok(automation)
    })
}

/// What the `TextPattern` read produced. `Empty` and `Unsupported` are
/// distinct for the same reason as on macOS: the former is a control saying
/// nothing is selected, the latter is a control that cannot say.
pub enum TextRead {
    Text(String),
    Empty,
    /// No element up to [`MAX_PATTERN_DEPTH`] offers a `TextPattern`.
    NoPattern,
    /// A call failed; carries the HRESULT (timeout, element gone, access
    /// denied, …).
    Failed(i32),
}

impl TextRead {
    /// Same vocabulary as the macOS `AttributeRead::kind`, so the probe reads
    /// the same on both platforms.
    pub fn kind(&self) -> &'static str {
        match self {
            TextRead::Text(_) => "text",
            TextRead::Empty => "empty",
            TextRead::NoPattern | TextRead::Failed(_) => "unsupported",
        }
    }

    pub fn status(&self) -> Option<i32> {
        match self {
            TextRead::Text(_) | TextRead::Empty => Some(0),
            TextRead::NoPattern => None,
            TextRead::Failed(status) => Some(*status),
        }
    }
}

/// The focused element, as UI Automation reports it system-wide.
pub struct FocusedElement {
    automation: IUIAutomation,
    element: IUIAutomationElement,
}

impl FocusedElement {
    /// `Ok(None)` when nothing has keyboard focus; `Err` carries the HRESULT
    /// when UI Automation itself could not be reached or refused.
    pub fn read() -> Result<Option<Self>, i32> {
        let automation = automation().map_err(|err| err.code().0)?;
        match unsafe { automation.GetFocusedElement() } {
            Ok(element) => Ok(Some(Self {
                automation,
                element,
            })),
            Err(err) if err.code() == HRESULT(0) => Ok(None),
            Err(err) => Err(err.code().0),
        }
    }

    pub fn control_type(&self) -> Option<String> {
        unsafe { self.element.CurrentControlType() }
            .ok()
            .map(|id| control_type_name(id.0))
    }

    pub fn class_name(&self) -> Option<String> {
        unsafe { self.element.CurrentClassName() }
            .ok()
            .map(|name| name.to_string())
            .filter(|name| !name.is_empty())
    }

    pub fn framework_id(&self) -> Option<String> {
        unsafe { self.element.CurrentFrameworkId() }
            .ok()
            .map(|name| name.to_string())
            .filter(|name| !name.is_empty())
    }

    /// Whether the focused control is a password field. The selection there
    /// is masked or empty, and the clipboard is likely what goes into it.
    pub fn is_password(&self) -> bool {
        unsafe { self.element.CurrentIsPassword() }.is_ok_and(|value| value.as_bool())
    }

    /// The selected text of the focused element or its nearest ancestor with
    /// a `TextPattern`, and how many levels up that was.
    pub fn selected_text(&self) -> (TextRead, Option<u32>) {
        let walker = match unsafe { self.automation.ControlViewWalker() } {
            Ok(walker) => walker,
            Err(err) => return (TextRead::Failed(err.code().0), None),
        };

        let mut current = self.element.clone();
        for depth in 0..=MAX_PATTERN_DEPTH {
            match unsafe {
                current.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
            } {
                Ok(pattern) => return (read_selection(&pattern), Some(depth)),
                // A null pattern is "not supported here": keep climbing.
                Err(err) if err.code() == HRESULT(0) => {}
                // Anything else — a timeout above all — would only repeat on
                // every ancestor, each costing the full transaction timeout.
                Err(err) => return (TextRead::Failed(err.code().0), None),
            }
            current = match unsafe { walker.GetParentElement(&current) } {
                Ok(parent) => parent,
                // Past the root.
                Err(err) if err.code() == HRESULT(0) => break,
                Err(err) => return (TextRead::Failed(err.code().0), None),
            };
        }
        (TextRead::NoPattern, None)
    }
}

fn read_selection(pattern: &IUIAutomationTextPattern) -> TextRead {
    let read = || -> windows::core::Result<String> {
        let ranges = unsafe { pattern.GetSelection() }?;
        let count = unsafe { ranges.Length() }?;
        let mut texts = Vec::with_capacity(count.max(0) as usize);
        for index in 0..count {
            let range = unsafe { ranges.GetElement(index) }?;
            // -1: no length cap. A capped read would cut a long selection off
            // silently, which is worse than taking a moment longer.
            texts.push(unsafe { range.GetText(-1) }?.to_string());
        }
        Ok(join_selection_ranges(texts))
    };
    match read() {
        Ok(text) if text.is_empty() => TextRead::Empty,
        Ok(text) => TextRead::Text(text),
        // A null range array or range is no selection either, just reported
        // less tidily than an empty one.
        Err(err) if err.code() == HRESULT(0) => TextRead::Empty,
        Err(err) => TextRead::Failed(err.code().0),
    }
}
