//! Opt-in observation with separately consented local text-pair storage.
use super::diagnostics::{self, Reason, Status};
use super::observation::{
    EndReason, Observation, ObservationResult, ObservationStatus, SampleError,
};
use super::platform::{
    ObserverWorker, OwnedSnapshot, ReadError, CONTROL, DESTROYED, FOCUS_CHANGED, TEXT_CHANGED,
};
use super::range::{RangeTracker, Snapshot};
use crate::injector::{DispatchOutcome, InjectionReceipt};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
const READ_TIMEOUT: Duration = Duration::from_millis(300);
pub fn known_remote_target(
    app: &crate::foreground_app::ForegroundAppInfo,
    clipboard_bridge: bool,
) -> bool {
    clipboard_bridge
        || app
            .bundle_id
            .as_deref()
            .is_some_and(|id| id.eq_ignore_ascii_case("com.microsoft.rdc.macos"))
        || app.process_name.as_deref().is_some_and(|name| {
            ["mstsc.exe", "msrdc.exe", "windowsapp.exe"]
                .iter()
                .any(|candidate| name.eq_ignore_ascii_case(candidate))
        })
}

type Stop = Arc<Mutex<Option<EndReason>>>;
struct Coordinator {
    worker: Arc<ObserverWorker>,
    active: Mutex<Option<(u64, Stop)>>,
}
static COORDINATOR: OnceLock<Option<Arc<Coordinator>>> = OnceLock::new();
static ATTEMPT: AtomicU64 = AtomicU64::new(0);
fn coordinator() -> Option<Arc<Coordinator>> {
    if !diagnostics::enabled() {
        return None;
    }
    COORDINATOR
        .get_or_init(|| match ObserverWorker::start() {
            Ok(worker) => Some(Arc::new(Coordinator {
                worker: Arc::new(worker),
                active: Mutex::new(None),
            })),
            Err(error) => {
                log::warn!("Post-edit worker could not start: {error}");
                None
            }
        })
        .clone()
}
fn existing() -> Option<&'static Arc<Coordinator>> {
    COORDINATOR.get().and_then(Option::as_ref)
}
/// Privacy/cancellation invalidation discards all in-flight native results.
pub fn cancel() {
    ATTEMPT.fetch_add(1, Ordering::SeqCst);
    if let Some(c) = existing() {
        c.invalidate();
    }
    diagnostics::interrupted();
}
/// Recording starts immediately; only the observation thread attempts a final read.
pub fn next_recording() {
    if let Some(c) = existing() {
        if let Some((generation, stop)) = c.active.lock().unwrap().as_ref() {
            *stop.lock().unwrap() = Some(EndReason::NextRecording);
            c.worker.events.notify(*generation, CONTROL);
        }
    }
}
/// All injection entry points call this under their existing serialization lock,
/// before resolving/preparing the new target. No native call is awaited here.
pub fn dispatch_boundary() {
    if let Some(c) = existing() {
        let active = c.active.lock().unwrap().is_some();
        if active {
            ATTEMPT.fetch_add(1, Ordering::SeqCst);
            diagnostics::boundary(Reason::NextInjection);
            c.invalidate();
        }
    }
}
pub fn begin_attempt() -> u64 {
    let attempt = ATTEMPT.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    if let Some(c) = existing() {
        c.invalidate();
    }
    diagnostics::publish(attempt, Status::Verifying, Reason::None, None, None);
    attempt
}
pub(super) fn attempt_is_current(attempt: u64) -> bool {
    ATTEMPT.load(Ordering::SeqCst) == attempt
}
pub fn skipped(attempt: u64, reason: Reason) {
    diagnostics::publish(attempt, Status::Skipped, reason, None, None);
}
impl Coordinator {
    fn invalidate(&self) {
        let mut active = self.active.lock().unwrap();
        self.worker.invalidate();
        if let Some((_, stop)) = active.take() {
            *stop.lock().unwrap() = Some(EndReason::NextInjection);
        }
    }
    fn finished(&self, generation: u64) {
        let mut active = self.active.lock().unwrap();
        self.worker.invalidate_if_current(generation);
        if active.as_ref().is_some_and(|(id, _)| *id == generation) {
            *active = None;
        }
    }
}
pub struct Pending {
    coordinator: Arc<Coordinator>,
    generation: u64,
    attempt: u64,
    before: Option<mpsc::Receiver<Result<OwnedSnapshot, ReadError>>>,
    stop: Stop,
    handed_off: bool,
    capture_ticket: Option<super::records::CaptureTicket>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if !self.handed_off {
            self.coordinator.finished(self.generation);
        }
    }
}
pub fn prepare(attempt: u64, expected_pid: u32) -> Option<Pending> {
    if !diagnostics::enabled() {
        return None;
    }
    let Some(c) = coordinator() else {
        diagnostics::publish(attempt, Status::ReadFailed, Reason::NativeError, None, None);
        return None;
    };
    let stop = Arc::new(Mutex::new(None));
    let generation = {
        let mut active = c.active.lock().unwrap();
        if !attempt_is_current(attempt) {
            return None;
        }
        let generation = c.worker.invalidate();
        *active = Some((generation, Arc::clone(&stop)));
        generation
    };
    let (tx, rx) = mpsc::sync_channel(1);
    let worker = Arc::clone(&c.worker);
    if std::thread::Builder::new()
        .name("post-edit-preread".into())
        .spawn(move || {
            let _ = tx.try_send(worker.capture(generation, expected_pid, READ_TIMEOUT));
        })
        .is_err()
    {
        diagnostics::publish(attempt, Status::ReadFailed, Reason::NativeError, None, None);
        c.finished(generation);
        return None;
    }
    Some(Pending {
        coordinator: c,
        generation,
        attempt,
        before: Some(rx),
        stop,
        handed_off: false,
        capture_ticket: super::records::capture_ticket(),
    })
}
impl Pending {
    pub fn dispatched(self, receipt: InjectionReceipt, injected: String) {
        self.dispatched_with_schedule(
            receipt,
            injected,
            [
                Duration::from_secs(10),
                Duration::from_secs(30),
                Duration::from_secs(60),
            ],
        );
    }
    fn dispatched_with_schedule(
        mut self,
        receipt: InjectionReceipt,
        injected: String,
        schedule: [Duration; 3],
    ) {
        let Some(before) = self.before.take() else {
            return;
        };
        let c = Arc::clone(&self.coordinator);
        let (generation, attempt, stop) = (self.generation, self.attempt, Arc::clone(&self.stop));
        let capture_ticket = self.capture_ticket;
        match std::thread::Builder::new()
            .name("post-edit-session".into())
            .spawn(move || {
                run(
                    &c.worker,
                    generation,
                    attempt,
                    before,
                    stop,
                    receipt,
                    injected,
                    schedule,
                    capture_ticket,
                );
                c.finished(generation);
            }) {
            Ok(_) => self.handed_off = true,
            Err(error) => {
                log::warn!("Post-edit session could not start: {error}");
                diagnostics::publish(attempt, Status::ReadFailed, Reason::NativeError, None, None);
            }
        }
    }
}
fn as_snapshot(s: &OwnedSnapshot) -> Snapshot<'_> {
    Snapshot {
        target: s.target,
        text: &s.text,
        selection_utf16: s.selection_utf16.clone(),
    }
}
fn current(worker: &ObserverWorker, generation: u64, attempt: u64) -> bool {
    diagnostics::enabled() && worker.is_current(generation) && attempt_is_current(attempt)
}
fn current_error(worker: &ObserverWorker, generation: u64, attempt: u64, error: ReadError) -> bool {
    diagnostics::enabled()
        && attempt_is_current(attempt)
        && (worker.is_current(generation)
            || (error == ReadError::TimedOut && worker.is_current(generation.wrapping_add(1))))
}
fn wait_until(worker: &ObserverWorker, generation: u64, stop: &Stop, deadline: Instant) -> bool {
    while Instant::now() < deadline && worker.is_current(generation) {
        if stop.lock().unwrap().is_some() {
            return false;
        }
        if worker.events.wait(
            generation,
            deadline.saturating_duration_since(Instant::now()),
        ) & (FOCUS_CHANGED | DESTROYED)
            != 0
        {
            return false;
        }
    }
    worker.is_current(generation) && stop.lock().unwrap().is_none()
}
fn run(
    worker: &ObserverWorker,
    generation: u64,
    attempt: u64,
    before: mpsc::Receiver<Result<OwnedSnapshot, ReadError>>,
    stop: Stop,
    receipt: InjectionReceipt,
    injected: String,
    schedule: [Duration; 3],
    capture_ticket: Option<super::records::CaptureTicket>,
) {
    // A pre-read timeout invalidates its generation before this thread starts.
    // Consume that error below so the view cannot remain stuck at Verifying.
    if !diagnostics::enabled() || !attempt_is_current(attempt) {
        return;
    }
    if receipt.outcome != DispatchOutcome::CommandsPosted {
        skipped(attempt, Reason::DispatchFailed);
        return;
    }
    let (Some(started), Some(finished), Some(app)) = (
        receipt.dispatch_started_at,
        receipt.dispatch_finished_at,
        receipt.resolved_app.as_ref(),
    ) else {
        skipped(attempt, Reason::TargetUnavailable);
        return;
    };
    if app.remote_bridge {
        skipped(attempt, Reason::RemoteTargetUnsupported);
        return;
    }
    // Waiting here is off the injection/HUD/audio path. A late pre-read can
    // establish the target, but cannot be used as pre-dispatch evidence.
    let pre = match before.recv_timeout(READ_TIMEOUT) {
        Ok(Ok(snapshot)) => Some(snapshot),
        Ok(Err(ReadError::NoValue)) => None,
        Ok(Err(error)) => {
            if current_error(worker, generation, attempt, error) {
                report_read_error(attempt, error, false);
            }
            return;
        }
        Err(_) => {
            if current(worker, generation, attempt) {
                report_read_error(attempt, ReadError::TimedOut, false);
            }
            return;
        }
    };
    if pre
        .as_ref()
        .is_some_and(|pre| pre.process_id != app.process_id)
    {
        skipped(attempt, Reason::TargetChanged);
        return;
    }
    let verified_pre = pre.as_ref().is_some_and(|pre| pre.read_at <= started);
    let mut baseline = None;
    let mut native_reads = 1;
    for delay in [120, 300, 700] {
        if !wait_until(
            worker,
            generation,
            &stop,
            finished + Duration::from_millis(delay),
        ) {
            if current(worker, generation, attempt) {
                diagnostics::publish(
                    attempt,
                    Status::BaselineUnavailable,
                    Reason::Interrupted,
                    None,
                    None,
                );
            }
            return;
        }
        native_reads += 1;
        let read = if pre.is_some() {
            worker.read(generation, READ_TIMEOUT)
        } else {
            worker.capture(generation, app.process_id, READ_TIMEOUT)
        };
        let after = match read {
            Ok(s) => s,
            Err(ReadError::NoValue) if delay < 700 => continue,
            Err(e) => {
                if current_error(worker, generation, attempt, e) {
                    report_read_error(attempt, e, false);
                }
                return;
            }
        };
        if !current(worker, generation, attempt) {
            return;
        }
        let tracker = if verified_pre {
            RangeTracker::from_before_after(
                &as_snapshot(pre.as_ref().unwrap()),
                &as_snapshot(&after),
                &injected,
            )
        } else {
            RangeTracker::from_post_anchored(&as_snapshot(&after), &injected)
        };
        if let Ok(tracker) = tracker {
            baseline = Some((tracker, after.read_at, after.event_driven));
            break;
        }
    }
    let Some((tracker, baseline_at, event_driven)) = baseline else {
        diagnostics::publish(
            attempt,
            Status::BaselineUnavailable,
            Reason::BaselineTransformed,
            None,
            None,
        );
        return;
    };
    drop(pre);
    drop(injected);
    let evidence = match tracker.evidence {
        super::range::RangeEvidence::BeforeAfterVerified => "before_after_verified",
        _ => "post_anchored",
    };
    let strategy = if event_driven {
        "event_with_checkpoints"
    } else {
        "checkpoint_only"
    };
    let mut observation = Observation::new(generation, tracker, baseline_at);
    diagnostics::publish(attempt, Status::Observing, Reason::None, None, None);
    diagnostics::context(attempt, strategy, evidence, 0, false);
    let end = baseline_at + schedule[2];
    // Reserve a stable comparison before the hard deadline, including for
    // providers without notifications. Never extend the observation duration.
    let mut deadlines = vec![
        baseline_at + schedule[0],
        baseline_at + schedule[1],
        end.checked_sub(Duration::from_millis(800) + READ_TIMEOUT)
            .unwrap_or(baseline_at),
        end,
    ];
    deadlines.sort();
    deadlines.dedup();
    let mut checkpoint = 0;
    let mut last_read = baseline_at;
    let mut dirty_due: Option<Instant> = None;
    let mut settle_due: Option<Instant> = None;
    let (result, reason) = (|| {
        loop {
            if !current(worker, generation, attempt) {
                return (
                    observation.progress(EndReason::NextInjection),
                    Reason::NextInjection,
                );
            }
            let stop_reason = *stop.lock().unwrap();
            let now = Instant::now();
            let deadline = deadlines[checkpoint]
                .min(dirty_due.unwrap_or(end))
                .min(settle_due.unwrap_or(end));
            if stop_reason.is_none() && now < deadline {
                let events = worker
                    .events
                    .wait(generation, deadline.saturating_duration_since(now));
                if events & (FOCUS_CHANGED | DESTROYED) != 0 {
                    let result = observation.progress(EndReason::FocusLeft);
                    publish_result(
                        worker,
                        generation,
                        attempt,
                        baseline_at,
                        strategy,
                        evidence,
                        &result,
                        Some(if events & DESTROYED != 0 {
                            Reason::ElementDestroyed
                        } else {
                            Reason::FocusLeft
                        }),
                        None,
                    );
                    return (
                        result,
                        if events & DESTROYED != 0 {
                            Reason::ElementDestroyed
                        } else {
                            Reason::FocusLeft
                        },
                    );
                }
                if events & TEXT_CHANGED != 0 {
                    dirty_due
                        .get_or_insert(Instant::now().max(last_read + Duration::from_millis(350)));
                }
                continue;
            }
            if native_reads >= 90 {
                let result = observation.progress(EndReason::BudgetExceeded);
                publish_result(
                    worker,
                    generation,
                    attempt,
                    baseline_at,
                    strategy,
                    evidence,
                    &result,
                    Some(Reason::BudgetExceeded),
                    None,
                );
                return (result, Reason::BudgetExceeded);
            }
            native_reads += 1;
            let snapshot = match worker.read(generation, READ_TIMEOUT) {
                Ok(s) => s,
                Err(error) => {
                    let result = observation.progress(EndReason::ReadFailed);
                    let (_, reason, code) = error_details(error, true);
                    if current_error(worker, generation, attempt, error) {
                        // Native timeout invalidates its own generation; publish
                        // its cached result using that invalidation boundary.
                        let result_generation = if error == ReadError::TimedOut {
                            generation.wrapping_add(1)
                        } else {
                            generation
                        };
                        publish_result(
                            worker,
                            result_generation,
                            attempt,
                            baseline_at,
                            strategy,
                            evidence,
                            &result,
                            Some(reason),
                            code,
                        );
                    }
                    return (result, reason);
                }
            };
            if !current(worker, generation, attempt) {
                return (
                    observation.progress(EndReason::NextInjection),
                    Reason::NextInjection,
                );
            }
            last_read = snapshot.read_at;
            if stop_reason.is_some() || last_read >= end {
                let result = observation.close(
                    generation,
                    stop_reason.unwrap_or(EndReason::Timeout),
                    Some((&as_snapshot(&snapshot), last_read)),
                );
                publish_result(
                    worker,
                    generation,
                    attempt,
                    baseline_at,
                    strategy,
                    evidence,
                    &result,
                    None,
                    None,
                );
                let reason = result_reason(&result);
                return (result, reason);
            }
            if let Err(error) = observation.sample(generation, &as_snapshot(&snapshot), last_read) {
                let (status, reason) = match error {
                    SampleError::Range(super::range::RangeError::FieldCleared) => {
                        (ObservationStatus::Partial, Reason::FieldCleared)
                    }
                    SampleError::Range(super::range::RangeError::TargetChanged) => {
                        (ObservationStatus::Partial, Reason::TargetChanged)
                    }
                    SampleError::Range(super::range::RangeError::BudgetExceeded)
                    | SampleError::ReadBudgetExceeded => {
                        (ObservationStatus::Partial, Reason::BudgetExceeded)
                    }
                    _ => (ObservationStatus::Ambiguous, Reason::RangeAmbiguous),
                };
                let mut result = observation.progress(EndReason::ReadFailed);
                result.status = status;
                publish_result(
                    worker,
                    generation,
                    attempt,
                    baseline_at,
                    strategy,
                    evidence,
                    &result,
                    Some(reason),
                    None,
                );
                return (result, reason);
            }
            let result = observation.progress(EndReason::Timeout);
            diagnostics::publish(attempt, Status::Observing, Reason::None, None, result.edits);
            diagnostics::context(
                attempt,
                strategy,
                evidence,
                last_read.saturating_duration_since(baseline_at).as_millis() as u64,
                result.had_intermediate_changes,
            );
            dirty_due = None;
            settle_due = (observation.stable_at() > last_read).then(|| {
                observation
                    .stable_at()
                    .max(last_read + Duration::from_millis(350))
                    .min(end)
            });
            while checkpoint + 1 < deadlines.len() && deadlines[checkpoint] <= last_read {
                checkpoint += 1;
            }
        }
    })();
    if let Some(ticket) = capture_ticket {
        persist(
            ticket,
            &receipt,
            app,
            finished,
            &observation,
            &result,
            reason,
            strategy,
            evidence,
        );
    }
}

