//! Rules for what a log line may carry.
//!
//! VoiceX handles text the user said, selected or is about to type into another
//! application, so ordinary (info-level) lines must stay readable without
//! becoming a transcript: they may say *which* sentence a line is about, never
//! quote it whole. Text that a report really needs in full belongs on a
//! `log::debug!` line, which is only written when someone asked for it with
//! `RUST_LOG`.

/// Longest run of text an ordinary log line may quote, in characters.
pub const PREVIEW_CHARS: usize = 120;

/// The first [`PREVIEW_CHARS`] characters of `text`, marked with `…` when it
/// was cut. Callers pass user or service text through this before logging it at
/// info level.
pub fn preview(text: &str) -> String {
    let mut s: String = text.chars().take(PREVIEW_CHARS).collect();
    if text.chars().count() > PREVIEW_CHARS {
        s.push('…');
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_untouched_and_long_text_is_cut_and_marked() {
        assert_eq!(preview("hello"), "hello");
        assert_eq!(preview(""), "");

        let short: String = "a".repeat(PREVIEW_CHARS);
        assert_eq!(preview(&short), short, "a text of exactly the limit fits");

        let long: String = "a".repeat(PREVIEW_CHARS + 1);
        let cut = preview(&long);
        assert_eq!(cut.chars().count(), PREVIEW_CHARS + 1, "… marks the cut");
        assert!(cut.ends_with('…'));
        assert_eq!(cut.trim_end_matches('…'), short);
    }

    #[test]
    fn the_cut_counts_characters_not_bytes() {
        let text = "中文".repeat(PREVIEW_CHARS);
        let cut = preview(&text);
        assert!(cut.ends_with('…'));
        assert_eq!(cut.chars().count(), PREVIEW_CHARS + 1);
    }
}
