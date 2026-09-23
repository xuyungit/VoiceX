//! The clipboard as a text source for reading aloud.
//!
//! Only reads: nothing here writes the clipboard, so none of the snapshot and
//! restore machinery of the selection's copy fallback applies. What does
//! apply is that the clipboard can hold things the user never meant to hear
//! or send anywhere — a password manager's copied password above all — and a
//! read goes to a cloud engine and possibly an LLM. Content the copying
//! application marked as concealed is refused outright.

use super::log_event;

/// Pasteboard type a password manager adds to mark what it copied as secret
/// (nspasteboard.org convention: 1Password, Bitwarden, KeePassXC, Keychain
/// Access and others set it).
#[cfg(target_os = "macos")]
const CONCEALED_TYPE: &str = "org.nspasteboard.ConcealedType";

/// Why the clipboard gave us nothing to read. [`ClipboardTextError::code`] is
/// what the HUD and the log key off.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClipboardTextError {
    #[error("The clipboard is empty")]
    Empty,

    /// Something is on the clipboard, but no plain text (an image, say).
    #[error("The clipboard holds no text")]
    NotText,

    /// The copying application marked the content as secret.
    #[error("The clipboard content is marked as concealed")]
    Concealed,

    #[error("The clipboard could not be read: {0}")]
    Unavailable(String),
}

impl ClipboardTextError {
    pub fn code(&self) -> &'static str {
        match self {
            ClipboardTextError::Empty => "clipboard_empty",
            ClipboardTextError::NotText => "clipboard_not_text",
            ClipboardTextError::Concealed => "clipboard_concealed",
            ClipboardTextError::Unavailable(_) => "clipboard_unavailable",
        }
    }
}

/// Read the clipboard's plain text for speaking. Blocking but fast: there is
/// no copy to wait for.
pub fn read() -> Result<String, ClipboardTextError> {
    let started = std::time::Instant::now();
    let result = read_platform();
    match &result {
        Ok(text) => log_event(
            "clipboard_ok",
            &[
                ("chars", text.chars().count().to_string()),
                ("elapsed_ms", started.elapsed().as_millis().to_string()),
            ],
        ),
        Err(err) => {
            let mut fields = vec![("error", err.code().to_string())];
            if let ClipboardTextError::Unavailable(detail) = err {
                fields.push(("detail", detail.clone()));
            }
            log_event("clipboard_err", &fields);
        }
    }
    result
}

/// Decide from what the clipboard declares and what text it yields. Kept free
/// of platform calls so the rules are testable.
fn classify(
    has_any_content: bool,
    concealed: bool,
    text: Option<String>,
) -> Result<String, ClipboardTextError> {
    // Checked first: a concealed password is text, and must never get as far
    // as being judged readable.
    if concealed {
        return Err(ClipboardTextError::Concealed);
    }
    match text {
        Some(text) if !text.trim().is_empty() => Ok(text),
        Some(_) => Err(ClipboardTextError::Empty),
        None if has_any_content => Err(ClipboardTextError::NotText),
        None => Err(ClipboardTextError::Empty),
    }
}

/// macOS: the pasteboard directly, because the concealed marker is a type
/// declaration `arboard` does not expose. Safe off the main thread, like the
/// selection's copy fallback that reads the same pasteboard.
#[cfg(target_os = "macos")]
fn read_platform() -> Result<String, ClipboardTextError> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};

    let pasteboard = NSPasteboard::generalPasteboard();
    let types: Vec<String> = pasteboard
        .types()
        .map(|types| types.iter().map(|ty| ty.to_string()).collect())
        .unwrap_or_default();
    let concealed = types.iter().any(|ty| ty == CONCEALED_TYPE);
    let text = if concealed {
        // Not even read into memory.
        None
    } else {
        unsafe { pasteboard.stringForType(NSPasteboardTypeString) }.map(|value| value.to_string())
    };
    classify(!types.is_empty(), concealed, text)
}

/// Elsewhere: plain text through `arboard`. There is no concealed check yet —
/// Windows marks secrets with the `ExcludeClipboardContentFromMonitorProcessing`
/// format, and honouring it is part of turning reading on for Windows (the
/// hotkey is not bound there until then).
#[cfg(not(target_os = "macos"))]
fn read_platform() -> Result<String, ClipboardTextError> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|err| ClipboardTextError::Unavailable(err.to_string()))?;
    match clipboard.get_text() {
        Ok(text) => classify(true, false, Some(text)),
        Err(arboard::Error::ContentNotAvailable) => {
            // `arboard` cannot tell "empty" from "not text"; an image is the
            // one other kind it can see.
            let has_image = clipboard.get_image().is_ok();
            classify(has_image, false, None)
        }
        Err(err) => Err(ClipboardTextError::Unavailable(err.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::{classify, ClipboardTextError};

    #[test]
    fn plain_text_is_read_as_is() {
        assert_eq!(
            classify(true, false, Some("  hello\n".to_string())),
            Ok("  hello\n".to_string())
        );
    }

    #[test]
    fn concealed_content_is_refused_even_though_it_is_text() {
        assert_eq!(
            classify(true, true, Some("hunter2".to_string())),
            Err(ClipboardTextError::Concealed)
        );
    }

    #[test]
    fn whitespace_counts_as_empty() {
        assert_eq!(
            classify(true, false, Some(" \n\t".to_string())),
            Err(ClipboardTextError::Empty)
        );
    }

    #[test]
    fn content_without_text_is_not_empty_but_not_text() {
        assert_eq!(classify(true, false, None), Err(ClipboardTextError::NotText));
        assert_eq!(classify(false, false, None), Err(ClipboardTextError::Empty));
    }

    #[test]
    fn error_codes_match_what_the_hud_maps() {
        // `hud.ts` maps these literally.
        assert_eq!(ClipboardTextError::Empty.code(), "clipboard_empty");
        assert_eq!(ClipboardTextError::NotText.code(), "clipboard_not_text");
        assert_eq!(ClipboardTextError::Concealed.code(), "clipboard_concealed");
        assert_eq!(
            ClipboardTextError::Unavailable(String::new()).code(),
            "clipboard_unavailable"
        );
    }
}
