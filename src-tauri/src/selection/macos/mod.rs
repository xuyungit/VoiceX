//! macOS selection reader: layered Accessibility read with a Copy fallback.

mod ax;
mod clipboard;

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::foreground_app;
use crate::selection::{
    normalize_text, SelectionError, SelectionOutcome, SelectionProbe, SelectionRequest,
    SelectionSource,
};
use crate::tts::log_event;

use ax::{AttributeRead, FocusedElement, SelectionAttributes};

pub fn read_selection(
    request: &SelectionRequest,
    probe: &mut SelectionProbe,
) -> Result<SelectionOutcome, SelectionError> {
    let started = Instant::now();

    // Freeze the foreground application before touching anything else, so every
    // later decision refers to the app that was frontmost when the hotkey fired.
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

    if !crate::hotkey::HotkeyPermissionStatus::detect().accessibility {
        return Err(SelectionError::PermissionDenied);
    }

    let mut focused = match FocusedElement::read() {
        Ok(focused) => focused,
        Err(_api_disabled) => return Err(SelectionError::PermissionDenied),
    };

    // An Electron application that has not been asked to build its
    // accessibility tree answers with nothing at all. Asking costs one call and
    // saves the clipboard fallback, which is the only path that touches what
    // the user copied. Gated on the read having actually come up empty, so a
    // healthy application is never poked.
    if focused.is_none() && ax::needs_manual_accessibility(app_bundle_id.as_deref()) {
        if enable_manual_accessibility_once(app_pid) {
            probe.enabled_manual_accessibility = true;
            focused = FocusedElement::read().unwrap_or(None);
            log_event(
                "selection_ax_enabled",
                &[
                    ("app", app_bundle_id.clone().unwrap_or_default()),
                    (
                        "focused",
                        if focused.is_some() { "yes" } else { "no" }.to_string(),
                    ),
                ],
            );
        }
    }

    // Distinguishes "the control says nothing is selected" from "the control
    // does not answer", which decides how a silent copy is reported.
    let mut ax_reported_empty = false;

    if let Some(element) = focused.as_ref() {
        probe.focused_role = element.role().map(str::to_string);
        probe.focused_subrole = element.subrole().map(str::to_string);

        let read = element.selected_text();
        probe.ax_attribute = Some(read.kind().to_string());
        probe.ax_status = Some(read.status());

        // Fast path first: when the Accessibility layer hands over text there is
        // nothing left to diagnose, and the probe below costs another round trip.
        if let AttributeRead::Text(raw) = &read {
            let text = normalize_text(raw);
            if !text.is_empty() {
                log_ax_probe(element, &read, None);
                return finish(text, SelectionSource::Ax, None);
            }
        }

        // Everything from here on is a fallback, so spend the extra call: which
        // attributes the element advertises is what decides whether the phase-1
        // range layer can help this application at all (plan §5).
        let attributes = element.selection_attributes();
        if attributes.enumerated {
            probe.advertises_selected_text = Some(attributes.selected_text);
            probe.advertises_selected_text_range = Some(attributes.selected_text_range);
            probe.advertises_marker_range = Some(attributes.selected_text_marker_range);
        }
        log_ax_probe(element, &read, Some(attributes));

        match read {
            AttributeRead::Text(_) | AttributeRead::Empty(_) => ax_reported_empty = true,
            AttributeRead::ApiDisabled => return Err(SelectionError::PermissionDenied),
            AttributeRead::Unsupported(_) => {}
        }

        // WebKit layer. A web area advertises only the marker range, and its
        // `AXSelectedText` answer above is `kAXErrorNoValue` regardless of
        // whether text is selected — so that "empty" was never evidence, and
        // the marker read below is the first one that is (plan §5.1).
        if attributes.selected_text_marker_range {
            let marker = element.selected_text_via_marker_range();
            probe.marker_attribute = Some(marker.kind().to_string());
            probe.marker_status = Some(marker.status());
            log_event(
                "selection_ax_marker",
                &[
                    ("role", element.role().unwrap_or_default().to_string()),
                    ("attr", marker.kind().to_string()),
                    ("status", marker.status().to_string()),
                ],
            );
            match marker {
                AttributeRead::Text(raw) => {
                    let text = normalize_text(&strip_object_replacements(&raw));
                    if !text.is_empty() {
                        return finish(text, SelectionSource::AxMarkerRange, None);
                    }
                    ax_reported_empty = true;
                }
                AttributeRead::Empty(_) => ax_reported_empty = true,
                AttributeRead::ApiDisabled => return Err(SelectionError::PermissionDenied),
                AttributeRead::Unsupported(_) => {}
            }
        }
    } else {
        log_event("selection_ax", &[("focused", "none".to_string())]);
    }

    if !request.allow_clipboard_fallback {
        return Err(if ax_reported_empty {
            SelectionError::NoSelection
        } else {
            SelectionError::UnsupportedControl
        });
    }

    // Only the copy path cares: secure keyboard entry makes the window server
    // drop synthetic key events, so the Cmd-C below would never arrive and the
    // read would fail as a copy timeout 300 ms later. Checking here turns that
    // into an immediate, explainable error. Not a privacy rule — the
    // Accessibility path above ran unconditionally (plan §3.4).
    if ax::secure_event_input_enabled() {
        return Err(SelectionError::SecureInput);
    }

    probe.used_clipboard_fallback = true;
    let copied = match clipboard::read_via_copy(request, app_pid) {
        Ok(copied) => copied,
        // A copy that changes nothing is indistinguishable from an empty
        // selection at the pasteboard level; trust the Accessibility layer when
        // it already told us the selection was empty.
        Err(SelectionError::CopyTimeout) if ax_reported_empty => {
            return Err(SelectionError::NoSelection)
        }
        Err(err) => return Err(err),
    };

    if !copied.restored {
        // The user's clipboard now holds the copied selection. Loud on purpose:
        // reported up through the outcome so it lands in the structured log
        // rather than only in a debug line nobody reads.
        log::warn!("Clipboard was not restored after the copy fallback");
    }

    let text = normalize_text(&copied.text);
    if text.is_empty() {
        return Err(SelectionError::NoSelection);
    }

    finish(text, SelectionSource::ClipboardCopy, Some(copied.restored))
}

