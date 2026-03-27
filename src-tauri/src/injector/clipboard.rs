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
    restore_delay_ms: u64,
}

impl TextInjector {
    pub fn new() -> Self {
        Self {
            mode: TextInjectionMode::Pasteboard,
            restore_delay_ms: 200,
        }
    }

    pub fn with_mode(mode: TextInjectionMode) -> Self {
        Self {
            mode,
            restore_delay_ms: 200,
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

        // 3. Send paste command
        self.send_paste_command()?;

        // 4. Wait and restore clipboard
        thread::sleep(Duration::from_millis(self.restore_delay_ms));
        backup.restore(&mut clipboard);

        log::debug!("Injected {} characters via pasteboard", text.len());
        Ok(())
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

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            // Use enigo 0.6's text() method which properly handles all Unicode chars
            // including Chinese punctuation via SendInput with KEYEVENTF_UNICODE.
            let mut enigo = Enigo::new(&Settings::default()).map_err(|e| {
                InjectorError::TypingError(format!("Failed to create Enigo: {}", e))
            })?;
            enigo
                .text(text)
                .map_err(|e| InjectorError::TypingError(format!("Failed to type text: {}", e)))?;
            log::debug!(
                "Injected {} characters via simulated typing (SendInput)",
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
