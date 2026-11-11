use super::diff;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

const MAX_UTF16_UNITS: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeError {
    TooLarge,
    InvalidSelection,
    BaselineTransformed,
    Ambiguous,
    TargetChanged,
    FieldCleared,
    BudgetExceeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeEvidence {
    BeforeAfterVerified,
    PostAnchored,
}

/// Identity is an opaque token owned by the native observation worker. It must
/// include the worker's generation and identify a control, not just a PID.
pub struct Snapshot<'a> {
    pub target: u64,
    pub text: &'a str,
    pub selection_utf16: Range<usize>,
}

pub struct RangeTracker {
    target: u64,
    pub baseline: String,
    pub evidence: RangeEvidence,
    reference: String,
    range: Range<usize>,
}

fn check_size(text: &str) -> Result<(), RangeError> {
    if text.encode_utf16().count() > MAX_UTF16_UNITS {
        Err(RangeError::TooLarge)
    } else {
        Ok(())
    }
}

/// Convert system offsets without splitting surrogate pairs or graphemes.
fn byte_offset(text: &str, offset: usize) -> Result<usize, RangeError> {
    let mut utf16 = 0;
    for (byte, grapheme) in text.grapheme_indices(true) {
        if utf16 == offset {
            return Ok(byte);
        }
        utf16 += grapheme.encode_utf16().count();
        if utf16 > offset {
            return Err(RangeError::InvalidSelection);
        }
    }
    if utf16 == offset {
        Ok(text.len())
    } else {
        Err(RangeError::InvalidSelection)
    }
}

fn unique_match(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let first = haystack.find(needle)?;
    let next = first + haystack[first..].chars().next()?.len_utf8();
    if haystack[next..].contains(needle) {
        None
    } else {
        Some(first)
    }
}

fn grapheme_boundary(text: &str, offset: usize) -> bool {
    offset == text.len()
        || text
            .grapheme_indices(true)
            .any(|(index, _)| index == offset)
}

impl RangeTracker {
    /// Verify that replacing the exact pre-injection selection explains the
    /// entire post-injection snapshot. No normalization is assumed here.
    pub fn from_before_after(
        before: &Snapshot<'_>,
        after: &Snapshot<'_>,
        injected: &str,
    ) -> Result<Self, RangeError> {
        if before.target != after.target {
            return Err(RangeError::TargetChanged);
        }
        check_size(before.text)?;
        check_size(after.text)?;
        check_size(injected)?;
        if injected.is_empty() || before.selection_utf16.start > before.selection_utf16.end {
            return Err(RangeError::InvalidSelection);
        }
        let start = byte_offset(before.text, before.selection_utf16.start)?;
        let end = byte_offset(before.text, before.selection_utf16.end)?;
        let expected = format!(
            "{}{}{}",
            &before.text[..start],
            injected,
            &before.text[end..]
        );
        if expected != after.text {
            return Err(RangeError::BaselineTransformed);
        }
        if !grapheme_boundary(after.text, start)
            || !grapheme_boundary(after.text, start + injected.len())
        {
            return Err(RangeError::Ambiguous);
        }
        Ok(Self::new(
            after.target,
            after.text,
            start..start + injected.len(),
            RangeEvidence::BeforeAfterVerified,
        ))
    }

    /// Without a pre-read, require a unique exact match ending at the caret.
    pub fn from_post_anchored(after: &Snapshot<'_>, injected: &str) -> Result<Self, RangeError> {
        check_size(after.text)?;
        check_size(injected)?;
        if after.selection_utf16.start != after.selection_utf16.end {
            return Err(RangeError::Ambiguous);
        }
        let caret = byte_offset(after.text, after.selection_utf16.end)?;
        let start = unique_match(after.text, injected).ok_or(RangeError::Ambiguous)?;
        if start + injected.len() != caret || !grapheme_boundary(after.text, start) {
            return Err(RangeError::Ambiguous);
        }
        Ok(Self::new(
            after.target,
            after.text,
            start..caret,
            RangeEvidence::PostAnchored,
        ))
    }

