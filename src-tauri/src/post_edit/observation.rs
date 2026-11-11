use std::time::{Duration, Instant};

use super::diff::{self, NetEdits};
use super::platform::CompositionState;
use super::range::{RangeError, RangeTracker, Snapshot};

pub(super) const STABLE_FOR: Duration = Duration::from_millis(400);
pub(super) const MAX_READS: usize = 120;
const MIN_REPLACED_GRAPHEMES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Timeout,
    FocusLeft,
    NextRecording,
    NextInjection,
    FieldCleared,
    TargetChanged,
    SettingsDisabled,
    DataCleared,
    ReadFailed,
    BudgetExceeded,
    SessionCancelled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationStatus {
    ObservedUnchanged,
    ObservedEdited,
    Partial,
    Ambiguous,
    ReadFailed,
}

#[derive(Clone, Debug)]
pub struct ConfirmedSnapshot {
    pub text: String,
    pub read_at: Instant,
    pub composition: CompositionState,
}

#[derive(Debug)]
pub struct ObservationResult {
    pub status: ObservationStatus,
    pub reason: EndReason,
    pub edits: Option<NetEdits>,
    pub had_intermediate_changes: bool,
    pub last_valid_at: Option<Instant>,
    pub read_count: usize,
    /// Later input was seen or notified after the retained stable evidence.
    pub has_newer_unconfirmed_input: bool,
    /// Text, timestamp and IME evidence from the same confirmed sample.
    pub confirmed: Option<ConfirmedSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleError {
    CompositionActive,
    StaleGeneration,
    OutOfOrder,
    Range(RangeError),
    ReadBudgetExceeded,
}

/// One observation, owned by its worker. Samples from an old generation are
/// rejected before inspecting their text. Nothing is written on each sample.
pub struct Observation {
    generation: u64,
    tracker: RangeTracker,
    last: String,
    last_valid_at: Option<Instant>,
    stable_since: Instant,
    had_intermediate_changes: bool,
    reads: usize,
    unresolved: bool,
    notified: bool,
    confirmed: Option<ConfirmedSnapshot>,
    /// Whether the latest read came from a reader that reports composer resets.
    reset_detection: bool,
}

impl Observation {
    pub fn new(generation: u64, tracker: RangeTracker, baseline_at: Instant) -> Self {
        Self {
            generation,
            last: tracker.baseline.clone(),
            tracker,
            last_valid_at: Some(baseline_at),
            stable_since: baseline_at,
            had_intermediate_changes: false,
            reads: 0,
            unresolved: false,
            notified: false,
            confirmed: None,
            reset_detection: false,
        }
    }

    pub fn sample(
        &mut self,
        generation: u64,
        snapshot: &Snapshot<'_>,
        read_at: Instant,
    ) -> Result<(), SampleError> {
        self.sample_with_composition(generation, snapshot, read_at, CompositionState::Unknown)
    }

    fn sample_with_composition(
        &mut self,
        generation: u64,
        snapshot: &Snapshot<'_>,
        read_at: Instant,
        composition: CompositionState,
    ) -> Result<(), SampleError> {
        if generation != self.generation {
            return Err(SampleError::StaleGeneration);
        }
        if self.last_valid_at.is_some_and(|last| read_at < last) {
            return Err(SampleError::OutOfOrder);
        }
        if self.reads >= MAX_READS {
            return Err(SampleError::ReadBudgetExceeded);
        }
        self.reads += 1;
        let slice = match self.tracker.slice(snapshot) {
            Ok(slice) => slice,
            Err(error) => {
                if !matches!(error, RangeError::FieldCleared | RangeError::TargetChanged) {
                    self.unresolved_input();
                }
                return Err(SampleError::Range(error));
            }
        };
        if !self.reset_detection && self.last != slice {
            // Without a native reset signal, a composer that shows its hint
            // after sending looks like a whole rewrite. Keep short corrections
            // and gradual edits, but not most of the text changing at once.
            let edits = diff::analyze(&self.last, slice).map_err(|_| {
                self.unresolved_input();
                SampleError::Range(RangeError::BudgetExceeded)
            })?;
            let replaced = edits.deletions + edits.substitutions;
            if replaced >= MIN_REPLACED_GRAPHEMES && replaced > edits.baseline_graphemes / 2 {
                self.unresolved_input();
                return Err(SampleError::Range(RangeError::Ambiguous));
            }
        }
        if self.last != slice || self.unresolved {
            self.stable_since = read_at;
        }
        self.unresolved = false;
        self.notified = false;
        if self.last != slice {
            self.last.clear();
            self.last.push_str(slice);
            self.had_intermediate_changes = true;
        }
        self.last_valid_at = Some(read_at);
        if self.is_stable() {
            self.confirmed = Some(ConfirmedSnapshot {
                text: self.last.clone(),
                read_at,
                composition,
            });
        }
        Ok(())
    }