fn persist(
    ticket: super::records::CaptureTicket,
    receipt: &InjectionReceipt,
    app: &crate::injector::ResolvedApp,
    finished: Instant,
    observation: &Observation,
    result: &ObservationResult,
    reason: Reason,
    strategy: &str,
    evidence: &str,
) {
    if let Some(record) = record_for(
        receipt,
        app,
        finished,
        observation,
        result,
        reason,
        strategy,
        evidence,
    ) {
        if let Err(error) = super::records::save(ticket, &record) {
            log::error!("Could not save post-edit record: {error}");
        }
    }
}

fn record_for(
    receipt: &InjectionReceipt,
    app: &crate::injector::ResolvedApp,
    finished: Instant,
    observation: &Observation,
    result: &ObservationResult,
    reason: Reason,
    strategy: &str,
    evidence: &str,
) -> Option<super::records::Record> {
    use chrono::{Duration as WallDuration, SecondsFormat, Utc};
    let (original, edited) = observation.text_pair();
    if original == edited {
        return None;
    }
    let at = Utc::now() - WallDuration::from_std(finished.elapsed()).unwrap();
    let last = at
        + WallDuration::from_std(
            result
                .last_valid_at
                .unwrap_or(finished)
                .saturating_duration_since(finished),
        )
        .unwrap();
    Some(super::records::Record {
        id: receipt.injection_id.to_string(),
        record_id: receipt.record_id.to_string(),
        session_id: receipt.session_id.to_string(),
        timestamp: at.to_rfc3339_opts(SecondsFormat::Millis, true),
        last_observed_at: last.to_rfc3339_opts(SecondsFormat::Millis, true),
        app_name: app.app_name.clone(),
        original_text: original.into(),
        edited_text: edited.into(),
        complete: result.status == ObservationStatus::ObservedEdited,
        end_reason: serde_json::to_value(reason)
            .unwrap()
            .as_str()
            .unwrap()
            .into(),
        strategy: strategy.into(),
        range_evidence: evidence.into(),
        insertions: result.edits.map(|e| e.insertions),
        deletions: result.edits.map(|e| e.deletions),
        substitutions: result.edits.map(|e| e.substitutions),
        had_intermediate_changes: result.had_intermediate_changes,
    })
}

