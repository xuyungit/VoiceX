//! Copy compatibility mode on Windows: snapshot the clipboard, synthesize
//! Ctrl+C, read the result, restore.
//!
//! The same contract as `selection/macos/clipboard.rs`, on Windows' own
//! primitives: the clipboard sequence number stands in for `changeCount`, and
//! the snapshot is every format `EnumClipboardFormats` lists rather than every
//! pasteboard item type. Like the macOS module it is deliberately not built on
//! `injector/clipboard.rs` — that path writes then pastes; this one reads what
//! another application writes.
//!
//! Fail-closed contract (plan §3.4): if any format cannot be captured in a
//! form that can be written back, the fallback is refused before the user's
//! clipboard is touched. Which formats those are is decided in
//! [`crate::selection::windows_rules::plan_formats`].
//!
//! One difference in the restore rule: the text is read and the snapshot put
//! back under a single `OpenClipboard`, and nobody can write to an open
//! clipboard, so there is no window between the two for a clipboard manager
//! to write into — the window macOS has to check `changeCount` across.

use std::mem::size_of;
use std::ptr::null_mut;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{GetLastError, GlobalFree, HANDLE};
use windows_sys::Win32::Graphics::Gdi::{
    DeleteEnhMetaFile, GetEnhMetaFileBits, SetEnhMetaFileBits,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, VIRTUAL_KEY, VK_C, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN,
    VK_SHIFT,
};

use crate::selection::windows_rules::{accumulate_snapshot_bytes, plan_formats, FormatPlan};
use crate::selection::{SelectionError, SelectionRequest};
use crate::win_clipboard::{alloc_global, format_name, sequence_number, OpenClipboardGuard};

/// How long the target application gets to answer the copy (plan §4.3).
const COPY_TIMEOUT_MS: u64 = 300;
const COPY_POLL_INTERVAL_MS: u64 = 10;

/// After a copy timeout, how much longer the clipboard is watched for the
/// copy landing anyway. Same reasoning as macOS: a slow application still
/// performs the Ctrl+C it was sent, and without this the user's clipboard
/// would quietly end up holding the selection.
const LATE_COPY_GRACE_MS: u64 = 2_000;
const LATE_COPY_POLL_INTERVAL_MS: u64 = 25;

/// How long we wait for the user to let go of the hotkey before synthesizing a
/// copy. Ctrl+C sent while Alt is physically down arrives as Ctrl+Alt+C —
/// somebody else's shortcut, or AltGr+C on many keyboard layouts.
const MODIFIER_RELEASE_TIMEOUT_MS: u64 = 3_000;
const MODIFIER_POLL_INTERVAL_MS: u64 = 10;

/// How long opening the clipboard may wait for another process to close it
/// before the snapshot.
const SNAPSHOT_OPEN_BUDGET: Duration = Duration::from_millis(500);

/// Opening right after the copy is detected waits for the target application
/// to finish writing: the sequence number moves at its `EmptyClipboard`, and
/// it holds the clipboard open until its last `SetClipboardData`. More
/// generous than the snapshot's budget because failing here leaves the
/// selection on the clipboard instead of the user's content.
const READ_OPEN_BUDGET: Duration = Duration::from_millis(1_000);

/// Reading a format the owner offered with delayed rendering makes the owner
/// render it on the spot, and a spreadsheet that copied a large range can take
/// seconds per format. Past this, the snapshot is refused instead of holding
/// the read hostage to rendering the user's old clipboard.
const SNAPSHOT_TIME_BUDGET: Duration = Duration::from_millis(1_500);

/// The keys whose being held turns our Ctrl+C into a different shortcut.
const MODIFIER_KEYS: [VIRTUAL_KEY; 5] = [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN];

fn modifiers_held() -> bool {
    MODIFIER_KEYS
        .iter()
        .any(|&key| unsafe { GetAsyncKeyState(i32::from(key)) } as u16 & 0x8000 != 0)
}