    pub fn stable_at(&self) -> Instant {
        self.stable_since + STABLE_FOR
    }

    /// A notification alone does not tell whether this is another edit or send.
    pub fn text_changed(&mut self) {
        self.notified = true;
    }

    /// A newer sample may be unusable without invalidating earlier evidence.
    pub fn unresolved_input(&mut self) {
        self.unresolved = true;
    }

    pub fn is_stable(&self) -> bool {
        !self.unresolved
            && !self.notified
            && self.last_valid_at.is_some_and(|at| at >= self.stable_at())
    }

    pub fn sample_read(
        &mut self,
        generation: u64,
        snapshot: &super::platform::OwnedSnapshot,
    ) -> Result<(), SampleError> {
        if generation != self.generation {
            return Err(SampleError::StaleGeneration);
        }
        if self
            .last_valid_at
            .is_some_and(|last| snapshot.read_at < last)
        {
            return Err(SampleError::OutOfOrder);
        }
        if snapshot.composition == super::platform::CompositionState::Active {
            self.unresolved_input();
            return Err(SampleError::CompositionActive);
        }
        self.reset_detection = snapshot.reset_detection;
        self.sample_with_composition(
            generation,
            &Snapshot {
                target: snapshot.target,
                text: &snapshot.text,
                selection_utf16: snapshot.selection_utf16.clone(),
            },
            snapshot.read_at,
            snapshot.composition,
        )
    }

    pub fn original_text(&self) -> &str {
        &self.tracker.baseline
    }

    /// Cached evidence is useful on send, loss of focus, and the next input.
    /// A partial count is never promoted to a valid terminal comparison.
    pub fn progress(&self, reason: EndReason) -> ObservationResult {
        let confirmed = self.confirmed.clone();
        let text = confirmed
            .as_ref()
            .map_or(self.last.as_str(), |s| s.text.as_str());
        let edits = diff::analyze(&self.tracker.baseline, text).ok();
        ObservationResult {
            status: ObservationStatus::Partial,
            reason,
            edits,
            had_intermediate_changes: self.had_intermediate_changes,
            last_valid_at: confirmed.as_ref().map(|s| s.read_at).or(self.last_valid_at),
            read_count: self.reads,
            has_newer_unconfirmed_input: !self.is_stable(),
            confirmed,
        }
    }

    pub fn finish(&self, reason: EndReason) -> ObservationResult {
        let mut result = self.progress(reason);
        if matches!(
            reason,
            EndReason::SessionCancelled
                | EndReason::Interrupted
                | EndReason::SettingsDisabled
                | EndReason::DataCleared
        ) {
            result.edits = None;
            result.confirmed = None;
        } else if !matches!(reason, EndReason::NextInjection | EndReason::FieldCleared)
            && self.is_stable()
            && result.confirmed.is_some()
        {
            if let Some(edits) = result.edits {
                result.status = if edits.insertions + edits.deletions + edits.substitutions == 0 {
                    ObservationStatus::ObservedUnchanged
                } else {
                    ObservationStatus::ObservedEdited
                };
            } else {
                result.reason = EndReason::BudgetExceeded;
            }
        }
        result
    }

