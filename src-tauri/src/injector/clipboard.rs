//! Clipboard-based text injection

use arboard::{Clipboard, ImageData};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use enigo::{Enigo, Keyboard, Settings};
use rdev::{simulate, EventType, Key};
use std::borrow::Cow;
use std::thread;
use std::time::Duration;

/// Maximum characters to inject via typing mode before falling back to clipboard.
/// Typing is slower but avoids clipboard sync issues; use clipboard for long text.
const TYPING_MODE_MAX_CHARS: usize = 500;

/// How long we wait for a clipboard write to become readable again.
const CLIPBOARD_WRITE_VERIFY_RETRIES: usize = 5;
const CLIPBOARD_WRITE_VERIFY_INTERVAL_MS: u64 = 20;

/// Give clipboard bridges (for example remote desktop sessions) a brief window
/// to observe the new clipboard payload before we synthesize paste.
#[cfg(target_os = "macos")]
const CLIPBOARD_PRE_PASTE_DELAY_MS: u64 = 120;
#[cfg(not(target_os = "macos"))]
const CLIPBOARD_PRE_PASTE_DELAY_MS: u64 = 80;

/// Do not restore the user's clipboard immediately after paste. Remote hosts
/// often sync clipboard contents asynchronously and can otherwise paste stale
/// data or receive the restore before the fresh payload is consumed.
#[cfg(target_os = "macos")]
const CLIPBOARD_RESTORE_DELAY_MS: u64 = 900;
#[cfg(not(target_os = "macos"))]
const CLIPBOARD_RESTORE_DELAY_MS: u64 = 500;

#[cfg(target_os = "macos")]
fn text_chunks(s: &str, max_chars: usize) -> Vec<&str> {
    assert!(max_chars > 0);
    let mut chunks = Vec::new();
    let mut chunk_start = 0;
    let mut count = 0;

    for (byte_idx, ch) in s.char_indices() {
        count += 1;

        if count >= max_chars {
            let end = byte_idx + ch.len_utf8();
            chunks.push(&s[chunk_start..end]);
            chunk_start = end;
            count = 0;
        }
    }

    if chunk_start < s.len() {
        chunks.push(&s[chunk_start..]);
    }

    chunks
}

/// Text injection mode
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextInjectionMode {
    Pasteboard, // Clipboard paste (default, cross-platform)
    Typing,     // Simulated typing (best effort)
}

impl Default for TextInjectionMode {
    fn default() -> Self {
        Self::Pasteboard
    }
}

impl TextInjectionMode {
    pub fn from_str(value: &str) -> Self {
        match value {
            "typing" => TextInjectionMode::Typing,
            _ => TextInjectionMode::Pasteboard,
        }
    }
}

/// Backup of clipboard content before injection
/// Supports text and image formats (the two main formats arboard can handle)
enum ClipboardBackup {
    Text(String),
    Image {
        width: usize,
        height: usize,
        bytes: Vec<u8>,
    },
    None,
}

impl ClipboardBackup {
    /// Save current clipboard content (tries text first, then image)
    fn save(clipboard: &mut Clipboard) -> Self {
        // Try text first (most common case)
        if let Ok(text) = clipboard.get_text() {
            if !text.is_empty() {
                return ClipboardBackup::Text(text);
            }
        }

        // Try image
        if let Ok(img) = clipboard.get_image() {
            return ClipboardBackup::Image {
                width: img.width,
                height: img.height,
                bytes: img.bytes.into_owned(),
            };
        }

        ClipboardBackup::None
    }

    /// Restore clipboard content
    fn restore(self, clipboard: &mut Clipboard) {
        match self {
            ClipboardBackup::Text(text) => {
                if let Err(e) = clipboard.set_text(&text) {
                    log::warn!("Failed to restore clipboard text: {}", e);
                }
            }
            ClipboardBackup::Image {
                width,
                height,
                bytes,
            } => {
                let img_data = ImageData {
                    width,
                    height,
                    bytes: Cow::Owned(bytes),
                };
                if let Err(e) = clipboard.set_image(img_data) {
                    log::warn!("Failed to restore clipboard image: {}", e);
                }
            }
            ClipboardBackup::None => {
                // Nothing to restore; optionally clear clipboard
                let _ = clipboard.clear();
            }
        }
    }
}