fn publish_result(
    worker: &ObserverWorker,
    generation: u64,
    attempt: u64,
    baseline_at: Instant,
    strategy: &'static str,
    evidence: &'static str,
    result: &ObservationResult,
    reason: Option<Reason>,
    code: Option<i32>,
) {
    if !current(worker, generation, attempt) {
        return;
    }
    let status = match result.status {
        ObservationStatus::ObservedUnchanged => Status::ObservedUnchanged,
        ObservationStatus::ObservedEdited => Status::ObservedEdited,
        ObservationStatus::Ambiguous => Status::Ambiguous,
        ObservationStatus::ReadFailed => Status::ReadFailed,
        ObservationStatus::Partial => Status::Partial,
    };
    let reason = reason.unwrap_or_else(|| result_reason(result));
    diagnostics::publish(attempt, status, reason, code, result.edits);
    diagnostics::context(
        attempt,
        strategy,
        evidence,
        result
            .last_valid_at
            .unwrap_or(baseline_at)
            .saturating_duration_since(baseline_at)
            .as_millis() as u64,
        result.had_intermediate_changes,
    );
}
fn result_reason(result: &ObservationResult) -> Reason {
    if result.status == ObservationStatus::Ambiguous {
        return Reason::RangeAmbiguous;
    }
    match result.reason {
        EndReason::Timeout => Reason::Timeout,
        EndReason::FocusLeft => Reason::FocusLeft,
        EndReason::NextRecording => Reason::NextRecording,
        EndReason::NextInjection => Reason::NextInjection,
        EndReason::FieldCleared => Reason::FieldCleared,
        EndReason::TargetChanged => Reason::TargetChanged,
        EndReason::BudgetExceeded => Reason::BudgetExceeded,
        _ => Reason::Interrupted,
    }
}

