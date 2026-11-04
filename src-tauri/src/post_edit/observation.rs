use std::time::{Duration, Instant};

use super::diff::{self, NetEdits};
use super::range::{RangeError, RangeTracker, Snapshot};

const STABLE_FOR: Duration = Duration::from_millis(800);
const MAX_READS: usize = 90;

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

#[derive(Debug)]
pub struct ObservationResult {
    pub status: ObservationStatus,
    pub reason: EndReason,
    pub edits: Option<NetEdits>,
    pub had_intermediate_changes: bool,
    pub last_valid_at: Option<Instant>,
    pub read_count: usize,
    /// Two consistent samples spanning STABLE_FOR, with no newer unresolved input.
    pub stable: bool,
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
    composition: super::platform::CompositionState,
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
            composition: super::platform::CompositionState::Unknown,
        }
    }

    pub fn sample(
        &mut self,
        generation: u64,
        snapshot: &Snapshot<'_>,
        read_at: Instant,
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
                    self.unresolved = true;
                }
                return Err(SampleError::Range(error));
            }
        };
        if self.last != slice || self.unresolved {
            self.stable_since = read_at;
        }
        self.unresolved = false;
        if self.last != slice {
            self.last.clear();
            self.last.push_str(slice);
            self.had_intermediate_changes = true;
        }
        self.last_valid_at = Some(read_at);
        Ok(())
    }

    pub fn stable_at(&self) -> Instant {
        self.stable_since + STABLE_FOR
    }

    /// Composition or an unmappable sample breaks the evidence of stability.
    pub fn unresolved_input(&mut self) {
        self.unresolved = true;
    }

    pub fn is_stable(&self) -> bool {
        !self.unresolved && self.last_valid_at.is_some_and(|at| at >= self.stable_at())
    }

    pub fn composition(&self) -> super::platform::CompositionState {
        self.composition
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
        self.sample(
            generation,
            &Snapshot {
                target: snapshot.target,
                text: &snapshot.text,
                selection_utf16: snapshot.selection_utf16.clone(),
            },
            snapshot.read_at,
        )?;
        self.composition = snapshot.composition;
        Ok(())
    }

    pub fn text_pair(&self) -> (&str, &str) {
        (&self.tracker.baseline, &self.last)
    }

    /// Cached evidence is useful on send, loss of focus, and the next input.
    /// A partial count is never promoted to a valid terminal comparison.
    pub fn progress(&self, reason: EndReason) -> ObservationResult {
        let edits = diff::analyze(&self.tracker.baseline, &self.last).ok();
        ObservationResult {
            status: ObservationStatus::Partial,
            reason,
            edits,
            had_intermediate_changes: self.had_intermediate_changes,
            last_valid_at: self.last_valid_at,
            read_count: self.reads,
            stable: self.is_stable(),
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
            result.stable = false;
        } else if reason != EndReason::NextInjection && result.stable {
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
            result.stable = false;
            return result;
        }
        if reason == EndReason::NextInjection {
            return result;
        }
        let Some((snapshot, at)) = terminal else {
            return result;
        };
        if let Err(error) = self.sample(generation, snapshot, at) {
            match error {
                SampleError::Range(RangeError::FieldCleared) => {
                    result.reason = EndReason::FieldCleared
                }
                SampleError::Range(RangeError::TargetChanged) => {
                    result.reason = EndReason::TargetChanged
                }
                SampleError::Range(RangeError::BudgetExceeded)
                | SampleError::ReadBudgetExceeded => result.reason = EndReason::BudgetExceeded,
                SampleError::Range(_) => result.status = ObservationStatus::Ambiguous,
                _ => {}
            }
            result.read_count = self.reads;
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
}
