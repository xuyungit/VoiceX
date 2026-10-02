//! Windows selection reader: UI Automation first, Ctrl+C fallback.
//!
//! The same layering as macOS (plan §3.1), on Windows' own APIs:
//!
//! 1. UI Automation `TextPattern` selection of the focused element or its
//!    nearest ancestor with the pattern. No side effects. Covers the classic
//!    edit controls, Notepad, WordPad, Word, WPF and WinUI text, Windows
//!    Terminal and the console, and Chromium/Edge pages.
//! 2. Copy compatibility mode (when allowed): wait for the hotkey modifiers to
//!    clear, re-verify the foreground app, snapshot every clipboard format,
//!    Ctrl+C, wait for the clipboard sequence number to move, read, restore.
//!
//! Two Windows-only refusals come first. A password field ends the read as
//! `secure_input` — the counterpart of macOS secure keyboard entry. An
//! elevated foreground application, when VoiceX is not elevated, ends it as
//! `target_elevated`: User Interface Privilege Isolation would fail the UI
//! Automation read and silently drop the synthesized Ctrl+C, and the user
//! would see a copy timeout that explains nothing.

mod clipboard;
mod uia;

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::null_mut;
use std::time::Instant;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::foreground_app;
use crate::selection::windows_rules::elevation_blocks_reading;
use crate::selection::{
    normalize_text, SelectionError, SelectionOutcome, SelectionProbe, SelectionRequest,
    SelectionSource, SelectionTarget,
};
use crate::tts::log_event;

use uia::{FocusedElement, TextRead};

pub fn read_selection(
    request: &SelectionRequest,
    probe: &mut SelectionProbe,
) -> Result<SelectionOutcome, SelectionError> {
    let started = Instant::now();

    // A tray-menu read starts with VoiceX in front; the selection is in the
    // window the menu took the foreground from (see `SelectionTarget`).
    if request.target == SelectionTarget::BeforeTrayMenu {
        match foreground_app::reactivate_window_behind_tray_menu() {
            Ok(waited) => log_event(
                "selection_reactivate",
                &[("ms", waited.as_millis().to_string())],
            ),
            Err(detail) => {
                log_event("selection_reactivate", &[("error", detail)]);
                return Err(SelectionError::NoForegroundApp);
            }
        }
    }

    // Freeze the foreground application first, so every later decision
    // refers to the one that was in front when the hotkey fired.
    let app_info = foreground_app::detect_foreground_app(&request.app).map_err(|err| {
        log::debug!("Foreground app detection failed: {err}");
        SelectionError::NoForegroundApp
    })?;
    let app_bundle_id = app_info.bundle_id.clone();
    let app_name = app_info.display_name.clone();
    let app_pid = app_info.process_id;
    probe.app_bundle_id = app_bundle_id.clone();
    probe.app_name = app_name.clone();

    let finish = |text: String, source: SelectionSource, clipboard_restored: Option<bool>| {
        Ok(SelectionOutcome {
            text,
            source,
            app_bundle_id: app_bundle_id.clone(),
            app_name: app_name.clone(),
            elapsed_ms: started.elapsed().as_millis() as u64,
            clipboard_restored,
        })
    };

    if app_info.is_self {
        return Err(SelectionError::FocusIsSelf);
    }

    let target_elevated = process_elevation(app_pid);
    probe.target_elevated = target_elevated;
    if elevation_blocks_reading(target_elevated, own_elevation()) {
        log_event(
            "selection_uia",
            &[
                ("app", app_name.clone().unwrap_or_default()),
                ("elevated", "true".to_string()),
            ],
        );
        return Err(SelectionError::TargetElevated);
    }

    // Distinguishes "the control says nothing is selected" from "the control
    // does not answer", which decides how a silent copy is reported.
    let mut uia_reported_empty = false;

    match FocusedElement::read() {
        Ok(Some(element)) => {
            probe.focused_role = element.control_type();
            probe.focused_subrole = element.class_name();
            probe.focused_framework = element.framework_id();

            if element.is_password() {
                log_uia_probe(probe, None, Some(true));
                return Err(SelectionError::SecureInput);
            }

            let (read, depth) = element.selected_text();
            probe.text_pattern_depth = depth;
            probe.advertises_selected_text = Some(depth.is_some());
            probe.ax_attribute = Some(read.kind().to_string());
            probe.ax_status = read.status();
            log_uia_probe(probe, depth, None);

            match read {
                TextRead::Text(raw) => {
                    let text = normalize_text(&raw);
                    if !text.is_empty() {
                        return finish(text, SelectionSource::Uia, None);
                    }
                    uia_reported_empty = true;
                }
                TextRead::Empty => uia_reported_empty = true,
                TextRead::NoPattern | TextRead::Failed(_) => {}
            }
        }
        Ok(None) => log_event("selection_uia", &[("focused", "none".to_string())]),
        Err(status) => {
            probe.ax_attribute = Some("unsupported".to_string());
            probe.ax_status = Some(status);
            log_event(
                "selection_uia",
                &[
                    ("focused", "error".to_string()),
                    ("status", format!("{status:#010x}")),
                ],
            );
        }
    }

    if !request.allow_clipboard_fallback {
        return Err(if uia_reported_empty {
            SelectionError::NoSelection
        } else {
            SelectionError::UnsupportedControl
        });
    }

    probe.used_clipboard_fallback = true;
    let copied = match clipboard::read_via_copy(request, app_pid) {
        Ok(copied) => copied,
        // A copy that changes nothing is indistinguishable from an empty
        // selection at the clipboard level; trust UI Automation when it
        // already said the selection was empty.
        Err(SelectionError::CopyTimeout) if uia_reported_empty => {
            return Err(SelectionError::NoSelection)
        }
        Err(err) => return Err(err),
    };

    if !copied.restored {
        log::warn!("Clipboard was not restored after the copy fallback");
    }

    let text = normalize_text(&copied.text);
    if text.is_empty() {
        return Err(SelectionError::NoSelection);
    }

    finish(text, SelectionSource::ClipboardCopy, Some(copied.restored))
}

