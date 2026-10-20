//! Copy compatibility mode: snapshot the pasteboard, synthesize Cmd-C, read the
//! result, restore.
//!
//! Deliberately **not** built on `injector/clipboard.rs`. That path writes then
//! pastes; this one reads what another app writes, so the ordering, the
//! `changeCount` bookkeeping and the failure modes are different assumptions
//! and are kept as separate code.
//!
//! Fail-closed contract (plan §3.4): if any declared pasteboard type cannot be
//! captured, the fallback is refused outright rather than clobbering the user's
//! clipboard and hoping the restore works.

use std::thread;
use std::time::{Duration, Instant};

use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};

use crate::mac_pasteboard::{PasteboardSnapshot, Unrestorable};
use crate::selection::{SelectionError, SelectionRequest};

/// Physical keycodes for a layout-independent Command+C.
const MACOS_COMMAND_KEYCODE: u16 = 55;
const MACOS_C_KEYCODE: u16 = 8;

/// How long the target application gets to answer the copy (plan §4.3).
const COPY_TIMEOUT_MS: u64 = 300;
const COPY_POLL_INTERVAL_MS: u64 = 10;

/// After a copy timeout, how much longer the pasteboard is watched for the
/// copy landing anyway.
///
/// A timeout does not cancel the Cmd-C that was posted: an application busy
/// enough to miss the budget still performs the copy once it gets around to
/// it, and without this the user's clipboard would quietly end up holding the
/// selection with nobody left to put the snapshot back. The read itself has
/// already failed by then; this only settles what the clipboard is left with.
const LATE_COPY_GRACE_MS: u64 = 2_000;
const LATE_COPY_POLL_INTERVAL_MS: u64 = 25;

/// How long we wait for the user to let go of the hotkey before synthesizing a
/// copy. Posting Cmd-C while Option is physically down delivers Option+Cmd+C to
/// the target application, which is somebody else's shortcut.
///
/// Generous on purpose: lingering on the chord while waiting for the read to
/// audibly start is entirely natural, and at 800 ms it was the main way a
/// Safari read failed (`modifiers_held`). The HUD shows "preparing" for the
/// whole wait, and the wait itself is cancellable, so a long ceiling costs
/// nothing when the user lets go promptly.
const MODIFIER_RELEASE_TIMEOUT_MS: u64 = 3_000;
const MODIFIER_POLL_INTERVAL_MS: u64 = 10;

/// Total snapshot budget. Beyond this we refuse rather than hold (and rewrite)
/// very large clipboard payloads.
const MAX_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;

#[allow(non_snake_case)]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceFlagsState(state_id: u32) -> u64;
}

const K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION: u32 = 0;
/// Command | Alternate | Control | Shift
const MODIFIER_FLAG_MASK: u64 = 0x0010_0000 | 0x0008_0000 | 0x0004_0000 | 0x0002_0000;

fn modifiers_held() -> bool {
    let flags = unsafe { CGEventSourceFlagsState(K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION) };
    flags & MODIFIER_FLAG_MASK != 0
}

