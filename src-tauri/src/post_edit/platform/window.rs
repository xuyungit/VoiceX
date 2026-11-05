//! Bounded document windows. Native readers retain the coordinate frame, never
//! follow a moving caret, and validate clipped edges before attributing edits.
#[cfg(any(target_os = "macos", test))]
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

use super::ReadError;

pub(super) const MAX_UNITS: usize = 16_384;
pub(super) const CONTEXT_UNITS: usize = 2_048;
const GUARD_GRAPHEMES: usize = 32;

#[derive(Default)]
pub(super) struct EdgeGuards {
    edges: Option<(String, String)>,
}
impl EdgeGuards {
    pub(super) fn check(&mut self, text: &str, left: bool, right: bool) -> Result<(), ReadError> {
        if text.is_empty() {
            return Ok(()); // The whole field clearing is handled by Observation.
        }
        if let Some((head, tail)) = &self.edges {
            if !text.starts_with(head) || !text.ends_with(tail) {
                return Err(ReadError::WindowChanged);
            }
        } else {
            let head = if left {
                text.graphemes(true).take(GUARD_GRAPHEMES).collect()
            } else {
                String::new()
            };
            let tail = if right {
                text.graphemes(true)
                    .rev()
                    .take(GUARD_GRAPHEMES)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect()
            } else {
                String::new()
            };
            self.edges = Some((head, tail));
        }
        Ok(())
    }
}

#[cfg(any(target_os = "macos", test))]
pub(super) struct ReadWindow {
    start: usize,
    end: usize,
    total: usize,
    guards: EdgeGuards,
}
#[cfg(any(target_os = "macos", test))]
impl ReadWindow {
    pub(super) fn new(
        total: usize,
        selection: Range<usize>,
        lookbehind: usize,
    ) -> Result<Self, ReadError> {
        if selection.start > selection.end || selection.end > total {
            return Err(ReadError::Unsupported);
        }
        let start = selection.start.saturating_sub(lookbehind);
        let end = selection.end.saturating_add(CONTEXT_UNITS).min(total);
        if end - start > MAX_UNITS {
            return Err(ReadError::TooLarge);
        }
        Ok(Self {
            start,
            end,
            total,
            guards: EdgeGuards::default(),
        })
    }

    pub(super) fn range(&self, total: usize) -> Result<Range<usize>, ReadError> {
        if total == 0 {
            return Ok(0..0);
        }
        // Changes within the window shift its right edge by the field's delta.
        // Changes outside it must pass the independent clipped-edge guards.
        let end = if total >= self.total {
            self.end.checked_add(total - self.total)
        } else {
            self.end.checked_sub(self.total - total)
        }
        .ok_or(ReadError::WindowChanged)?;
        if self.start >= end || end > total {
            return Err(ReadError::WindowChanged);
        }
        if end - self.start > MAX_UNITS {
            return Err(ReadError::TooLarge);
        }
        Ok(self.start..end)
    }

    pub(super) fn check(&mut self, text: &str) -> Result<(), ReadError> {
        self.guards
            .check(text, self.start > 0, self.end < self.total)
    }