fn wait_for_modifier_release(request: &SelectionRequest) -> Result<(), SelectionError> {
    let deadline = Instant::now() + Duration::from_millis(MODIFIER_RELEASE_TIMEOUT_MS);
    while modifiers_held() {
        // Cancellation must win over the wait, as on macOS: this is where a
        // stop hotkey or Escape lands while the chord is still held.
        if request.is_cancelled() {
            return Err(SelectionError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(SelectionError::ModifiersHeld);
        }
        thread::sleep(Duration::from_millis(MODIFIER_POLL_INTERVAL_MS));
    }
    Ok(())
}

fn refuse(reason: impl Into<String>) -> SelectionError {
    SelectionError::ClipboardSnapshotRefused(reason.into())
}

/// One captured format.
enum Captured {
    Memory(Vec<u8>),
    EnhMetaFile(Vec<u8>),
}

impl Captured {
    fn len(&self) -> usize {
        match self {
            Captured::Memory(bytes) | Captured::EnhMetaFile(bytes) => bytes.len(),
        }
    }
}

/// The user's clipboard, format by format, in the order it was offered — the
/// order applications pick the first format they understand in.
struct ClipboardSnapshot {
    formats: Vec<(u32, Captured)>,
}

impl ClipboardSnapshot {
    fn capture(clipboard: &OpenClipboardGuard) -> Result<Self, SelectionError> {
        let started = Instant::now();
        let listed = clipboard.formats().map_err(refuse)?;
        let mut formats = Vec::with_capacity(listed.len());
        let mut total_bytes = 0usize;

        for (format, plan) in plan_formats(&listed) {
            if started.elapsed() > SNAPSHOT_TIME_BUDGET {
                return Err(refuse(format!(
                    "the clipboard owner took over {} ms to render its formats",
                    SNAPSHOT_TIME_BUDGET.as_millis()
                )));
            }
            let captured = match plan {
                FormatPlan::Synthesized => continue,
                FormatPlan::Refuse(reason) => {
                    return Err(refuse(format!("{reason}: {}", format_name(format))))
                }
                FormatPlan::Memory => clipboard
                    .global_bytes(format)
                    .map(Captured::Memory)
                    // Delayed rendering whose owner refused or went away, or a
                    // registered format that is not global memory after all.
                    .ok_or_else(|| refuse(format!("unreadable format {}", format_name(format))))?,
                FormatPlan::EnhMetaFile => enh_metafile_bits(clipboard, format)
                    .map(Captured::EnhMetaFile)
                    .ok_or_else(|| refuse("unreadable enhanced metafile"))?,
            };
            total_bytes = accumulate_snapshot_bytes(total_bytes, captured.len())?;
            formats.push((format, captured));
        }

        Ok(Self { formats })
    }

    /// Put the captured contents back.
    ///
    /// Every handle is built *before* the clipboard is emptied: emptying is
    /// the destructive step, so whatever can fail ahead of it does so while
    /// the clipboard still holds something — here, the copied selection —
    /// rather than leaving it empty. Only `SetClipboardData` itself can still
    /// fail afterwards.
    fn restore(&self, clipboard: &OpenClipboardGuard) -> Result<(), String> {
        let prepared = self
            .formats
            .iter()
            .map(|(format, captured)| PreparedHandle::build(*format, captured))
            .collect::<Result<Vec<_>, _>>()?;

        clipboard.empty()?;
        for mut item in prepared {
            clipboard.set_handle(item.format, item.handle)?;
            // The clipboard owns it now.
            item.handle = null_mut();
        }
        Ok(())
    }
}

fn enh_metafile_bits(clipboard: &OpenClipboardGuard, format: u32) -> Option<Vec<u8>> {
    let handle = clipboard.data_handle(format)?;
    let size = unsafe { GetEnhMetaFileBits(handle, 0, null_mut()) };
    if size == 0 {
        return None;
    }
    let mut bits = vec![0u8; size as usize];
    let written = unsafe { GetEnhMetaFileBits(handle, size, bits.as_mut_ptr()) };
    (written == size).then_some(bits)
}

/// A handle built for the restore, freed on drop unless the clipboard took it.
struct PreparedHandle {
    format: u32,
    handle: HANDLE,
    enh_metafile: bool,
}

impl PreparedHandle {
    fn build(format: u32, captured: &Captured) -> Result<Self, String> {
        let (handle, enh_metafile) = match captured {
            Captured::Memory(bytes) => (alloc_global(bytes)?, false),
            Captured::EnhMetaFile(bits) => {
                let handle = unsafe { SetEnhMetaFileBits(bits.len() as u32, bits.as_ptr()) };
                if handle.is_null() {
                    return Err(format!("SetEnhMetaFileBits failed (error {})", unsafe {
                        GetLastError()
                    }));
                }
                (handle, true)
            }
        };
        Ok(Self {
            format,
            handle,
            enh_metafile,
        })
    }
}

impl Drop for PreparedHandle {
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        unsafe {
            if self.enh_metafile {
                DeleteEnhMetaFile(self.handle);
            } else {
                GlobalFree(self.handle);
            }
        }
    }
}

pub struct CopyReadOutcome {
    pub text: String,
    /// False when putting the snapshot back failed; the clipboard then holds
    /// the copied selection.
    pub restored: bool,
}