    pub fn close(
        &mut self,
        generation: u64,
        reason: EndReason,
        terminal: Option<(&Snapshot<'_>, Instant)>,
    ) -> ObservationResult {
        let mut result = self.progress(reason);
        if generation != self.generation
            || matches!(
                reason,
                EndReason::SettingsDisabled
                    | EndReason::DataCleared
                    | EndReason::SessionCancelled
                    | EndReason::Interrupted
            )
        {
            result.edits = None;
            result.confirmed = None;
            return result;
        }
        if reason == EndReason::NextInjection {
            return result;
        }
        let Some((snapshot, at)) = terminal else {
            return result;
        };
        if let Err(error) = self.sample(generation, snapshot, at) {
            result = self.progress(reason);
            match error {
                SampleError::Range(RangeError::FieldCleared) => {
                    result.reason = EndReason::FieldCleared
                }
                SampleError::Range(RangeError::TargetChanged) => {
                    result.reason = EndReason::TargetChanged
                }
                SampleError::Range(RangeError::BudgetExceeded)
                | SampleError::ReadBudgetExceeded => result.reason = EndReason::BudgetExceeded,
                SampleError::Range(_) if result.confirmed.is_none() => {
                    result.status = ObservationStatus::Ambiguous
                }
                _ => {}
            }
            result.read_count = self.reads;
            if result.reason == EndReason::FieldCleared {
                return self.progress(EndReason::FieldCleared);
            }
            return result;
        }
        self.finish(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::post_edit::range::{RangeTracker, Snapshot};

    fn snapshot(text: &str) -> Snapshot<'_> {
        Snapshot {
            target: 1,
            text,
            selection_utf16: 0..0,
        }
    }
    fn observation(now: Instant) -> Observation {
        let before = Snapshot {
            target: 1,
            text: "前后",
            selection_utf16: 1..1,
        };
        let after = snapshot("前返利后");
        Observation::new(
            5,
            RangeTracker::from_before_after(&before, &after, "返利").unwrap(),
            now,
        )
    }

    fn whole_field(now: Instant, text: &str) -> Observation {
        Observation::new(
            5,
            RangeTracker::from_before_after(&snapshot(""), &snapshot(text), text).unwrap(),
            now,
        )
    }

    fn read(
        text: &str,
        at: Instant,
        reset_detection: bool,
    ) -> super::super::platform::OwnedSnapshot {
        super::super::platform::OwnedSnapshot {
            target: 1,
            process_id: 1,
            text: text.into(),
            selection_utf16: 0..0,
            read_at: at,
            event_driven: false,
            composition: CompositionState::Inactive,
            reset_detection,
        }
    }

    #[test]
    fn whole_rewrites_are_valid_edits_when_the_reader_reports_resets() {
        let now = Instant::now();
        for (original, edited) in [
            ("abcdefgh", "ABCDEFGH"),
            (
                "有些 release 版本和 Debug 版本，好像差异挺大的。",
                "和 Codex 一起工作\n",
            ),
        ] {
            let mut o = whole_field(now, original);
            for seconds in [1, 2] {
                o.sample_read(5, &read(edited, now + Duration::from_secs(seconds), true))
                    .unwrap();
            }
            let result = o.finish(EndReason::Timeout);
            assert_eq!(result.status, ObservationStatus::ObservedEdited);
            assert_eq!(result.confirmed.unwrap().text, edited);
            assert!(!result.has_newer_unconfirmed_input);
        }
    }

    #[test]
    fn wholesale_replacements_stay_ambiguous_without_reset_detection() {
        let now = Instant::now();
        let mut o = whole_field(now, "请检查这段听写原文");
        for seconds in [1, 2] {
            o.sample_read(
                5,
                &read(
                    "请检查这段听写正文",
                    now + Duration::from_secs(seconds),
                    false,
                ),
            )
            .unwrap();
        }
        for seconds in [3, 4, 5] {
            assert_eq!(
                o.sample_read(
                    5,
                    &read("输入你的问题", now + Duration::from_secs(seconds), false)
                ),
                Err(SampleError::Range(RangeError::Ambiguous))
            );
        }
        let result = o.finish(EndReason::Timeout);
        assert_eq!(result.status, ObservationStatus::Partial);
        assert!(result.has_newer_unconfirmed_input);
        assert_eq!(result.confirmed.unwrap().text, "请检查这段听写正文");
        // A rejected hint must neither become the comparison text nor prevent
        // a later, attributable correction from being confirmed.
        for seconds in [6, 7] {
            o.sample_read(
                5,
                &read(
                    "请检查这段听写内容",
                    now + Duration::from_secs(seconds),
                    false,
                ),
            )
            .unwrap();
        }
        assert_eq!(
            o.finish(EndReason::Timeout).confirmed.unwrap().text,
            "请检查这段听写内容"
        );
        assert!(o.is_stable());
    }

    #[test]
    fn short_corrections_and_gradual_rewrites_keep_their_continuity() {
        let now = Instant::now();
        let mut o = whole_field(now, "abcdefgh");
        for (index, text) in ["ABcdefgh", "ABCDefgh", "ABCDEFgh", "ABCDEFGH", "ABCDEFGH"]
            .into_iter()
            .enumerate()
        {
            o.sample(
                5,
                &snapshot(text),
                now + Duration::from_secs(index as u64 + 1),
            )
            .unwrap();
        }
        let result = o.finish(EndReason::Timeout);
        assert_eq!(result.status, ObservationStatus::ObservedEdited);
        assert_eq!(result.confirmed.unwrap().text, "ABCDEFGH");
        // Even a complete replacement of a short word can be a correction.
        let mut o = whole_field(now, "返利");
        for seconds in [1, 2] {
            o.sample(5, &snapshot("反力"), now + Duration::from_secs(seconds))
                .unwrap();
        }
        assert_eq!(
            o.finish(EndReason::Timeout).status,
            ObservationStatus::ObservedEdited
        );
    }

    #[test]
    fn clear_notification_preserves_the_confirmed_text_time_and_composition_as_partial() {
        let now = Instant::now();
        let confirmed_at = now + Duration::from_secs(2);
        let mut o = observation(now);
        for at in [now + Duration::from_secs(1), confirmed_at] {
            o.sample_with_composition(5, &snapshot("前反力后"), at, CompositionState::Inactive)
                .unwrap();
        }
        o.text_changed();
        assert!(!o.is_stable());
        assert!(o.progress(EndReason::Timeout).has_newer_unconfirmed_input);
        let result = o.close(
            5,
            EndReason::Timeout,
            Some((&snapshot(""), now + Duration::from_secs(3))),
        );
        assert_eq!(result.status, ObservationStatus::Partial);
        assert_eq!(result.reason, EndReason::FieldCleared);
        assert_eq!(result.last_valid_at, Some(confirmed_at));
        let confirmed = result.confirmed.unwrap();
        assert_eq!(confirmed.text, "反力");
        assert_eq!(confirmed.read_at, confirmed_at);
        assert_eq!(confirmed.composition, CompositionState::Inactive);
    }

    #[test]
    fn later_unconfirmed_input_preserves_stable_evidence_without_promoting_it() {
        let now = Instant::now();
        for next in ["前反力续写后", "前反后"] {
            let mut o = observation(now);
            for seconds in [1, 2] {
                o.sample(5, &snapshot("前反力后"), now + Duration::from_secs(seconds))
                    .unwrap();
            }
            let _ = o.sample(5, &snapshot(next), now + Duration::from_secs(3));
            let result = o.finish(EndReason::Timeout);
            assert_eq!(result.status, ObservationStatus::Partial);
            assert!(result.has_newer_unconfirmed_input);
            assert_eq!(result.edits.unwrap().substitutions, 2);
            let confirmed = result.confirmed.unwrap();
            assert_eq!(confirmed.text, "反力");
            assert_eq!(confirmed.read_at, now + Duration::from_secs(2));
        }
    }

    #[test]
    fn external_edits_over_budget_preserve_the_last_stable_slice() {
        let now = Instant::now();
        let original = format!("{}支座返力{}", "a".repeat(2048), "z".repeat(2044));
        let corrected = original.replace("返力", "反力");
        let tracker = RangeTracker::from_post_anchored(
            &Snapshot {
                target: 1,
                text: &original,
                selection_utf16: 2052..2052,
            },
            "支座返力",
        )
        .unwrap();
        let mut o = Observation::new(5, tracker, now);
        for seconds in [1, 2] {
            o.sample(5, &snapshot(&corrected), now + Duration::from_secs(seconds))
                .unwrap();
        }
        let changed = format!("{}{}", "b".repeat(300), &corrected[300..]);
        assert_eq!(
            o.sample(5, &snapshot(&changed), now + Duration::from_secs(3)),
            Err(SampleError::Range(RangeError::BudgetExceeded))
        );
        let result = o.finish(EndReason::BudgetExceeded);
        assert_eq!(result.status, ObservationStatus::Partial);
        assert!(result.has_newer_unconfirmed_input);
        assert_eq!(result.confirmed.unwrap().text, "支座反力");
    }

    #[test]
    fn stable_final_edit_counts_and_undo_is_not_a_net_edit() {
        let now = Instant::now();
        let mut o = observation(now);
        o.sample(5, &snapshot("前反力后"), now + Duration::from_secs(1))
            .unwrap();
        let result = o.close(
            5,
            EndReason::Timeout,
            Some((&snapshot("前反力后"), now + Duration::from_secs(2))),
        );
        assert_eq!(result.status, ObservationStatus::ObservedEdited);
        assert_eq!(result.edits.unwrap().substitutions, 2);
        let mut o = observation(now);
        o.sample(5, &snapshot("前反力后"), now + Duration::from_secs(1))
            .unwrap();
        o.sample(5, &snapshot("前返利后"), now + Duration::from_secs(2))
            .unwrap();
        let result = o.close(
            5,
            EndReason::Timeout,
            Some((&snapshot("前返利后"), now + Duration::from_secs(3))),
        );
        assert_eq!(result.status, ObservationStatus::ObservedUnchanged);
        assert!(result.had_intermediate_changes);
    }

    #[test]
    fn clearing_and_new_injection_never_become_final_deletions() {
        let now = Instant::now();
        let result = observation(now).close(
            5,
            EndReason::Timeout,
            Some((&snapshot(""), now + Duration::from_secs(3))),
        );
        assert_eq!(
            (result.status, result.reason),
            (ObservationStatus::Partial, EndReason::FieldCleared)
        );
        let result = observation(now).close(
            5,
            EndReason::NextInjection,
            Some((&snapshot("前新输入后"), now + Duration::from_secs(3))),
        );
        assert_eq!(result.status, ObservationStatus::Partial);
        assert_eq!(result.edits.unwrap().substitutions, 0);
    }

    #[test]
    fn old_generation_and_unstable_terminal_are_not_valid_results() {
        let now = Instant::now();
        let mut o = observation(now);
        assert_eq!(
            o.sample(4, &snapshot("前秘密后"), now),
            Err(SampleError::StaleGeneration)
        );
        let result = o.close(
            5,
            EndReason::Timeout,
            Some((&snapshot("前反力后"), now + Duration::from_secs(1))),
        );
        assert_eq!(result.status, ObservationStatus::Partial);
        assert_eq!(result.edits.unwrap().substitutions, 2);
    }

    #[test]
    fn last_edits_and_timestamp_survive_clear_or_a_missing_terminal_read() {
        let now = Instant::now();
        for reason in [
            EndReason::FieldCleared,
            EndReason::FocusLeft,
            EndReason::NextInjection,
        ] {
            let mut o = observation(now);
            let edited_at = now + Duration::from_secs(1);
            o.sample(5, &snapshot("前反力后"), edited_at).unwrap();
            let result = o.close(5, reason, None);
            assert_eq!(result.status, ObservationStatus::Partial);
            assert_eq!(result.last_valid_at, Some(edited_at));
            assert_eq!(result.edits.unwrap().substitutions, 2);
        }
    }

    #[test]
    fn reserved_terminal_sample_confirms_an_edit_first_seen_after_the_last_checkpoint() {
        let now = Instant::now();
        let mut o = observation(now);
        o.sample(5, &snapshot("前返利后"), now + Duration::from_secs(30))
            .unwrap();
        o.sample(
            5,
            &snapshot("前反力后"),
            now + Duration::from_millis(59_200),
        )
        .unwrap();
        let result = o.close(
            5,
            EndReason::Timeout,
            Some((&snapshot("前反力后"), now + Duration::from_secs(60))),
        );
        assert_eq!(result.status, ObservationStatus::ObservedEdited);
        assert_eq!(result.edits.unwrap().substitutions, 2);
    }

    #[test]
    fn late_text_changed_notification_for_identical_slice_does_not_reset_stability() {
        let now = Instant::now();
        let mut o = observation(now);
        o.sample(5, &snapshot("前反力后"), now + Duration::from_millis(100))
            .unwrap();
        // A late or coalesced AXValueChanged notification arrives after the read
        // that already saw "前反力后".
        o.text_changed();
        o.sample(5, &snapshot("前反力后"), now + Duration::from_millis(550))
            .unwrap();
        let result = o.finish(EndReason::Timeout);
        assert_eq!(result.status, ObservationStatus::ObservedEdited);
        assert!(!result.has_newer_unconfirmed_input);
        assert_eq!(result.confirmed.unwrap().text, "反力");
    }
}
