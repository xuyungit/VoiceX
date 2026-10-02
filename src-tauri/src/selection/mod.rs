//! Cross-application selected-text reading.
//!
//! This module is the platform-agnostic surface of the selection subsystem.
//! Platform types (AX element handles, `NSPasteboard`, UI Automation
//! interfaces, clipboard handles) never cross this boundary: the macOS and
//! Windows readers each provide the same `read_selection` entry point and
//! report through the same [`SelectionOutcome`] and [`SelectionProbe`].
//!
//! Reads are serialized on a dedicated worker thread: the macOS Accessibility
//! API is documented as single-threaded, the Windows reader keeps its COM
//! apartment and UI Automation client on that one thread, and serializing also
//! keeps the clipboard fallback from interleaving with itself.

use std::sync::{
    mpsc::{self, Sender, SyncSender},
    OnceLock,
};
use std::thread;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
// The Windows reader's pure decisions, compiled everywhere so their tests run
// on every development machine.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod windows_rules;

/// Which layer of the fallback chain produced the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionSource {
    /// `kAXSelectedTextAttribute` read directly off the focused element.
    Ax,
    /// WebKit's `AXSelectedTextMarkerRange` resolved through the parameterized
    /// `AXStringForTextMarkerRange`. Web areas advertise neither
    /// `AXSelectedText` nor `AXSelectedTextRange`, so this is the only
    /// Accessibility read that works for Safari (plan §5.1).
    AxMarkerRange,
    /// Windows: the UI Automation `TextPattern` selection of the focused
    /// element or its nearest ancestor that has one.
    Uia,
    /// Synthetic Cmd-C (macOS) or Ctrl+C (Windows) with clipboard
    /// snapshot/restore.
    ClipboardCopy,
}

impl SelectionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            SelectionSource::Ax => "ax",
            SelectionSource::AxMarkerRange => "ax_marker_range",
            SelectionSource::Uia => "uia",
            SelectionSource::ClipboardCopy => "clipboard_copy",
        }
    }
}

/// What the reader observed on its way through, successful or not.
///
/// Exists because the interesting detail — which control had focus, what it
/// advertised, which path was taken — only ever reached the structured log,
/// which means it is only visible to someone who launched the app from a
/// terminal. A failed read is exactly when that detail matters, so this is
/// collected on every path including the early returns.
///
/// Platform types never appear here: the macOS reader flattens its `AXError`
/// and attribute list into plain fields, and the Windows reader fills the same
/// shape from UI Automation (`focusedRole` is the control type, `axStatus` the
/// HRESULT). The few facts only one platform has are their own optional
/// fields, left `None` elsewhere.
#[derive(Debug, Default, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionProbe {
    pub app_bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub focused_role: Option<String>,
    /// macOS: `AXSubrole`. Windows: the UI Automation class name, which is
    /// what tells a Chromium render widget from a RichEdit from a console.
    pub focused_subrole: Option<String>,
    /// Windows only: the UI Automation framework id (`Win32`, `WPF`, `XAML`,
    /// `Chrome`, …).
    pub focused_framework: Option<String>,
    /// `text` | `empty` | `unsupported` | `api_disabled`, or `None` when there
    /// was no focused element to ask.
    pub ax_attribute: Option<String>,
    /// The raw platform status behind `ax_attribute`. Several very different
    /// causes collapse into one kind, and only this tells them apart.
    pub ax_status: Option<i32>,
    /// Which selection attributes the control advertises. `None` means we never
    /// got as far as asking.
    pub advertises_selected_text: Option<bool>,
    pub advertises_selected_text_range: Option<bool>,
    pub advertises_marker_range: Option<bool>,
    /// What the WebKit marker-range read produced, same vocabulary as
    /// `ax_attribute`. `None` when the control did not advertise the marker
    /// range and the layer was skipped.
    pub marker_attribute: Option<String>,
    pub marker_status: Option<i32>,
    /// Windows only: how many ancestors above the focused element the
    /// `TextPattern` was found — 0 is the focused element itself. `None` when
    /// no element on the way up had one.
    pub text_pattern_depth: Option<u32>,
    /// Windows only: whether the foreground process runs elevated, when that
    /// could be determined.
    pub target_elevated: Option<bool>,
    /// We asked this application to build its accessibility tree.
    pub enabled_manual_accessibility: bool,
    pub used_clipboard_fallback: bool,
}

