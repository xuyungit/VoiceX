//! In-memory P0 test status. No target text is stored in this structure.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Disabled,
    Ready,
    Verifying,
    Observing,
    ObservedUnchanged,
    ObservedEdited,
    Partial,
    Ambiguous,
    BaselineUnavailable,
    Skipped,
    ReadFailed,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    None,
    ModeExcluded,
    RemoteTargetUnsupported,
    TargetUnavailable,
    SecureInput,
    SecurityUnknown,
    UnsupportedControl,
    PermissionDenied,
    TargetChanged,
    NoValue,
    TooLarge,
    ReadTimeout,
    NativeError,
    PreReadUnavailable,
    BaselineTransformed,
    DispatchFailed,
    Interrupted,
    FieldCleared,
    RangeAmbiguous,
    BudgetExceeded,
    Timeout,
    FocusLeft,
    NextRecording,
    NextInjection,
    ElementDestroyed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub enabled: bool,
    pub status: Status,
    pub reason: Reason,
    pub native_code: Option<i32>,
    pub insertions: Option<usize>,
    pub deletions: Option<usize>,
    pub substitutions: Option<usize>,
    pub strategy: Option<&'static str>,
    pub range_evidence: Option<&'static str>,
    pub last_observed_ms: Option<u64>,
    pub had_intermediate_changes: bool,
}

static ENABLED: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());
static CURRENT: OnceLock<Mutex<Snapshot>> = OnceLock::new();

fn current() -> &'static Mutex<Snapshot> {
    CURRENT.get_or_init(|| {
        Mutex::new(Snapshot {
            enabled: false,
            status: Status::Disabled,
            reason: Reason::None,
            native_code: None,
            insertions: None,
            deletions: None,
            substitutions: None,
            strategy: None,
            range_evidence: None,
            last_observed_ms: None,
            had_intermediate_changes: false,
        })
    })
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

pub fn snapshot() -> Snapshot {
    current().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn set_enabled(enabled: bool) -> Snapshot {
    ENABLED.store(enabled, Ordering::SeqCst);
    super::coordinator::cancel();
    let mut state = current().lock().unwrap_or_else(|e| e.into_inner());
    *state = Snapshot {
        enabled,
        status: if enabled {
            Status::Ready
        } else {
            Status::Disabled
        },
        reason: Reason::None,
        native_code: None,
        insertions: None,
        deletions: None,
        substitutions: None,
        strategy: None,
        range_evidence: None,
        last_observed_ms: None,
        had_intermediate_changes: false,
    };
    state.clone()
}

/// Reject stale completions while holding the same lock that clears the view.
pub(super) fn publish(
    attempt: u64,
    status: Status,
    reason: Reason,
    native_code: Option<i32>,
    edits: Option<super::diff::NetEdits>,
) {
    let mut state = current().lock().unwrap_or_else(|e| e.into_inner());
    if !enabled() || !super::coordinator::attempt_is_current(attempt) {
        return;
    }
    state.enabled = true;
    state.status = status;
    state.reason = reason;
    state.native_code = native_code;
    state.insertions = edits.map(|e| e.insertions);
    state.deletions = edits.map(|e| e.deletions);
    state.substitutions = edits.map(|e| e.substitutions);
    if status == Status::Verifying {
        state.strategy = None;
        state.range_evidence = None;
        state.last_observed_ms = None;
        state.had_intermediate_changes = false;
    }
}

pub(super) fn context(
    attempt: u64,
    strategy: &'static str,
    evidence: &'static str,
    elapsed_ms: u64,
    intermediate: bool,
) {
    let mut state = current().lock().unwrap_or_else(|e| e.into_inner());
    if enabled() && super::coordinator::attempt_is_current(attempt) {
        state.strategy = Some(strategy);
        state.range_evidence = Some(evidence);
        state.last_observed_ms = Some(elapsed_ms);
        state.had_intermediate_changes = intermediate;
    }
}

pub(super) fn interrupted() {
    boundary(Reason::Interrupted);
}

pub(super) fn boundary(reason: Reason) {
    let mut state = current().lock().unwrap_or_else(|e| e.into_inner());
    if enabled() && matches!(state.status, Status::Verifying | Status::Observing) {
        state.status = Status::Partial;
        state.reason = reason;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::post_edit::coordinator;

    #[test]
    fn disabled_or_stale_attempts_cannot_repopulate_the_user_visible_status() {
        let _guard = TEST_LOCK.lock().unwrap();
        set_enabled(true);
        let first = coordinator::begin_attempt();
        assert_eq!(snapshot().status, Status::Verifying);
        publish(first, Status::Skipped, Reason::SecurityUnknown, None, None);
        assert_eq!(snapshot().reason, Reason::SecurityUnknown);
        let second = coordinator::begin_attempt();
        publish(
            first,
            Status::ObservedUnchanged,
            Reason::Timeout,
            None,
            None,
        );
        assert_eq!(snapshot().status, Status::Verifying);
        coordinator::report_read_error(
            second,
            crate::post_edit::platform::ReadError::SecurityUnknown,
            true,
        );
        assert_eq!(snapshot().status, Status::Partial);
        publish(second, Status::Observing, Reason::None, None, None);
        coordinator::cancel();
        assert_eq!(snapshot().status, Status::Partial);
        publish(second, Status::ObservedEdited, Reason::Timeout, None, None);
        assert_eq!(snapshot().status, Status::Partial);
        set_enabled(false);
        publish(
            second,
            Status::ObservedUnchanged,
            Reason::Timeout,
            None,
            None,
        );
        let cleared = snapshot();
        assert_eq!(cleared.status, Status::Disabled);
        assert!(!serde_json::to_string(&cleared).unwrap().contains("text"));
    }
}