    /// AX ranges are UTF-16 offsets and can cut a surrogate pair at a clipped
    /// edge. Align the initial frame before converting; never repair body text.
    pub(super) fn decode(
        &mut self,
        units: &[u16],
        bounds: &mut Range<usize>,
    ) -> Result<String, ReadError> {
        let mut start = 0;
        let mut end = units.len();
        if self.guards.edges.is_none() {
            if self.start > 0 && units.first().is_some_and(|u| (0xDC00..=0xDFFF).contains(u)) {
                self.start += 1;
                bounds.start += 1;
                start += 1;
            }
            if self.end < self.total && units.last().is_some_and(|u| (0xD800..=0xDBFF).contains(u))
            {
                self.end -= 1;
                bounds.end -= 1;
                end -= 1;
            }
        }
        String::from_utf16(&units[start..end]).map_err(|_| ReadError::WindowChanged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn slice(text: &str, range: Range<usize>) -> String {
        String::from_utf16(&text.encode_utf16().collect::<Vec<_>>()[range]).unwrap()
    }

    #[test]
    fn reads_near_a_distant_selection_and_keeps_the_frame_after_growth() {
        let prefix = "前".repeat(100_000);
        let suffix = "后".repeat(100_000);
        let before = format!("{prefix}旧文{suffix}");
        let mut window = ReadWindow::new(200_002, 100_000..100_002, CONTEXT_UNITS).unwrap();
        let range = window.range(200_002).unwrap();
        assert_eq!(range.len(), CONTEXT_UNITS * 2 + 2);
        let local_before = slice(&before, range.clone());
        window.check(&local_before).unwrap();
        let after = format!("{prefix}支座返力{suffix}");
        let next = window.range(200_004).unwrap();
        assert_eq!(next.start, range.start);
        let local_after = slice(&after, next);
        window.check(&local_after).unwrap();
        use crate::post_edit::range::{RangeTracker, Snapshot};
        let tracker = RangeTracker::from_before_after(
            &Snapshot {
                target: 1,
                text: &local_before,
                selection_utf16: CONTEXT_UNITS..CONTEXT_UNITS + 2,
            },
            &Snapshot {
                target: 1,
                text: &local_after,
                selection_utf16: CONTEXT_UNITS + 4..CONTEXT_UNITS + 4,
            },
            "支座返力",
        )
        .unwrap();
        let corrected = local_after.replace("支座返力", "支座反力");
        assert_eq!(
            tracker.slice(&Snapshot {
                target: 1,
                text: &corrected,
                selection_utf16: 0..0
            }),
            Ok("支座反力")
        );
        assert_eq!(window.range(0).unwrap(), 0..0);
    }

    #[test]
    fn rejects_lost_edges_and_bounds_single_reads_instead_of_document_size() {
        let mut window = ReadWindow::new(1_000_000, 500_000..500_000, CONTEXT_UNITS).unwrap();
        let text = format!("{}中{}", "a".repeat(2048), "b".repeat(2047));
        window.check(&text).unwrap();
        assert_eq!(
            window.check(&format!("x{}", &text[1..])),
            Err(ReadError::WindowChanged)
        );
        assert_eq!(
            window.range(1_000_000 + MAX_UNITS),
            Err(ReadError::TooLarge)
        );
        assert!(matches!(
            ReadWindow::new(1_000_000, 0..100_000, CONTEXT_UNITS),
            Err(ReadError::TooLarge)
        ));
    }

    #[test]
    fn aligns_surrogate_edges_without_rewriting_emoji_or_combining_text() {
        let mut window = ReadWindow::new(100_000, 50_001..50_001, CONTEXT_UNITS).unwrap();
        let mut bounds = window.range(100_000).unwrap();
        let core = "a👩‍💻e\u{301}";
        let middle = format!(
            "{core}{}",
            "x".repeat(bounds.len() - 2 - core.encode_utf16().count())
        );
        let mut units = vec![0xDE00]; // clipped low surrogate
        units.extend(middle.encode_utf16());
        units.push(0xD83D); // clipped high surrogate
        let original = bounds.clone();
        let text = window.decode(&units, &mut bounds).unwrap();
        assert_eq!(text, middle);
        assert_eq!(bounds, original.start + 1..original.end - 1);
        window.check(&text).unwrap();
        assert_eq!(
            window.decode(&[0xD83D], &mut bounds),
            Err(ReadError::WindowChanged)
        );
    }

    #[test]
    fn post_only_window_can_include_a_long_insertion_at_a_distant_caret() {
        let injected = "听写".repeat(3000);
        let total = 1_000_000 + injected.encode_utf16().count();
        let caret = 500_000 + injected.encode_utf16().count();
        let window = ReadWindow::new(
            total,
            caret..caret,
            injected.encode_utf16().count() + CONTEXT_UNITS,
        )
        .unwrap();
        let bounds = window.range(total).unwrap();
        assert!(bounds.start <= 500_000 && bounds.end >= caret);
        assert!(bounds.len() <= MAX_UNITS);
    }
}