/// A successful selection read.
#[derive(Debug, Clone)]
pub struct SelectionOutcome {
    pub text: String,
    pub source: SelectionSource,
    pub app_bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub elapsed_ms: u64,
    /// Only set for [`SelectionSource::ClipboardCopy`]: whether the user's
    /// clipboard was put back. `Some(false)` means their clipboard now holds
    /// the copied selection instead of what they had — surfaced in the
    /// structured log, and in the HUD once phase 3 adds status detail.
    pub clipboard_restored: Option<bool>,
}

/// Structured failure reasons. Every variant maps to a stable machine code via
/// [`SelectionError::code`], which the HUD and the automation harness key off.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SelectionError {
    #[error("No text is selected")]
    NoSelection,

    #[error("The focused control does not expose its selection")]
    UnsupportedControl,

    #[error("Accessibility permission is not granted")]
    PermissionDenied,

    /// macOS: secure keyboard entry is active. Windows: the focused control is
    /// a password field. Either way the selection cannot be read, and the
    /// clipboard is likely about to be pasted into it.
    #[error("Secure input is active, so the selection cannot be read")]
    SecureInput,

    /// Windows: the foreground application runs elevated and VoiceX does not.
    /// User Interface Privilege Isolation then blocks both the UI Automation
    /// read and the synthesized copy — the latter silently.
    #[error("The foreground application runs as administrator")]
    TargetElevated,

    #[error("The application did not respond to the copy command in time")]
    CopyTimeout,

    #[error("Refusing the clipboard fallback: {0}")]
    ClipboardSnapshotRefused(String),

    #[error("Hotkey modifiers are still held; cannot synthesize a copy")]
    ModifiersHeld,

    /// The session was cancelled while the read was in progress (stop hotkey,
    /// Escape, a superseding read). Never surfaced to the user: the caller
    /// discards the whole result of a cancelled read before looking at it.
    #[error("The read was cancelled")]
    Cancelled,

    #[error("The foreground application changed while reading the selection")]
    ForegroundChanged,

    #[error("VoiceX itself has focus")]
    FocusIsSelf,

    #[error("No foreground application")]
    NoForegroundApp,

    #[error("Selection reading is not supported on this platform")]
    PlatformUnsupported,

    #[error("Selection reading failed: {0}")]
    Internal(String),
}

impl SelectionError {
    pub fn code(&self) -> &'static str {
        match self {
            SelectionError::NoSelection => "no_selection",
            SelectionError::UnsupportedControl => "unsupported_control",
            SelectionError::PermissionDenied => "permission_denied",
            SelectionError::SecureInput => "secure_input",
            SelectionError::TargetElevated => "target_elevated",
            SelectionError::CopyTimeout => "copy_timeout",
            SelectionError::ClipboardSnapshotRefused(_) => "clipboard_snapshot_refused",
            SelectionError::ModifiersHeld => "modifiers_held",
            SelectionError::Cancelled => "cancelled",
            SelectionError::ForegroundChanged => "foreground_changed",
            SelectionError::FocusIsSelf => "focus_is_self",
            SelectionError::NoForegroundApp => "no_foreground_app",
            SelectionError::PlatformUnsupported => "platform_unsupported",
            SelectionError::Internal(_) => "internal",
        }
    }
}

/// Which application the selection is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionTarget {
    /// Whatever is in front right now — a hotkey press leaves the foreground
    /// where the user was working.
    Foreground,
    /// The application that was in front before the tray menu was opened.
    ///
    /// macOS: the same application as `Foreground`. A status-item menu is
    /// tracked without activating its owner, so the frontmost application
    /// keeps key focus — and with it the focused control and its selection —
    /// while the menu is open and after an item is chosen.
    ///
    /// Windows: not the foreground. The notification-area menu has to be
    /// owned by a foreground window to close on an outside click, so VoiceX
    /// takes the foreground before showing it, and the click on the taskbar
    /// had already taken it from the user's window before that. The reader
    /// gives the foreground back to that window before reading; the window
    /// keeps its selection while inactive and restores focus to the same
    /// control on reactivation.
    BeforeTrayMenu,
}

impl SelectionTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            SelectionTarget::Foreground => "foreground",
            SelectionTarget::BeforeTrayMenu => "before_tray_menu",
        }
    }
}

#[derive(Clone)]
pub struct SelectionRequest {
    pub app: tauri::AppHandle,
    /// Only Windows has to act on this; see [`SelectionTarget::BeforeTrayMenu`].
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub target: SelectionTarget,
    /// Compatibility mode: synthesize Cmd-C (Ctrl+C on Windows) when the
    /// accessibility path comes up empty. Subject to the fail-closed clipboard
    /// rules regardless.
    pub allow_clipboard_fallback: bool,
    /// Session cancellation, polled during the slow parts of a read — waiting
    /// for the hotkey modifiers to clear, waiting for the copy to land. A
    /// closure rather than the TTS session token, so this module keeps not
    /// knowing about the TTS subsystem. `None` (diagnostics) never cancels.
    pub cancelled: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl SelectionRequest {
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.as_ref().is_some_and(|check| check())
    }
}

