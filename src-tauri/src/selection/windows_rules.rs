//! The decisions of the Windows selection reader that involve no Win32 call.
//!
//! The reader itself (`selection/windows/`) only builds for Windows. These
//! rules are compiled everywhere so their tests run on whichever machine a
//! developer has — the clipboard rules especially, since they are the
//! fail-closed contract (plan §3.4) and nothing else pins them down.

use super::SelectionError;

/// Predefined clipboard formats the snapshot treats specially. Numbers from
/// `WinUser.h`; spelled out so this file needs no Windows bindings.
pub mod cf {
    pub const BITMAP: u32 = 2;
    pub const METAFILEPICT: u32 = 3;
    pub const DIB: u32 = 8;
    pub const PALETTE: u32 = 9;
    pub const ENHMETAFILE: u32 = 14;
    pub const DIBV5: u32 = 17;
    pub const OWNERDISPLAY: u32 = 0x0080;
    pub const DSPBITMAP: u32 = 0x0082;
    pub const DSPMETAFILEPICT: u32 = 0x0083;
    pub const DSPENHMETAFILE: u32 = 0x008E;
    pub const PRIVATE: std::ops::RangeInclusive<u32> = 0x0200..=0x02FF;
    pub const GDIOBJ: std::ops::RangeInclusive<u32> = 0x0300..=0x03FF;
}

/// Total snapshot budget, the same as on macOS. Beyond this the fallback is
/// refused rather than holding (and later rewriting) a huge clipboard.
pub const MAX_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;

/// How one clipboard format is captured for the restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatPlan {
    /// Global memory: copy the bytes, write the same bytes back.
    Memory,
    /// `CF_ENHMETAFILE` is a GDI handle, not memory; captured as its bits.
    EnhMetaFile,
    /// The system rebuilds it on demand from a format that is captured, so
    /// capturing it too would only hold a second copy — for an image, a
    /// second copy of a screenshot.
    Synthesized,
    /// Nothing the reader can capture and put back faithfully. The copy
    /// fallback is refused before it touches the clipboard.
    Refuse(&'static str),
}

/// Decide, for every format on the clipboard, how the snapshot captures it.
///
/// `formats` must be in `EnumClipboardFormats` order: the writer's own
/// formats first, then the ones the system synthesizes. That order is what
/// tells a native bitmap format from its synthesized twin.
pub fn plan_formats(formats: &[u32]) -> Vec<(u32, FormatPlan)> {
    let has = |format: u32| formats.contains(&format);
    // Of the two device-independent bitmap formats the system synthesizes
    // each from the other, so one is enough: the first listed, which is the
    // writer's own whenever the writer placed one.
    let kept_dib = formats
        .iter()
        .copied()
        .find(|&format| format == cf::DIB || format == cf::DIBV5);

    formats
        .iter()
        .map(|&format| {
            let plan = match format {
                cf::DIB | cf::DIBV5 if Some(format) == kept_dib => FormatPlan::Memory,
                cf::DIB | cf::DIBV5 => FormatPlan::Synthesized,
                // A bitmap or palette handle is rebuilt from the DIB.
                cf::BITMAP | cf::PALETTE if kept_dib.is_some() => FormatPlan::Synthesized,
                cf::BITMAP | cf::PALETTE => {
                    FormatPlan::Refuse("a bitmap handle with no device-independent copy")
                }
                cf::ENHMETAFILE => FormatPlan::EnhMetaFile,
                // The old metafile and the enhanced one are synthesized from
                // each other, and only the enhanced one has a byte form.
                cf::METAFILEPICT if has(cf::ENHMETAFILE) => FormatPlan::Synthesized,
                cf::METAFILEPICT => FormatPlan::Refuse("a metafile with no enhanced copy"),
                cf::OWNERDISPLAY => FormatPlan::Refuse("owner-drawn clipboard content"),
                cf::DSPBITMAP | cf::DSPMETAFILEPICT | cf::DSPENHMETAFILE => {
                    FormatPlan::Refuse("a display format backed by a GDI handle")
                }
                format if cf::PRIVATE.contains(&format) => {
                    FormatPlan::Refuse("a private-handle format")
                }
                format if cf::GDIOBJ.contains(&format) => FormatPlan::Refuse("a GDI object format"),
                // Every other predefined format and every registered one
                // ("HTML Format", "Rich Text Format", "PNG", …) is global
                // memory by contract. One that turns out not to be is caught
                // when the capture cannot lock it.
                _ => FormatPlan::Memory,
            };
            (format, plan)
        })
        .collect()
}