    fn new(target: u64, text: &str, range: Range<usize>, evidence: RangeEvidence) -> Self {
        let start = text[..range.start].graphemes(true).count();
        let length = text[range.clone()].graphemes(true).count();
        Self {
            target,
            baseline: text[range].to_string(),
            evidence,
            reference: text.to_string(),
            range: start..start + length,
        }
    }

    /// Project the original injection range onto the current field. A sampled
    /// backspace must not shrink the coordinate frame used for the next sample.
    /// Net boundary growth remains unknown: correction and continuation cannot
    /// be distinguished from text alone.
    pub fn slice<'a>(&self, snapshot: &'a Snapshot<'_>) -> Result<&'a str, RangeError> {
        if snapshot.target != self.target {
            return Err(RangeError::TargetChanged);
        }
        check_size(snapshot.text)?;
        // Rich-text and web inputs report an emptied field as a line break
        // (e.g. "\n" after a chat app sends and clears its composer), or as
        // a zero-width caret anchor.
        if snapshot.text.chars().all(is_blank) {
            return Err(RangeError::FieldCleared);
        }
        let (changes, mapped) =
            diff::track_changes(&self.reference, snapshot.text, self.range.clone()).map_err(
                |error| match error {
                    diff::TrackingError::Ambiguous => RangeError::Ambiguous,
                    diff::TrackingError::BudgetExceeded => RangeError::BudgetExceeded,
                },
            )?;
        for change in changes {
            let old = &change.before;
            let delta = change.after.len() as isize - old.len() as isize;
            if old.is_empty() && (old.start == self.range.start || old.start == self.range.end) {
                return Err(RangeError::Ambiguous);
            }
            if old.end <= self.range.start || old.start >= self.range.end {
                // Outside the injection; it does not contribute to its edits.
            } else if old.start >= self.range.start && old.end <= self.range.end {
                if delta > 0 && (old.start == self.range.start || old.end == self.range.end) {
                    return Err(RangeError::Ambiguous);
                }
            } else {
                return Err(RangeError::Ambiguous);
            }
        }
        let offsets: Vec<_> = snapshot
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain(std::iter::once(snapshot.text.len()))
            .collect();
        let (start, end) = (mapped.start, mapped.end);
        let slice = &snapshot.text[offsets[start]..offsets[end]];
        Ok(slice)
    }
}