type Job = (SelectionRequest, SyncSender<SelectionReport>);

fn worker() -> &'static Sender<Job> {
    static WORKER: OnceLock<Sender<Job>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name("voicex-selection".to_string())
            .spawn(move || {
                while let Ok((request, reply)) = rx.recv() {
                    let _ = reply.send(read_selection_on_worker(&request));
                }
            })
            .expect("failed to spawn the selection worker thread");
        tx
    })
}

fn read_selection_on_worker(request: &SelectionRequest) -> SelectionReport {
    #[cfg(target_os = "macos")]
    {
        let mut probe = SelectionProbe::default();
        let outcome = macos::read_selection(request, &mut probe);
        SelectionReport { outcome, probe }
    }

    #[cfg(target_os = "windows")]
    {
        let mut probe = SelectionProbe::default();
        let outcome = windows::read_selection(request, &mut probe);
        SelectionReport { outcome, probe }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = request;
        SelectionReport {
            outcome: Err(SelectionError::PlatformUnsupported),
            probe: SelectionProbe::default(),
        }
    }
}

/// A read plus everything observed while performing it.
pub struct SelectionReport {
    pub outcome: Result<SelectionOutcome, SelectionError>,
    pub probe: SelectionProbe,
}

/// Read the current selection from the foreground application.
///
/// Blocking. Must not be called from the main thread: the macOS implementation
/// hops to the main thread for AppKit queries and would deadlock, and the
/// Windows one makes cross-process UI Automation calls that must not run on a
/// thread with windows of its own.
pub fn read_selection(request: SelectionRequest) -> Result<SelectionOutcome, SelectionError> {
    read_selection_reporting(request).outcome
}

/// Read, and also report what was observed on the way.
///
/// Same code path as [`read_selection`] — deliberately, since a diagnostic that
/// exercises a parallel implementation proves nothing about the real one.
pub fn read_selection_reporting(request: SelectionRequest) -> SelectionReport {
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    let gone = |message: &str| SelectionReport {
        outcome: Err(SelectionError::Internal(message.to_string())),
        probe: SelectionProbe::default(),
    };

    if worker().send((request, reply_tx)).is_err() {
        return gone("selection worker is gone");
    }
    reply_rx
        .recv()
        .unwrap_or_else(|_| gone("selection worker dropped the request"))
}

/// Minimal normalization: CRLF/CR to LF, then trim.
///
/// Table-text and whitespace normalization proper lands in phase 1 together
/// with the fixtures that pin down the expected output.
pub fn normalize_text(raw: &str) -> String {
    raw.replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_folds_line_endings_and_trims() {
        assert_eq!(normalize_text("  hello\r\nworld\r  "), "hello\nworld");
    }

    #[test]
    fn the_probe_reports_no_text_only_facts_about_the_read() {
        // The report is meant to be pasted into a bug report. Nothing derived
        // from what the user had selected may appear in it — the length lives
        // on the outcome, and the text itself never leaves the process.
        let probe = SelectionProbe {
            app_bundle_id: Some("com.example.app".to_string()),
            focused_role: Some("AXTextArea".to_string()),
            ax_attribute: Some("empty".to_string()),
            ax_status: Some(-25212),
            used_clipboard_fallback: true,
            ..SelectionProbe::default()
        };
        let json = serde_json::to_string(&probe).unwrap();

        // Field names are what someone reads in a pasted report and what the
        // settings page renders, so renaming one is a breaking change.
        assert!(
            json.contains("\"appBundleId\":\"com.example.app\""),
            "{json}"
        );
        assert!(json.contains("\"focusedRole\":\"AXTextArea\""), "{json}");
        assert!(json.contains("\"axStatus\":-25212"), "{json}");
        assert!(json.contains("\"usedClipboardFallback\":true"), "{json}");
        assert!(
            !json.contains("text\":\""),
            "no selected text may appear: {json}"
        );
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(SelectionError::NoSelection.code(), "no_selection");
        assert_eq!(SelectionError::SecureInput.code(), "secure_input");
        assert_eq!(SelectionError::TargetElevated.code(), "target_elevated");
        assert_eq!(SelectionSource::Uia.as_str(), "uia");
        assert_eq!(SelectionError::Cancelled.code(), "cancelled");
        assert_eq!(
            SelectionError::ClipboardSnapshotRefused("x".to_string()).code(),
            "clipboard_snapshot_refused"
        );
    }
}