/// Text injector for inserting recognized text into applications
pub struct TextInjector {
    mode: TextInjectionMode,
    pre_paste_delay_ms: u64,
    restore_delay_ms: u64,
}

impl TextInjector {
    pub fn new() -> Self {
        Self {
            mode: TextInjectionMode::Pasteboard,
            pre_paste_delay_ms: CLIPBOARD_PRE_PASTE_DELAY_MS,
            restore_delay_ms: CLIPBOARD_RESTORE_DELAY_MS,
        }
    }

    pub fn with_mode(mode: TextInjectionMode) -> Self {
        Self {
            mode,
            pre_paste_delay_ms: CLIPBOARD_PRE_PASTE_DELAY_MS,
            restore_delay_ms: CLIPBOARD_RESTORE_DELAY_MS,
        }
    }

    /// Inject text using the configured mode
    pub fn inject(&self, text: &str) -> Result<(), InjectorError> {
        if text.is_empty() {
            return Ok(());
        }

        match self.mode {
            TextInjectionMode::Pasteboard => self.inject_via_pasteboard(text),
            TextInjectionMode::Typing => self.inject_via_typing(text),
        }
    }

    fn inject_via_pasteboard(&self, text: &str) -> Result<(), InjectorError> {
        let mut clipboard =
            Clipboard::new().map_err(|e| InjectorError::ClipboardError(e.to_string()))?;

        // 1. Save current clipboard content (text or image)
        let backup = ClipboardBackup::save(&mut clipboard);
        log::debug!(
            "Clipboard backup: {}",
            match &backup {
                ClipboardBackup::Text(t) => format!("text ({} chars)", t.len()),
                ClipboardBackup::Image { width, height, .. } =>
                    format!("image ({}x{})", width, height),
                ClipboardBackup::None => "none".to_string(),
            }
        );

        // 2. Set new text
        clipboard
            .set_text(text)
            .map_err(|e| InjectorError::ClipboardError(e.to_string()))?;
        self.verify_clipboard_text(&mut clipboard, text)?;

        if self.pre_paste_delay_ms > 0 {
            thread::sleep(Duration::from_millis(self.pre_paste_delay_ms));
        }

        // 3. Send paste command
        self.send_paste_command()?;

        // 4. Wait and restore clipboard
        thread::sleep(Duration::from_millis(self.restore_delay_ms));
        self.restore_clipboard_if_unchanged(&mut clipboard, backup, text);

        log::debug!("Injected {} characters via pasteboard", text.len());
        Ok(())
    }

    fn verify_clipboard_text(
        &self,
        clipboard: &mut Clipboard,
        expected: &str,
    ) -> Result<(), InjectorError> {
        for attempt in 0..CLIPBOARD_WRITE_VERIFY_RETRIES {
            match clipboard.get_text() {
                Ok(current) if current == expected => return Ok(()),
                Ok(current) => {
                    log::debug!(
                        "Clipboard verification mismatch on attempt {} (expected {} chars, got {} chars)",
                        attempt + 1,
                        expected.chars().count(),
                        current.chars().count()
                    );
                }
                Err(err) => {
                    log::debug!(
                        "Clipboard verification read failed on attempt {}: {}",
                        attempt + 1,
                        err
                    );
                }
            }

            thread::sleep(Duration::from_millis(
                CLIPBOARD_WRITE_VERIFY_INTERVAL_MS,
            ));
        }

        Err(InjectorError::ClipboardError(
            "Clipboard write could not be verified before paste".to_string(),
        ))
    }

