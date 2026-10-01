//! Direct Win32 clipboard access for the reading feature.
//!
//! `arboard` covers plain text and images, which is all the dictation injector
//! needs. Reading needs what it hides: the clipboard sequence number (how the
//! copy fallback knows a copy landed), every format on the clipboard (what a
//! fail-closed snapshot has to capture), and whether one particular registered
//! format is present (how a password manager marks a secret). Both callers —
//! `selection`'s copy fallback and `tts::clipboard_text` — go through here.
//!
//! The clipboard is a single, system-wide lock: whoever has it open blocks
//! everyone else, so [`OpenClipboard`] is held only as long as a scope needs
//! it and opening retries briefly instead of failing on the first refusal.

use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{GetLastError, GlobalFree, HANDLE, HGLOBAL};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, CountClipboardFormats, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardFormatNameW, GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};

/// `CF_UNICODETEXT`. Spelled out rather than imported so this module and the
/// platform-neutral format rules agree on one number without a dependency.
pub const CF_UNICODETEXT: u32 = 13;

/// How often a refused `OpenClipboard` is retried while another process holds
/// the clipboard. Clipboard managers and `rdpclip` open it for a few
/// milliseconds at a time; the copy fallback also opens it right behind the
/// application that is still writing the copy.
const OPEN_RETRY_INTERVAL_MS: u64 = 5;

/// The system-wide clipboard sequence number. It changes on every write —
/// `EmptyClipboard` and each `SetClipboardData` — and needs no open clipboard.
pub fn sequence_number() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// The id of a registered format such as `"HTML Format"`, registering it if
/// nobody has yet. `None` only when the system refuses, which it does not do
/// for a valid name.
pub fn registered_format(name: &str) -> Option<u32> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    match unsafe { RegisterClipboardFormatW(wide.as_ptr()) } {
        0 => None,
        format => Some(format),
    }
}

/// A registered format's name, for logs and refusal reasons. Predefined
/// formats have no registered name and come back as their number.
pub fn format_name(format: u32) -> String {
    let mut buffer = [0u16; 256];
    let len = unsafe { GetClipboardFormatNameW(format, buffer.as_mut_ptr(), buffer.len() as i32) };
    if len > 0 {
        String::from_utf16_lossy(&buffer[..len as usize])
    } else {
        format!("CF_{format}")
    }
}

/// The clipboard, opened. Closes on drop.
///
/// Opened without an owner window: what the reader writes back is plain
/// memory, never delayed rendering, which is the one thing an ownerless
/// clipboard cannot offer.
pub struct OpenClipboardGuard {
    _private: (),
}

impl OpenClipboardGuard {
    /// Open the clipboard, retrying for up to `budget` while another process
    /// has it open. The error is the last `GetLastError`, for the log.
    pub fn open(budget: Duration) -> Result<Self, String> {
        let deadline = Instant::now() + budget;
        loop {
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Ok(Self { _private: () });
            }
            let error = unsafe { GetLastError() };
            if Instant::now() >= deadline {
                return Err(format!(
                    "OpenClipboard kept failing for {} ms (error {error})",
                    budget.as_millis()
                ));
            }
            thread::sleep(Duration::from_millis(OPEN_RETRY_INTERVAL_MS));
        }
    }

    /// Every format on the clipboard, in the order `EnumClipboardFormats`
    /// lists them: the ones the writer placed first, in placement order, then
    /// the ones the system can synthesize from them.
    pub fn formats(&self) -> Result<Vec<u32>, String> {
        let mut formats = Vec::new();
        let mut format = 0u32;
        loop {
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            formats.push(format);
        }
        // 0 is both "no more formats" and "failed"; only the error code tells
        // them apart, and a half-enumerated list must not pass for a whole one.
        match unsafe { GetLastError() } {
            0 => Ok(formats),
            error => Err(format!("EnumClipboardFormats failed (error {error})")),
        }
    }

    pub fn has_format(&self, format: u32) -> bool {
        unsafe { IsClipboardFormatAvailable(format) != 0 }
    }

    pub fn is_empty(&self) -> bool {
        unsafe { CountClipboardFormats() == 0 }
    }

    /// The raw handle behind `format`. Asking for a format the owner offered
    /// with delayed rendering makes the owner render it now, synchronously.
    pub fn data_handle(&self, format: u32) -> Option<HANDLE> {
        let handle = unsafe { GetClipboardData(format) };
        (!handle.is_null()).then_some(handle)
    }

    /// The bytes of a global-memory format, or `None` when the handle is not
    /// global memory the caller can lock.
    pub fn global_bytes(&self, format: u32) -> Option<Vec<u8>> {
        let handle = self.data_handle(format)?;
        unsafe { copy_global(handle) }
    }

    /// The clipboard's Unicode text, up to its terminating NUL. `None` when
    /// there is no text on the clipboard at all.
    pub fn unicode_text(&self) -> Option<String> {
        let bytes = self.global_bytes(CF_UNICODETEXT)?;
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|&unit| unit != 0)
            .collect();
        Some(String::from_utf16_lossy(&units))
    }

    /// Clear the clipboard and take ownership of it. The destructive step of a
    /// restore: everything that can fail should already have succeeded.
    pub fn empty(&self) -> Result<(), String> {
        if unsafe { EmptyClipboard() } == 0 {
            return Err(format!("EmptyClipboard failed (error {})", unsafe {
                GetLastError()
            }));
        }
        Ok(())
    }

    /// Hand `handle` to the clipboard. On success the system owns it; on
    /// failure it is still the caller's to free.
    pub fn set_handle(&self, format: u32, handle: HANDLE) -> Result<(), String> {
        if unsafe { SetClipboardData(format, handle) }.is_null() {
            return Err(format!(
                "SetClipboardData failed for {} (error {})",
                format_name(format),
                unsafe { GetLastError() }
            ));
        }
        Ok(())
    }
}

impl Drop for OpenClipboardGuard {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

/// Copy a global-memory block. `None` for a handle that is not one.
unsafe fn copy_global(handle: HANDLE) -> Option<Vec<u8>> {
    let memory = handle as HGLOBAL;
    let size = GlobalSize(memory);
    let pointer = GlobalLock(memory) as *const u8;
    if pointer.is_null() {
        return None;
    }
    let bytes = std::slice::from_raw_parts(pointer, size).to_vec();
    GlobalUnlock(memory);
    Some(bytes)
}

/// Fresh movable global memory holding `bytes`, the form `SetClipboardData`
/// takes for every memory format. The caller frees it unless the clipboard
/// accepts it.
pub fn alloc_global(bytes: &[u8]) -> Result<HGLOBAL, String> {
    // A zero-sized block cannot be locked; formats with an empty payload still
    // get a valid (one-byte) block, the way their writers allocated them.
    let size = bytes.len().max(1);
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, size);
        if memory.is_null() {
            return Err(format!("GlobalAlloc({size}) failed"));
        }
        let pointer = GlobalLock(memory) as *mut u8;
        if pointer.is_null() {
            GlobalFree(memory);
            return Err("GlobalLock failed on fresh memory".to_string());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len());
        GlobalUnlock(memory);
        Ok(memory)
    }
}