fn wait_for_modifier_release(request: &SelectionRequest) -> Result<(), SelectionError> {
    let deadline = Instant::now() + Duration::from_millis(MODIFIER_RELEASE_TIMEOUT_MS);
    while modifiers_held() {
        // Cancellation must win over the wait: this is where a stop hotkey or
        // Escape lands while the user is still holding the chord, and a wait
        // that cannot be interrupted would hold the session for seconds.
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

/// Reason a type could not be captured, for the structured error.
fn refuse(reason: impl Into<String>) -> SelectionError {
    SelectionError::ClipboardSnapshotRefused(reason.into())
}

/// Capture the whole pasteboard or refuse: anything that cannot be put back
/// faithfully means the copy fallback must not touch the clipboard.
fn capture_snapshot(pasteboard: &NSPasteboard) -> Result<PasteboardSnapshot, SelectionError> {
    PasteboardSnapshot::capture(pasteboard, MAX_SNAPSHOT_BYTES, Unrestorable::Refuse)
        .map_err(refuse)
}

/// Whether the snapshot may be written back.
///
/// Only when the pasteboard still holds exactly what our own copy put there.
/// A clipboard manager or another app writing in that window owns the
/// clipboard now, and restoring over it would destroy their write — the user
/// would see their clipboard silently revert.
fn may_restore(change_count_after_copy: isize, change_count_now: isize) -> bool {
    change_count_after_copy == change_count_now
}

pub struct CopyReadOutcome {
    pub text: String,
    /// False when a concurrent clipboard change made restoring unsafe.
    pub restored: bool,
}

/// Synthesize Cmd-C against the foreground app and read back the plain text.
///
/// `expected` is the application the selection was read from; the copy is
/// posted to whatever is frontmost *now*, so the caller's snapshot is
/// re-verified immediately beforehand. Waiting for the modifiers to clear can
/// take several seconds, which is ample time to switch windows, and posting
/// Cmd-C into an app we never inspected could both read the wrong content and
/// hit a control we never checked for secure input.
///
/// Returns [`SelectionError::CopyTimeout`] when the pasteboard never changed —
/// which is also what an empty selection looks like from here; the caller
/// disambiguates using what the Accessibility layer already reported.
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

    let pasteboard = NSPasteboard::generalPasteboard();
    let snapshot = capture_snapshot(&pasteboard)?;
    let change_count_before = pasteboard.changeCount();

    post_copy_shortcut()?;

    let deadline = Instant::now() + Duration::from_millis(COPY_TIMEOUT_MS);
    loop {
        if pasteboard.changeCount() != change_count_before {
            break;
        }
        if request.is_cancelled() {
            // The copy was already posted; nothing to undo — the pasteboard
            // has not changed, so the user's clipboard is untouched.
            return Err(SelectionError::Cancelled);
        }
        if Instant::now() >= deadline {
            // Nothing has been written *yet*. The Cmd-C is still in the
            // target's queue, so keep an eye on the pasteboard a little longer
            // and restore if the copy lands late.
            watch_for_late_copy(snapshot, change_count_before);
            return Err(SelectionError::CopyTimeout);
        }
        thread::sleep(Duration::from_millis(COPY_POLL_INTERVAL_MS));
    }

    let change_count_after_copy = pasteboard.changeCount();
    let text = unsafe { pasteboard.stringForType(NSPasteboardTypeString) }
        .map(|value| value.to_string())
        .unwrap_or_default();

    // Only restore if nothing else has touched the pasteboard since our copy;
    // a clipboard manager or another app must not lose its write.
    let restored = if may_restore(change_count_after_copy, pasteboard.changeCount()) {
        match snapshot.restore(&pasteboard, false) {
            Ok(()) => true,
            Err(err) => {
                log::warn!("Failed to restore the clipboard after a copy fallback: {err}");
                false
            }
        }
    } else {
        log::info!("Skipping clipboard restore: another process wrote to it during the copy");
        false
    };

    Ok(CopyReadOutcome { text, restored })
}

/// Keep watching the pasteboard after a copy timeout and put the snapshot back
/// if the copy lands within the grace period.
///
/// Runs on its own thread so the failed read can report immediately; the
/// selection worker is free to serve the next hotkey. The same restore rule as
/// the in-budget path applies: only if nothing else has written since the
/// change we saw, so a clipboard manager or the user's own copy is never
/// reverted. Either way the outcome is logged — a clipboard left holding the
/// selection is something the user should be able to find in the log.
fn watch_for_late_copy(snapshot: PasteboardSnapshot, change_count_before: isize) {
    let spawned = thread::Builder::new()
        .name("voicex-late-copy".to_string())
        .spawn(move || {
            let pasteboard = NSPasteboard::generalPasteboard();
            let deadline = Instant::now() + Duration::from_millis(LATE_COPY_GRACE_MS);
            let landed = loop {
                let now = pasteboard.changeCount();
                if now != change_count_before {
                    break Some(now);
                }
                if Instant::now() >= deadline {
                    break None;
                }
                thread::sleep(Duration::from_millis(LATE_COPY_POLL_INTERVAL_MS));
            };

            let Some(landed) = landed else {
                crate::tts::log_event("copy_landed_late", &[("landed", "false".to_string())]);
                return;
            };

            let restored = if may_restore(landed, pasteboard.changeCount()) {
                match snapshot.restore(&pasteboard, false) {
                    Ok(()) => true,
                    Err(err) => {
                        log::warn!("Failed to restore the clipboard after a late copy: {err}");
                        false
                    }
                }
            } else {
                false
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

/// Post Command+C as a raw CGEvent sourced from `HIDSystemState`, the same
/// source real hardware uses. Applications that gate on the event source (for
/// example remote-desktop clients) otherwise ignore the shortcut.
fn post_copy_shortcut() -> Result<(), SelectionError> {
    const NX_DEVICELCMDKEYMASK: CGEventFlags = CGEventFlags::from_bits_retain(0x0000_0008);

    let base_flags = {
        let mut flags = CGEventFlags::CGEventFlagNonCoalesced;
        flags.set(CGEventFlags::from_bits_retain(0x2000_0000), true);
        flags
    };
    let command_held_flags = base_flags | CGEventFlags::CGEventFlagCommand | NX_DEVICELCMDKEYMASK;

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| SelectionError::Internal("CGEventSource::new failed".to_string()))?;

    let post_key =
        |keycode: u16, keydown: bool, flags: CGEventFlags| -> Result<(), SelectionError> {
            let event =
                CGEvent::new_keyboard_event(source.clone(), keycode, keydown).map_err(|_| {
                    SelectionError::Internal("CGEvent::new_keyboard_event failed".to_string())
                })?;
            event.set_flags(flags);
            event.post(CGEventTapLocation::HID);
            Ok(())
        };

    post_key(MACOS_COMMAND_KEYCODE, true, command_held_flags)?;
    post_key(MACOS_C_KEYCODE, true, command_held_flags)?;
    post_key(MACOS_C_KEYCODE, false, command_held_flags)?;
    post_key(MACOS_COMMAND_KEYCODE, false, base_flags)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_is_skipped_when_anything_else_wrote_to_the_clipboard() {
        // A clipboard manager writing between our copy and our restore now owns
        // the clipboard. Restoring over it would destroy their write, and the
        // user would see their clipboard silently revert.
        assert!(may_restore(7, 7), "untouched since our own copy");
        assert!(!may_restore(7, 8), "someone else wrote after us");
        assert!(!may_restore(7, 6), "went backwards: not ours to overwrite");
    }
}