/// Add one format's payload to the running snapshot size, refusing past the
/// budget. Saturating on purpose — an overflow that wrapped would read as
/// "plenty of room left".
pub fn accumulate_snapshot_bytes(total: usize, added: usize) -> Result<usize, SelectionError> {
    let total = total.saturating_add(added);
    if total > MAX_SNAPSHOT_BYTES {
        return Err(SelectionError::ClipboardSnapshotRefused(format!(
            "clipboard exceeds {MAX_SNAPSHOT_BYTES} bytes"
        )));
    }
    Ok(total)
}

/// Whether the foreground process is out of reach because of its elevation.
///
/// User Interface Privilege Isolation stops a normal process from sending
/// input to, or reading the UI of, an elevated one — both the UI Automation
/// read and the synthesized Ctrl+C would fail, the latter silently. Only a
/// definite "target elevated, VoiceX not" blocks: when either side could not
/// be determined the read is attempted, and fails on its own terms if it must.
pub fn elevation_blocks_reading(target_elevated: Option<bool>, own_elevated: Option<bool>) -> bool {
    target_elevated == Some(true) && own_elevated == Some(false)
}

/// The text of a UI Automation selection. Most controls report one range;
/// Word and a few others allow several disjoint ones, read in order, one per
/// line. An empty range is the caret, not a selection.
pub fn join_selection_ranges(ranges: Vec<String>) -> String {
    ranges
        .into_iter()
        .filter(|range| !range.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The name of a UI Automation control type id, for the diagnostic probe —
/// the counterpart of the macOS `AXRole`.
pub fn control_type_name(id: i32) -> String {
    const NAMES: [&str; 41] = [
        "Button",
        "Calendar",
        "CheckBox",
        "ComboBox",
        "Edit",
        "Hyperlink",
        "Image",
        "ListItem",
        "List",
        "Menu",
        "MenuBar",
        "MenuItem",
        "ProgressBar",
        "RadioButton",
        "ScrollBar",
        "Slider",
        "Spinner",
        "StatusBar",
        "Tab",
        "TabItem",
        "Text",
        "ToolBar",
        "ToolTip",
        "Tree",
        "TreeItem",
        "Custom",
        "Group",
        "Thumb",
        "DataGrid",
        "DataItem",
        "Document",
        "SplitButton",
        "Window",
        "Pane",
        "Header",
        "HeaderItem",
        "Table",
        "TitleBar",
        "Separator",
        "SemanticZoom",
        "AppBar",
    ];
    id.checked_sub(50_000)
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| NAMES.get(index))
        .map(|name| name.to_string())
        .unwrap_or_else(|| format!("ControlType{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML_FORMAT: u32 = 0xC0F1;
    const UNICODETEXT: u32 = 13;
    const TEXT: u32 = 1;
    const LOCALE: u32 = 16;

    fn plan_of(plans: &[(u32, FormatPlan)], format: u32) -> FormatPlan {
        plans
            .iter()
            .find(|(candidate, _)| *candidate == format)
            .map(|(_, plan)| *plan)
            .unwrap()
    }

    #[test]
    fn text_and_registered_formats_are_captured_as_memory() {
        // A browser copy: HTML first, then text and what the system adds.
        let formats = [HTML_FORMAT, UNICODETEXT, LOCALE, TEXT];
        let plans = plan_formats(&formats);
        assert!(
            plans.iter().all(|(_, plan)| *plan == FormatPlan::Memory),
            "{plans:?}"
        );
        assert_eq!(
            plans.iter().map(|(format, _)| *format).collect::<Vec<_>>(),
            formats,
            "the restore writes formats back in the order they were offered"
        );
    }

    #[test]
    fn a_screenshot_is_held_once_not_three_times() {
        // A screenshot tool places a DIB; the system lists the bitmap handle,
        // the V5 header and the palette as synthesized from it.
        let plans = plan_formats(&[cf::DIB, cf::BITMAP, cf::PALETTE, cf::DIBV5]);
        assert_eq!(plan_of(&plans, cf::DIB), FormatPlan::Memory);
        assert_eq!(plan_of(&plans, cf::DIBV5), FormatPlan::Synthesized);
        assert_eq!(plan_of(&plans, cf::BITMAP), FormatPlan::Synthesized);
        assert_eq!(plan_of(&plans, cf::PALETTE), FormatPlan::Synthesized);
    }

    #[test]
    fn the_writers_own_bitmap_format_is_the_one_kept() {
        // A writer that placed V5 (with alpha) must not be restored as the
        // plain DIB the system made from it.
        let plans = plan_formats(&[cf::DIBV5, cf::BITMAP, cf::DIB]);
        assert_eq!(plan_of(&plans, cf::DIBV5), FormatPlan::Memory);
        assert_eq!(plan_of(&plans, cf::DIB), FormatPlan::Synthesized);
    }

    #[test]
    fn a_native_bitmap_handle_is_captured_through_its_synthesized_dib() {
        let plans = plan_formats(&[cf::BITMAP, cf::DIB, cf::DIBV5]);
        assert_eq!(plan_of(&plans, cf::BITMAP), FormatPlan::Synthesized);
        assert_eq!(plan_of(&plans, cf::DIB), FormatPlan::Memory);
    }

    #[test]
    fn metafiles_are_captured_through_the_enhanced_form() {
        let plans = plan_formats(&[cf::ENHMETAFILE, cf::METAFILEPICT]);
        assert_eq!(plan_of(&plans, cf::ENHMETAFILE), FormatPlan::EnhMetaFile);
        assert_eq!(plan_of(&plans, cf::METAFILEPICT), FormatPlan::Synthesized);
    }

    #[test]
    fn handle_formats_with_no_byte_form_refuse_the_fallback() {
        // Fail closed: content that cannot be put back must not be cleared.
        for format in [
            cf::OWNERDISPLAY,
            cf::DSPBITMAP,
            cf::DSPENHMETAFILE,
            0x0200,
            0x02FF,
            0x0300,
            0x03FF,
        ] {
            assert!(
                matches!(
                    plan_of(&plan_formats(&[format]), format),
                    FormatPlan::Refuse(_)
                ),
                "{format:#06x}"
            );
        }
        // A lone handle with nothing the system could rebuild it from.
        assert!(matches!(
            plan_of(&plan_formats(&[cf::BITMAP]), cf::BITMAP),
            FormatPlan::Refuse(_)
        ));
        assert!(matches!(
            plan_of(&plan_formats(&[cf::METAFILEPICT]), cf::METAFILEPICT),
            FormatPlan::Refuse(_)
        ));
    }

    #[test]
    fn the_snapshot_budget_is_a_saturating_running_total() {
        assert_eq!(accumulate_snapshot_bytes(0, 1024).unwrap(), 1024);
        assert_eq!(
            accumulate_snapshot_bytes(MAX_SNAPSHOT_BYTES - 1, 1).unwrap(),
            MAX_SNAPSHOT_BYTES
        );
        let refused = accumulate_snapshot_bytes(MAX_SNAPSHOT_BYTES, 1).unwrap_err();
        assert_eq!(refused.code(), "clipboard_snapshot_refused");
        assert!(accumulate_snapshot_bytes(usize::MAX, usize::MAX).is_err());
    }

    #[test]
    fn only_a_definite_elevated_target_blocks_the_read() {
        assert!(elevation_blocks_reading(Some(true), Some(false)));
        // VoiceX elevated too: same privilege, nothing in the way.
        assert!(!elevation_blocks_reading(Some(true), Some(true)));
        assert!(!elevation_blocks_reading(Some(false), Some(false)));
        // Unknown on either side is not evidence.
        assert!(!elevation_blocks_reading(None, Some(false)));
        assert!(!elevation_blocks_reading(Some(true), None));
    }

    #[test]
    fn selection_ranges_are_read_in_order_and_carets_dropped() {
        assert_eq!(join_selection_ranges(vec!["one".to_string()]), "one");
        assert_eq!(
            join_selection_ranges(vec![
                "first".to_string(),
                String::new(),
                "second".to_string()
            ]),
            "first\nsecond"
        );
        assert_eq!(join_selection_ranges(vec![String::new()]), "");
        assert_eq!(join_selection_ranges(Vec::new()), "");
    }

    #[test]
    fn control_types_are_named_like_the_uia_constants() {
        assert_eq!(control_type_name(50_000), "Button");
        assert_eq!(control_type_name(50_004), "Edit");
        assert_eq!(control_type_name(50_030), "Document");
        assert_eq!(control_type_name(50_040), "AppBar");
        assert_eq!(control_type_name(50_041), "ControlType50041");
        assert_eq!(control_type_name(0), "ControlType0");
    }
}