fn error_details(error: ReadError, has_baseline: bool) -> (Status, Reason, Option<i32>) {
    let partial = if has_baseline {
        Status::Partial
    } else {
        Status::Skipped
    };
    match error {
        ReadError::SecureInput => (partial, Reason::SecureInput, None),
        ReadError::SecurityUnknown => (partial, Reason::SecurityUnknown, None),
        ReadError::Unsupported => (partial, Reason::UnsupportedControl, None),
        ReadError::PermissionDenied => (partial, Reason::PermissionDenied, None),
        ReadError::TargetChanged => (
            if has_baseline {
                Status::Partial
            } else {
                Status::BaselineUnavailable
            },
            Reason::TargetChanged,
            None,
        ),
        ReadError::NoValue => (
            if has_baseline {
                Status::Partial
            } else {
                Status::BaselineUnavailable
            },
            Reason::NoValue,
            None,
        ),
        ReadError::TooLarge => (partial, Reason::TooLarge, None),
        ReadError::TimedOut | ReadError::WorkerBusy => (
            if has_baseline {
                Status::Partial
            } else {
                Status::ReadFailed
            },
            Reason::ReadTimeout,
            None,
        ),
        ReadError::Native(code) => (
            if has_baseline {
                Status::Partial
            } else {
                Status::ReadFailed
            },
            Reason::NativeError,
            Some(code),
        ),
        ReadError::StaleGeneration => (partial, Reason::Interrupted, None),
    }
}
pub(super) fn report_read_error(attempt: u64, error: ReadError, has_baseline: bool) {
    if error == ReadError::StaleGeneration {
        return;
    }
    let (status, reason, code) = error_details(error, has_baseline);
    diagnostics::publish(attempt, status, reason, code, None);
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Blocked {
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }
    impl super::super::platform::Reader for Blocked {
        fn capture(&mut self, _: u64, _: u32) -> Result<OwnedSnapshot, ReadError> {
            self.started.send(()).unwrap();
            self.release.recv().unwrap();
            Err(ReadError::NoValue)
        }
        fn read(&mut self, _: u64) -> Result<OwnedSnapshot, ReadError> {
            Err(ReadError::NoValue)
        }
        fn clear(&mut self) {}
    }
    #[test]
    fn record_contains_only_latest_injected_slice_and_keeps_partial_evidence() {
        let at = Instant::now();
        let before = Snapshot {
            target: 1,
            text: "前后",
            selection_utf16: 1..1,
        };
        let after = Snapshot {
            target: 1,
            text: "前返利后",
            selection_utf16: 3..3,
        };
        let tracker = RangeTracker::from_before_after(&before, &after, "返利").unwrap();
        let mut observation = Observation::new(1, tracker, at);
        let edited = Snapshot {
            target: 1,
            text: "前反力后",
            selection_utf16: 3..3,
        };
        observation
            .sample(1, &edited, at + Duration::from_secs(1))
            .unwrap();
        let app = crate::injector::ResolvedApp {
            process_id: 1,
            remote_bridge: false,
            app_name: Some("Fixture".into()),
        };
        let receipt = InjectionReceipt {
            session_id: uuid::Uuid::new_v4(),
            record_id: uuid::Uuid::new_v4(),
            injection_id: uuid::Uuid::new_v4(),
            injection_epoch: 1,
            outcome: DispatchOutcome::CommandsPosted,
            dispatch_started_at: Some(at),
            dispatch_finished_at: Some(at),
            resolved_app: Some(app.clone()),
        };
        let result = observation.progress(EndReason::NextInjection);
        let record = record_for(
            &receipt,
            &app,
            at,
            &observation,
            &result,
            Reason::NextInjection,
            "checkpoint_only",
            "before_after_verified",
        )
        .unwrap();
        assert_eq!(
            (record.original_text.as_str(), record.edited_text.as_str()),
            ("返利", "反力")
        );
        assert_eq!(record.record_id, receipt.record_id.to_string());
        assert_eq!(record.end_reason, "next_injection");
        assert!(!record.complete);
        assert!(record.last_observed_at > record.timestamp);
        // A later undo leaves no net pair to persist.
        observation
            .sample(1, &after, at + Duration::from_secs(2))
            .unwrap();
        let result = observation.progress(EndReason::Timeout);
        assert!(record_for(
            &receipt,
            &app,
            at,
            &observation,
            &result,
            Reason::Timeout,
            "checkpoint_only",
            "before_after_verified"
        )
        .is_none());
    }
    #[test]
    fn pre_read_timeout_before_receipt_handoff_reports_failure_instead_of_staying_verifying() {
        let _guard = diagnostics::TEST_LOCK.lock().unwrap();
        diagnostics::set_enabled(true);
        let attempt = begin_attempt();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = Arc::new(
            ObserverWorker::start_with(move || Blocked {
                started: started_tx,
                release: release_rx,
            })
            .unwrap(),
        );
        let native = Arc::clone(&worker);
        let call = std::thread::spawn(move || native.capture(0, 1, Duration::from_millis(50)));
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(call.join().unwrap(), Err(ReadError::TimedOut)));
        let (tx, rx) = mpsc::channel();
        tx.send(Err(ReadError::TimedOut)).unwrap();
        let now = Instant::now();
        run(
            &worker,
            0,
            attempt,
            rx,
            Arc::new(Mutex::new(None)),
            InjectionReceipt {
                session_id: uuid::Uuid::new_v4(),
                record_id: uuid::Uuid::new_v4(),
                injection_id: uuid::Uuid::new_v4(),
                injection_epoch: 1,
                outcome: DispatchOutcome::CommandsPosted,
                dispatch_started_at: Some(now),
                dispatch_finished_at: Some(now),
                resolved_app: Some(crate::injector::ResolvedApp {
                    process_id: 1,
                    remote_bridge: false,
                    app_name: Some("Fixture".into()),
                }),
            },
            "synthetic".into(),
            [
                Duration::from_secs(10),
                Duration::from_secs(30),
                Duration::from_secs(60),
            ],
            None,
        );
        let status = diagnostics::snapshot();
        release_tx.send(()).unwrap();
        diagnostics::set_enabled(false);
        assert_eq!(
            (status.status, status.reason),
            (Status::ReadFailed, Reason::ReadTimeout)
        );
    }
}