/// Whitespace, or a zero-width character editors keep as a caret anchor in an
/// empty paragraph (Slate renders U+FEFF; others use U+200B). Neither is
/// content a user typed or dictated.
pub fn is_blank(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(text: &str, selection: Range<usize>) -> Snapshot<'_> {
        Snapshot {
            target: 1,
            text,
            selection_utf16: selection,
        }
    }

    #[test]
    fn verifies_selection_replacement_and_tracks_only_anchored_text() {
        let before = snap("前旧内容后", 1..4);
        let after = snap("前返利后", 3..3);
        let tracker = RangeTracker::from_before_after(&before, &after, "返利").unwrap();
        assert_eq!(tracker.slice(&snap("前反力后", 3..3)), Ok("反力"));
        assert_eq!(
            tracker.slice(&snap("前返利续写后", 5..5)),
            Err(RangeError::Ambiguous)
        );
        for cleared in ["", "\n", " \n", "\u{FEFF}", "\u{FEFF}\n", "\u{200B}"] {
            assert_eq!(
                tracker.slice(&snap(cleared, 0..0)),
                Err(RangeError::FieldCleared)
            );
        }
        assert_eq!(
            tracker.slice(&snap("前反力后前反力后", 3..3)),
            Err(RangeError::Ambiguous)
        );
    }

    #[test]
    fn rejects_split_emoji_repeated_matches_and_tail_continuation() {
        let before = snap("a👩‍💻b", 2..2);
        assert!(matches!(
            RangeTracker::from_before_after(&before, &snap("x", 0..0), "x"),
            Err(RangeError::InvalidSelection)
        ));
        assert!(matches!(
            RangeTracker::from_post_anchored(&snap("hi hi", 5..5), "hi"),
            Err(RangeError::Ambiguous)
        ));
        assert!(matches!(
            RangeTracker::from_post_anchored(&snap("aaa", 2..2), "aa"),
            Err(RangeError::Ambiguous)
        ));
        let tracker =
            RangeTracker::from_post_anchored(&snap("prefix hello", 12..12), "hello").unwrap();
        assert_eq!(
            tracker.slice(&snap("prefix hello more", 17..17)),
            Err(RangeError::Ambiguous)
        );
    }

    #[test]
    fn rejects_changed_target_and_transformed_baseline() {
        let before = snap("ab", 1..1);
        assert!(matches!(
            RangeTracker::from_before_after(&before, &snap("aXb", 2..2), "x"),
            Err(RangeError::BaselineTransformed)
        ));
        let other = Snapshot {
            target: 2,
            text: "axb",
            selection_utf16: 2..2,
        };
        assert!(matches!(
            RangeTracker::from_before_after(&before, &other, "x"),
            Err(RangeError::TargetChanged)
        ));
    }

    #[test]
    fn tracks_empty_fields_and_tail_corrections_but_rejects_mixed_continuation() {
        for (before, selection, after, edited) in [
            ("", 0..0, "支座返力", "支座反力"),
            ("原文：", 3..3, "原文：支座返力", "原文：支座反力"),
        ] {
            let tracker = RangeTracker::from_before_after(
                &snap(before, selection),
                &snap(after, 0..0),
                "支座返力",
            )
            .unwrap();
            assert_eq!(tracker.slice(&snap(edited, 0..0)), Ok("支座反力"));
        }
        let tracker = RangeTracker::from_before_after(
            &snap("前后", 1..1),
            &snap("前支座返力后", 5..5),
            "支座返力",
        )
        .unwrap();
        assert_eq!(
            tracker.slice(&snap("前支座反力另外补一句后", 10..10)),
            Err(RangeError::Ambiguous)
        );
    }

    #[test]
    fn maps_unrelated_prefix_edits_and_rejects_changes_across_the_boundary() {
        let tracker = RangeTracker::from_before_after(
            &snap("前文：后文", 3..3),
            &snap("前文：测试后文", 5..5),
            "测试",
        )
        .unwrap();
        assert_eq!(tracker.slice(&snap("新的前文：测试后文", 0..0)), Ok("测试"));
        assert_eq!(tracker.slice(&snap("新的前文：测式后文", 0..0)), Ok("测式"));
        assert_eq!(
            tracker.slice(&snap("新的前文后文", 0..0)),
            Err(RangeError::Ambiguous)
        );
    }

    #[test]
    fn repeated_characters_or_phrases_cannot_assign_a_deletion_to_either_side() {
        for (before, after, injected, selection, edited) in [
            ("AX", "AXX", "X", 2..2, "AX"),
            ("ab", "abab", "ab", 2..2, "ab"),
        ] {
            let tracker = RangeTracker::from_before_after(
                &snap(before, selection),
                &snap(after, 0..0),
                injected,
            )
            .unwrap();
            assert_eq!(
                tracker.slice(&snap(edited, 0..0)),
                Err(RangeError::Ambiguous)
            );
        }
    }

    #[test]
    fn backspace_then_retype_recovers_the_original_injection_boundary() {
        let tracker =
            RangeTracker::from_before_after(&snap("", 0..0), &snap("支座返力", 4..4), "支座返力")
                .unwrap();
        assert_eq!(tracker.slice(&snap("支座返", 3..3)), Ok("支座返"));
        assert_eq!(tracker.slice(&snap("支座反力", 4..4)), Ok("支座反力"));
        assert_eq!(
            tracker.slice(&snap("支座反力，继续写", 8..8)),
            Err(RangeError::Ambiguous)
        );
    }
}
