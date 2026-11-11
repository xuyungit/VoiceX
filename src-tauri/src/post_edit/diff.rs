use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

pub const ALGORITHM_VERSION: &str = "grapheme-levenshtein-v1";
// Deterministic work/memory limit. Results never depend on elapsed wall time.
const MAX_CELLS: usize = 1_000_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NetEdits {
    pub baseline_graphemes: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub substitutions: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffError {
    BudgetExceeded,
}

/// Coordinates are grapheme offsets in the two original strings. Adjacent
/// edits are kept together so a replacement cannot hide a boundary insertion.
#[derive(Debug, PartialEq, Eq)]
pub struct Change {
    pub before: Range<usize>,
    pub after: Range<usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TrackingError {
    Ambiguous,
    BudgetExceeded,
}

/// A chosen minimum edit script alone cannot identify a range: deleting either
/// X in AXX gives AX. Check both boundaries against *all* optimal alignments.
/// Boundary transitions are anchored on the outer neighbors (`range.start - 1
/// -> range.start` and `range.end -> range.end + 1`) so that length-changing
/// edits at the boundary do not fail solely due to equivalent `Sub+Ins` vs
/// `Ins+Sub` orderings; pure insertions at `range.start` or `range.end` are
/// included in the returned range and must be validated by the caller (`RangeTracker::slice`)
/// against `changes`.
/// A distance-d diagonal band keeps ordinary small edits linear in field size.
pub fn track_changes(
    before: &str,
    after: &str,
    range: Range<usize>,
) -> Result<(Vec<Change>, Range<usize>), TrackingError> {
    let a: Vec<_> = before.graphemes(true).collect();
    if range.is_empty() || range.end > a.len() {
        return Err(TrackingError::Ambiguous);
    }
    let (edits, changes) = compute(before, after).map_err(|_| TrackingError::BudgetExceeded)?;
    let d = edits.insertions + edits.deletions + edits.substitutions;
    if d == 0 {
        return Ok((changes, range));
    }
    let b: Vec<_> = after.graphemes(true).collect();
    let width = 2 * d + 1;
    let cells = (a.len() + 1)
        .checked_mul(width)
        .filter(|n| *n <= MAX_CELLS)
        .ok_or(TrackingError::BudgetExceeded)?;
    let infinity = d + 1;
    let index = |i: usize, j: usize| -> Option<usize> {
        (i <= a.len() && j <= b.len() && i.abs_diff(j) <= d).then(|| i * width + j + d - i)
    };
    let mut costs = vec![infinity; cells];
    for i in 0..=a.len() {
        for j in i.saturating_sub(d)..=b.len().min(i + d) {
            let get = |i, j| index(i, j).map_or(infinity, |idx| costs[idx]);
            let cost = if i == 0 {
                j
            } else if j == 0 {
                i
            } else {
                (get(i - 1, j - 1) + usize::from(a[i - 1] != b[j - 1]))
                    .min(get(i - 1, j) + 1)
                    .min(get(i, j - 1) + 1)
            };
            costs[index(i, j).unwrap()] = cost.min(infinity);
        }
    }
    let mut mapped = [
        (range.start == 0).then_some(0),
        (range.end == a.len()).then_some(b.len()),
    ];
    let mut visited = vec![false; cells];
    let mut pending = vec![(a.len(), b.len())];
    visited[index(a.len(), b.len()).unwrap()] = true;
    while let Some((i, j)) = pending.pop() {
        let current = costs[index(i, j).unwrap()];
        for (next_i, next_j, cost) in [
            (
                i.checked_sub(1),
                j.checked_sub(1),
                if i > 0 && j > 0 {
                    usize::from(a[i - 1] != b[j - 1])
                } else {
                    0
                },
            ),
            (i.checked_sub(1), Some(j), 1),
            (Some(i), j.checked_sub(1), 1),
        ] {
            if let (Some(prev_i), Some(prev_j)) = (next_i, next_j) {
                if let Some(idx) = index(prev_i, prev_j) {
                    if costs[idx] + cost == current {
                        if i == range.start && prev_i + 1 == range.start {
                            if mapped[0].is_some_and(|previous| previous != j) {
                                return Err(TrackingError::Ambiguous);
                            }
                            mapped[0] = Some(j);
                        }
                        if prev_i == range.end && i == range.end + 1 {
                            if mapped[1].is_some_and(|previous| previous != prev_j) {
                                return Err(TrackingError::Ambiguous);
                            }
                            mapped[1] = Some(prev_j);
                        }
                        if !visited[idx] {
                            visited[idx] = true;
                            pending.push((prev_i, prev_j));
                        }
                    }
                }
            }
        }
    }
    let (Some(start), Some(end)) = (mapped[0], mapped[1]) else {
        return Err(TrackingError::Ambiguous);
    };
    if start > end {
        return Err(TrackingError::Ambiguous);
    }
    Ok((changes, start..end))
}

pub fn changes(before: &str, after: &str) -> Result<Vec<Change>, DiffError> {
    compute(before, after).map(|(_, changes)| changes)
}

/// Unit-cost Levenshtein counts and range mapping use the same edit script.
pub fn analyze(before: &str, after: &str) -> Result<NetEdits, DiffError> {
    compute(before, after).map(|(counts, _)| counts)
}

fn compute(before: &str, after: &str) -> Result<(NetEdits, Vec<Change>), DiffError> {
    let a: Vec<_> = before.graphemes(true).collect();
    let b: Vec<_> = after.graphemes(true).collect();
    if before.encode_utf16().count() > 16_384 || after.encode_utf16().count() > 16_384 {
        return Err(DiffError::BudgetExceeded);
    }
    let mut counts = NetEdits {
        baseline_graphemes: a.len(),
        ..Default::default()
    };
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(a.len().min(b.len()) - prefix)
        .take_while(|(x, y)| x == y)
        .count();
    let (a, b) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let width = b.len() + 1;
    let cells = (a.len() + 1)
        .checked_mul(width)
        .filter(|n| *n <= MAX_CELLS)
        .ok_or(DiffError::BudgetExceeded)?;
    let mut costs = vec![0usize; cells];
    for i in 0..=a.len() {
        costs[i * width] = i;
    }
    for j in 0..width {
        costs[j] = j;
    }
    for i in 1..=a.len() {
        for j in 1..width {
            costs[i * width + j] = (costs[(i - 1) * width + j - 1]
                + usize::from(a[i - 1] != b[j - 1]))
            .min(costs[(i - 1) * width + j] + 1)
            .min(costs[i * width + j - 1] + 1);
        }
    }
    let (mut i, mut j) = (a.len(), b.len());
    let mut result: Vec<Change> = Vec::new();
    let mut pending: Option<Change> = None;
    while i > 0 || j > 0 {
        let (old_i, old_j) = (i, j);
        let equal = i > 0
            && j > 0
            && a[i - 1] == b[j - 1]
            && costs[i * width + j] == costs[(i - 1) * width + j - 1];
        if equal || (i > 0 && j > 0 && costs[i * width + j] == costs[(i - 1) * width + j - 1] + 1) {
            if !equal {
                counts.substitutions += 1;
            }
            i -= 1;
            j -= 1;
        } else if i > 0 && costs[i * width + j] == costs[(i - 1) * width + j] + 1 {
            counts.deletions += 1;
            i -= 1;
        } else {
            counts.insertions += 1;
            j -= 1;
        }
        if equal {
            if let Some(change) = pending.take() {
                result.push(change);
            }
        } else {
            let change = pending.get_or_insert(Change {
                before: prefix + i..prefix + old_i,
                after: prefix + j..prefix + old_j,
            });
            change.before.start = prefix + i;
            change.after.start = prefix + j;
        }
    }
    if let Some(change) = pending {
        result.push(change);
    }
    result.reverse();
    Ok((counts, result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_unicode_graphemes_and_net_changes() {
        assert_eq!(
            analyze("支座返利 👩‍💻", "支座反力 👩‍💻!").unwrap(),
            NetEdits {
                baseline_graphemes: 6,
                insertions: 1,
                deletions: 0,
                substitutions: 2
            }
        );
        assert_eq!(
            analyze("e\u{301}", "é").unwrap(),
            NetEdits {
                baseline_graphemes: 1,
                substitutions: 1,
                ..Default::default()
            }
        );
        assert_eq!(analyze("相同", "相同").unwrap().substitutions, 0);
    }

    #[test]
    fn rejects_work_over_budget_without_returning_guessed_counts() {
        assert_eq!(
            analyze(&"a".repeat(1100), &"b".repeat(1100)),
            Err(DiffError::BudgetExceeded)
        );
    }

    #[test]
    fn boundary_mapping_checks_all_optimal_alignments_without_quadratic_field_work() {
        assert_eq!(
            track_changes("AXX", "AX", 2..3),
            Err(TrackingError::Ambiguous)
        );
        assert_eq!(
            track_changes("abab", "ab", 2..4),
            Err(TrackingError::Ambiguous)
        );
        let before = format!("{}支座返力{}", "a".repeat(4000), "b".repeat(4000));
        let after = before.replace("返力", "反力");
        let (_, range) = track_changes(&before, &after, 4000..4004).unwrap();
        assert_eq!(range, 4000..4004);
        let (_, grown) = track_changes("前支座返力后", "前混凝土反力后", 1..5).unwrap();
        assert_eq!(grown, 1..6);
    }

    #[test]
    fn range_work_is_bounded_even_when_trimmed_text_analysis_is_small() {
        let before = "a".repeat(12000);
        let after = format!("{}{}", "b".repeat(50), "a".repeat(11950));
        assert_eq!(analyze(&before, &after).unwrap().substitutions, 50);
        assert_eq!(
            track_changes(&before, &after, 100..110),
            Err(TrackingError::BudgetExceeded)
        );
    }
}