    fn restore_clipboard_if_unchanged(
        &self,
        clipboard: &mut Clipboard,
        backup: ClipboardBackup,
        injected_text: &str,
    ) {
        match clipboard.get_text() {
            Ok(current) if current == injected_text => backup.restore(clipboard),
            Ok(current) => {
                log::info!(
                    "Skipping clipboard restore because clipboard changed after injection ({} chars)",
                    current.chars().count()
                );
            }
            Err(err) => {
                log::info!(
                    "Skipping clipboard restore because clipboard is no longer readable after injection: {}",
                    err
                );
            }
        }
    }

    fn inject_via_typing(&self, text: &str) -> Result<(), InjectorError> {
        // For long text, fall back to clipboard as typing is too slow
        let char_count = text.chars().count();
        if char_count > TYPING_MODE_MAX_CHARS {
            log::info!(
                "Text too long ({} chars > {} limit); using clipboard mode for speed",
                char_count,
                TYPING_MODE_MAX_CHARS
            );
            return self.inject_via_pasteboard(text);
        }

        #[cfg(target_os = "macos")]
        {
            if text.contains('\n') || text.contains('\r') {
                log::info!(
                    "Typing mode on macOS falls back to pasteboard for multiline text to avoid IME/newline injection bugs"
                );
                return self.inject_via_pasteboard(text);
            }

            // On macOS, enigo ultimately uses CGEventKeyboardSetUnicodeString,
            // which truncates each posted string to 20 Unicode scalars.
            // For now, multiline text uses pasteboard mode above; single-line text
            // stays on typing mode and is chunked to respect that limit.
            const CHUNK_SIZE: usize = 20;

            let chunks = text_chunks(text, CHUNK_SIZE);
            let mut enigo = Enigo::new(&Settings::default()).map_err(|e| {
                InjectorError::TypingError(format!("Failed to create Enigo: {}", e))
            })?;
            log::debug!(
                "Injecting {} characters via simulated typing on macOS ({} chunks of ≤{})",
                char_count,
                chunks.len(),
                CHUNK_SIZE
            );

            for chunk in chunks {
                enigo.text(chunk).map_err(|e| {
                    InjectorError::TypingError(format!("Failed to type text chunk: {}", e))
                })?;
            }

            Ok(())
        }

        #[cfg(target_os = "windows")]
        {
            let mut enigo = Enigo::new(&Settings::default()).map_err(|e| {
                InjectorError::TypingError(format!("Failed to create Enigo: {}", e))
            })?;
            enigo
                .text(text)
                .map_err(|e| InjectorError::TypingError(format!("Failed to type text: {}", e)))?;
            log::debug!(
                "Injected {} characters via simulated typing on Windows",
                char_count
            );
            Ok(())
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            log::warn!("Typing mode not supported on this platform; falling back to pasteboard");
            self.inject_via_pasteboard(text)
        }
    }

    fn send_paste_command(&self) -> Result<(), InjectorError> {
        #[cfg(target_os = "macos")]
        let modifiers = [Key::MetaLeft];

        #[cfg(not(target_os = "macos"))]
        let modifiers = [Key::ControlLeft];

        let sequence = modifiers
            .iter()
            .map(|m| EventType::KeyPress(*m))
            .chain(std::iter::once(EventType::KeyPress(Key::KeyV)))
            .chain(std::iter::once(EventType::KeyRelease(Key::KeyV)))
            .chain(modifiers.iter().rev().map(|m| EventType::KeyRelease(*m)));

        for evt in sequence {
            self.simulate_key(evt)?;
        }

        Ok(())
    }

    fn simulate_key(&self, evt: EventType) -> Result<(), InjectorError> {
        simulate(&evt).map_err(|e| InjectorError::PasteCommandFailed(format!("{e}")))?;
        // Tiny delay to preserve key ordering for some hosts.
        thread::sleep(Duration::from_millis(5));
        Ok(())
    }
}

impl Default for TextInjector {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InjectorError {
    #[error("Clipboard error: {0}")]
    ClipboardError(String),

    #[error("Failed to send paste command: {0}")]
    PasteCommandFailed(String),

    #[error("Typing error: {0}")]
    TypingError(String),

    #[error("Platform not supported")]
    UnsupportedPlatform,
}