/// Synthesize Ctrl+C against the foreground application and read back the
/// plain text.
///
/// `expected_pid` is the process the selection was read from; the copy goes to
/// whatever is in front *now*, so that is re-verified immediately beforehand,
/// for the same reasons as on macOS.
///
/// Returns [`SelectionError::CopyTimeout`] when the clipboard never changed —
/// which is also what an empty selection looks like from here; the caller
/// disambiguates using what UI Automation already reported.
pub fn read_via_copy(
    request: &SelectionRequest,
    expected_pid: u32,
) -> Result<CopyReadOutcome, SelectionError> {
    wait_for_modifier_release(request)?;

    let current = crate::foreground_app::detect_foreground_app(&request.app)
        .map_err(|_| SelectionError::NoForegroundApp)?;
    if current.process_id != expected_pid {
        return Err(SelectionError::ForegroundChanged);
    }

    // Closed again before the copy: the target cannot write to a clipboard
    // somebody else holds open.
    let snapshot = {
        let clipboard = OpenClipboardGuard::open(SNAPSHOT_OPEN_BUDGET).map_err(refuse)?;
        ClipboardSnapshot::capture(&clipboard)?
    };
    // Taken after the capture: rendering a delayed format during the capture
    // is itself a clipboard write and moves the number.
    let sequence_before = sequence_number();

    post_copy_shortcut()?;

    let deadline = Instant::now() + Duration::from_millis(COPY_TIMEOUT_MS);
    loop {
        if sequence_number() != sequence_before {
            break;
        }
        if request.is_cancelled() {
            // The copy was already sent; nothing to undo — the clipboard has
            // not changed, so the user's content is untouched.
            return Err(SelectionError::Cancelled);
        }
        if Instant::now() >= deadline {
            // Nothing written *yet*. The Ctrl+C is still in the target's
            // queue, so keep watching and restore if it lands late.
            watch_for_late_copy(snapshot, sequence_before);
            return Err(SelectionError::CopyTimeout);
        }
        thread::sleep(Duration::from_millis(COPY_POLL_INTERVAL_MS));
    }

    let clipboard = OpenClipboardGuard::open(READ_OPEN_BUDGET).map_err(|err| {
        log::warn!(
            "Copy landed but the clipboard stayed locked; it holds the selection now: {err}"
        );
        SelectionError::Internal(format!("clipboard locked after the copy: {err}"))
    })?;
    let text = clipboard.unicode_text().unwrap_or_default();
    let restored = match snapshot.restore(&clipboard) {
        Ok(()) => true,
        Err(err) => {
            log::warn!("Failed to restore the clipboard after a copy fallback: {err}");
            false
        }
    };

    Ok(CopyReadOutcome { text, restored })
}

/// Keep watching the clipboard after a copy timeout and put the snapshot back
/// if the copy lands within the grace period. Runs on its own thread so the
/// failed read can report immediately; the outcome is logged either way.
fn watch_for_late_copy(snapshot: ClipboardSnapshot, sequence_before: u32) {
    let spawned = thread::Builder::new()
        .name("voicex-late-copy".to_string())
        .spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(LATE_COPY_GRACE_MS);
            let landed = loop {
                if sequence_number() != sequence_before {
                    break true;
                }
                if Instant::now() >= deadline {
                    break false;
                }
                thread::sleep(Duration::from_millis(LATE_COPY_POLL_INTERVAL_MS));
            };

            if !landed {
                crate::tts::log_event("copy_landed_late", &[("landed", "false".to_string())]);
                return;
            }

            let restored = match OpenClipboardGuard::open(READ_OPEN_BUDGET) {
                Ok(clipboard) => match snapshot.restore(&clipboard) {
                    Ok(()) => true,
                    Err(err) => {
                        log::warn!("Failed to restore the clipboard after a late copy: {err}");
                        false
                    }
                },
                Err(err) => {
                    log::warn!("Clipboard locked after a late copy; it holds the selection: {err}");
                    false
                }
            };
            crate::tts::log_event(
                "copy_landed_late",
                &[
                    ("landed", "true".to_string()),
                    ("restored", restored.to_string()),
                ],
            );
        });
    if let Err(err) = spawned {
        log::warn!("Could not watch for a late copy; the clipboard may hold the selection: {err}");
    }
}

/// One keyboard event for `SendInput`. The scan code goes along with the
/// virtual key: applications that look at scan codes (remote-desktop clients,
/// some Java and game-engine UIs) otherwise see a key with no physical
/// identity.
fn key_input(key: VIRTUAL_KEY, up: bool) -> INPUT {
    let scan = unsafe { MapVirtualKeyW(u32::from(key), MAPVK_VK_TO_VSC) } as u16;
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: scan,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Ctrl+C by virtual key, so it is Copy on every keyboard layout: Dvorak and
/// AZERTY move the C key, but accelerators match on `VK_C`, not on position.
/// One `SendInput` call keeps the four events contiguous — nothing the user
/// types can land between them.
fn post_copy_shortcut() -> Result<(), SelectionError> {
    let inputs = [
        key_input(VK_CONTROL, false),
        key_input(VK_C, false),
        key_input(VK_C, true),
        key_input(VK_CONTROL, true),
    ];
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    if sent as usize != inputs.len() {
        return Err(SelectionError::Internal(format!(
            "SendInput delivered {sent} of {} key events (error {})",
            inputs.len(),
            unsafe { GetLastError() }
        )));
    }
    Ok(())
}
