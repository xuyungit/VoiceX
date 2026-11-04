use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::InjectorError;

/// A successful dispatch only confirms system input calls, not text insertion.
#[derive(Debug, PartialEq, Eq)]
pub enum DispatchOutcome {
    CommandsPosted,
    CancelledBeforeDispatch,
    PartialOrUncertain,
    Failed { code: &'static str },
}

/// The app resolved by the existing per-app injection configuration lookup.
/// This is not, by itself, proof of the control that received input.
#[derive(Debug, Clone)]
pub struct ResolvedApp {
    pub process_id: u32,
    pub app_name: Option<String>,
    pub remote_bridge: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct AttemptIdentity {
    pub session_id: uuid::Uuid,
    pub record_id: uuid::Uuid,
    pub injection_epoch: u64,
}

#[derive(Debug)]
pub struct InjectionReceipt {
    pub session_id: uuid::Uuid,
    pub record_id: uuid::Uuid,
    pub injection_id: uuid::Uuid,
    pub injection_epoch: u64,
    pub outcome: DispatchOutcome,
    pub dispatch_started_at: Option<Instant>,
    pub dispatch_finished_at: Option<Instant>,
    pub resolved_app: Option<ResolvedApp>,
}

#[derive(Default)]
struct Progress {
    started: Option<Instant>,
    finished: Option<Instant>,
    resolved_app: Option<ResolvedApp>,
}

#[derive(Clone)]
pub(super) struct DispatchTracker {
    injection_id: uuid::Uuid,
    progress: Arc<Mutex<Progress>>,
}

impl Default for DispatchTracker {
    fn default() -> Self {
        Self {
            injection_id: uuid::Uuid::new_v4(),
            progress: Arc::new(Mutex::new(Progress::default())),
        }
    }
}

impl DispatchTracker {
    pub(super) fn resolved_app(&self, app: Option<ResolvedApp>) {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resolved_app = app;
    }
    /// Called immediately before a native input call: even a failed call can
    /// have posted part of its input, especially for compound Windows calls.
    pub(super) fn attempting_input(&self) {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        progress.started.get_or_insert(now);
        progress.finished = Some(now);
    }

    pub(super) fn input_returned(&self) {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finished = Some(Instant::now());
    }

    pub(super) fn finish(
        self,
        identity: AttemptIdentity,
        result: Result<(), InjectorError>,
    ) -> InjectionReceipt {
        let progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        let outcome = match result {
            Ok(()) if progress.started.is_some() => DispatchOutcome::CommandsPosted,
            Ok(()) => DispatchOutcome::Failed {
                code: "no_input_dispatched",
            },
            Err(_) if progress.started.is_some() => DispatchOutcome::PartialOrUncertain,
            Err(InjectorError::Cancelled) => DispatchOutcome::CancelledBeforeDispatch,
            Err(error) => DispatchOutcome::Failed { code: error.code() },
        };
        InjectionReceipt {
            session_id: identity.session_id,
            record_id: identity.record_id,
            injection_id: self.injection_id,
            injection_epoch: identity.injection_epoch,
            outcome,
            dispatch_started_at: progress.started,
            dispatch_finished_at: progress.finished,
            resolved_app: progress.resolved_app.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(epoch: u64) -> AttemptIdentity {
        AttemptIdentity {
            session_id: uuid::Uuid::new_v4(),
            record_id: uuid::Uuid::new_v4(),
            injection_epoch: epoch,
        }
    }

    #[test]
    fn cancellation_and_errors_after_dispatch_are_never_reported_as_undispatched() {
        let tracker = DispatchTracker::default();
        assert_eq!(
            tracker
                .finish(identity(1), Err(InjectorError::Cancelled))
                .outcome,
            DispatchOutcome::CancelledBeforeDispatch
        );
        let tracker = DispatchTracker::default();
        tracker.attempting_input();
        let receipt = tracker.finish(identity(2), Err(InjectorError::Cancelled));
        assert_eq!(receipt.outcome, DispatchOutcome::PartialOrUncertain);
        assert!(receipt.dispatch_started_at <= receipt.dispatch_finished_at);
        let tracker = DispatchTracker::default();
        tracker.attempting_input();
        assert_eq!(
            tracker
                .finish(
                    identity(3),
                    Err(InjectorError::TypingError("native".into()))
                )
                .outcome,
            DispatchOutcome::PartialOrUncertain
        );
    }

    #[test]
    fn successful_attempts_have_distinct_ids_and_require_an_input_call() {
        let tracker = DispatchTracker::default();
        assert_eq!(
            tracker.finish(identity(0), Ok(())).outcome,
            DispatchOutcome::Failed {
                code: "no_input_dispatched"
            }
        );
        let first = DispatchTracker::default();
        first.attempting_input();
        let first = first.finish(identity(7), Ok(()));
        let second = DispatchTracker::default();
        second.attempting_input();
        let second = second.finish(identity(7), Ok(()));
        assert_eq!(first.outcome, DispatchOutcome::CommandsPosted);
        assert_ne!(first.injection_id, second.injection_id);
        assert_eq!(first.injection_epoch, 7);
    }
}