/// Drop the U+FFFC placeholders WebKit emits for replaced elements.
///
/// `AXStringForTextMarkerRange` stands in an object replacement character for
/// every image, attachment or embedded frame inside the range. Those are not
/// text the user selected, and a speech backend either reads them as garbage
/// or rejects the request; nothing else in the pipeline expects them.
fn strip_object_replacements(raw: &str) -> String {
    raw.replace('\u{FFFC}', "")
}

/// Set `AXManualAccessibility` at most once per process.
///
/// Repeating it every read would be wasted calls, and more importantly it would
/// keep re-announcing a change to another application's state that we already
/// made. Keyed on pid, so a relaunched application is asked again — the new
/// process has its own tree.
fn enable_manual_accessibility_once(pid: u32) -> bool {
    static ASKED: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
    let asked = ASKED.get_or_init(|| Mutex::new(HashSet::new()));

    let Ok(mut asked) = asked.lock() else {
        return false;
    };
    if !asked.insert(pid) {
        // Already asked and it did not help; falling through to Copy is the
        // answer, not asking again.
        return false;
    }
    ax::enable_manual_accessibility(pid as i32)
}

/// Record what the Accessibility layer said about the focused control.
///
/// Diagnostic only — no decision reads this. It exists because "AX did not
/// return text" hides several very different situations, and phase 1 needs to
/// know which one each application is in before deciding whether a range layer
/// is worth building (plan §5). `attributes` is `None` on the success path,
/// where the extra round trip would buy nothing.
///
/// Roles and attribute names are control metadata, never content, so this is
/// safe at the ordinary log level under §3.4.
fn log_ax_probe(
    element: &FocusedElement,
    read: &AttributeRead,
    attributes: Option<SelectionAttributes>,
) {
    let mut fields = vec![
        ("role", element.role().unwrap_or_default().to_string()),
        ("subrole", element.subrole().unwrap_or_default().to_string()),
        ("attr", read.kind().to_string()),
        ("status", read.status().to_string()),
    ];
    if let Some(attributes) = attributes {
        fields.push(("enumerated", attributes.enumerated.to_string()));
        fields.push(("has_sel_text", attributes.selected_text.to_string()));
        fields.push(("has_sel_range", attributes.selected_text_range.to_string()));
        fields.push((
            "has_marker_range",
            attributes.selected_text_marker_range.to_string(),
        ));
        fields.push(("has_value", attributes.value.to_string()));
    }
    log_event("selection_ax", &fields);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webkit_object_replacement_characters_do_not_reach_the_speech_backend() {
        // A Safari selection spanning an inline image comes back as
        // "… image \u{FFFC} inline …"; the placeholder is layout, not text.
        assert_eq!(
            strip_object_replacements("an image \u{FFFC} inline"),
            "an image  inline"
        );
        assert_eq!(strip_object_replacements("\u{FFFC}"), "");
        assert_eq!(strip_object_replacements("plain"), "plain");
    }
}