/// Record what UI Automation said about the focused control. Control
/// metadata only, never content (plan §3.4), like the macOS `selection_ax`.
fn log_uia_probe(probe: &SelectionProbe, depth: Option<u32>, password: Option<bool>) {
    let mut fields = vec![
        ("app", probe.app_name.clone().unwrap_or_default()),
        ("role", probe.focused_role.clone().unwrap_or_default()),
        ("class", probe.focused_subrole.clone().unwrap_or_default()),
        (
            "framework",
            probe.focused_framework.clone().unwrap_or_default(),
        ),
    ];
    if let Some(password) = password {
        fields.push(("password", password.to_string()));
    } else {
        fields.push(("attr", probe.ax_attribute.clone().unwrap_or_default()));
        fields.push((
            "status",
            probe
                .ax_status
                .map(|status| format!("{status:#010x}"))
                .unwrap_or_default(),
        ));
        fields.push((
            "depth",
            depth.map(|depth| depth.to_string()).unwrap_or_default(),
        ));
    }
    log_event("selection_uia", &fields);
}

/// Whether `pid` runs elevated. `None` when that cannot be determined — a
/// process of another user or a protected one refuses the query.
fn process_elevation(pid: u32) -> Option<bool> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let elevated = token_elevation(process);
        CloseHandle(process);
        elevated
    }
}

fn own_elevation() -> Option<bool> {
    // A pseudo-handle: nothing to close.
    unsafe { token_elevation(GetCurrentProcess()) }
}

unsafe fn token_elevation(process: HANDLE) -> Option<bool> {
    let mut token: HANDLE = null_mut();
    if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
        return None;
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut returned = 0u32;
    let ok = GetTokenInformation(
        token,
        TokenElevation,
        &mut elevation as *mut TOKEN_ELEVATION as *mut c_void,
        size_of::<TOKEN_ELEVATION>() as u32,
        &mut returned,
    );
    CloseHandle(token);
    (ok != 0).then_some(elevation.TokenIsElevated != 0)
}
