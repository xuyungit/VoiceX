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
/// Explicit session cancellation discards in-flight results and text pairs.
pub fn cancel() {
    if let Some(c) = existing() {
        c.invalidate(EndReason::SessionCancelled);
    }
    ATTEMPT.fetch_add(1, Ordering::SeqCst);
    diagnostics::boundary(Reason::SessionCancelled);
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
            c.invalidate(EndReason::NextInjection);
            ATTEMPT.fetch_add(1, Ordering::SeqCst);
            diagnostics::boundary(Reason::NextInjection);
        }
    }
}
pub fn begin_attempt() -> u64 {
    if let Some(c) = existing() {
        c.invalidate(EndReason::NextInjection);
    }
    let attempt = ATTEMPT.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    diagnostics::publish(attempt, Status::Verifying, Reason::None, None, None);
    attempt
}
pub(super) fn attempt_is_current(attempt: u64) -> bool {
    ATTEMPT.load(Ordering::SeqCst) == attempt
}
pub fn skipped(attempt: u64, reason: Reason) {
    diagnostics::publish(attempt, Status::Skipped, reason, None, None);
}

pub fn finish_dispatch(
    attempt: u64,
    receipt: InjectionReceipt,
    injected: String,
    pending: Option<Pending>,
) {
    if receipt.outcome != DispatchOutcome::CommandsPosted {
        skipped(attempt, Reason::DispatchFailed);
        return;
    }
    if let Some(pending) = pending {
        pending.dispatched(receipt, injected);
    }
}
impl Coordinator {
    fn invalidate(&self, reason: EndReason) {
        let mut active = self.active.lock().unwrap();
        if let Some((_, stop)) = active.take() {
            *stop.lock().unwrap() = Some(reason);
        }
        self.worker.invalidate();
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
        let _ = self.dispatched_with_schedule(
            receipt,
            injected,
            [
                Duration::from_secs(10),
                Duration::from_secs(30),
                Duration::from_secs(60),
            ],
        );
    }
    pub(super) fn dispatched_with_schedule(
        mut self,
        receipt: InjectionReceipt,
        injected: String,
        schedule: [Duration; 3],
    ) -> Option<std::thread::JoinHandle<Option<super::records::Record>>> {
        let before = self.before.take()?;
        let c = Arc::clone(&self.coordinator);
        let (generation, attempt, stop) = (self.generation, self.attempt, Arc::clone(&self.stop));
        let capture_ticket = self.capture_ticket;
        match std::thread::Builder::new()
            .name("post-edit-session".into())
            .spawn(move || {
                let record = run(
                    &c.worker,
                    generation,
                    attempt,
                    before,
                    stop,
                    receipt,
                    injected,
                    schedule,
                    &SystemClock,
                );
                if let (Some(ticket), Some(record)) = (capture_ticket, record.as_ref()) {
                    if let Err(error) = super::records::save(ticket, record) {
                        log::error!("Could not save post-edit record: {error}");
                    }
                }
                c.finished(generation);
                record
            }) {
            Ok(handle) => {
                self.handed_off = true;
                Some(handle)
            }
            Err(error) => {
                log::warn!("Post-edit session could not start: {error}");
                diagnostics::publish(attempt, Status::ReadFailed, Reason::NativeError, None, None);
                None
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
/// The caller never infers a timeout boundary by doing generation arithmetic.
fn error_generation(worker: &ObserverWorker, generation: u64, error: ReadError) -> Option<u64> {
    let boundary = match error {
        ReadError::TimedOut {
            invalidated_generation,
        } => invalidated_generation,
        _ => generation,
    };
    worker.is_current(boundary).then_some(boundary)
}

trait SessionClock {
    fn now(&self) -> Instant;
    fn wait(&self, events: &super::platform::EventSignal, generation: u64, timeout: Duration)
        -> u8;
}
struct SystemClock;
impl SessionClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn wait(
        &self,
        events: &super::platform::EventSignal,
        generation: u64,
        timeout: Duration,
    ) -> u8 {
        events.wait(generation, timeout)
    }
}

struct Baseline {
    observation: Observation,
    at: Instant,
    strategy: &'static str,
    evidence: &'static str,
    native_reads: usize,
}
struct BaselineFailure {
    status: Status,
    reason: Reason,
    code: Option<i32>,
    generation: u64,
}
impl BaselineFailure {
    fn new(generation: u64, reason: Reason) -> Self {
        Self {
            status: Status::BaselineUnavailable,
            reason,
            code: None,
            generation,
        }
    }
    fn read(worker: &ObserverWorker, generation: u64, error: ReadError) -> Self {
        let (status, reason, code) = error_details(error, false);
        Self {
            status,
            reason,
            code,
            generation: error_generation(worker, generation, error).unwrap_or(generation),
        }
    }
}

fn wait_until(
    worker: &ObserverWorker,
    generation: u64,
    stop: &Stop,
    deadline: Instant,
    clock: &impl SessionClock,
) -> bool {
    while clock.now() < deadline && worker.is_current(generation) {
        if stop.lock().unwrap().is_some() {
            return false;
        }
        if clock.wait(
            &worker.events,
            generation,
            deadline.saturating_duration_since(clock.now()),
        ) & DESTROYED
            != 0
        {
            return false;
        }
    }
    worker.is_current(generation) && stop.lock().unwrap().is_none()
}

fn establish_baseline(
    worker: &ObserverWorker,
    generation: u64,
    attempt: u64,
    before: mpsc::Receiver<Result<OwnedSnapshot, ReadError>>,
    stop: &Stop,
    receipt: &InjectionReceipt,
    injected: &str,
    clock: &impl SessionClock,
) -> Result<Baseline, BaselineFailure> {
    let (Some(started), Some(finished), Some(app)) = (
        receipt.dispatch_started_at,
        receipt.dispatch_finished_at,
        receipt.resolved_app.as_ref(),
    ) else {
        return Err(BaselineFailure::new(generation, Reason::TargetUnavailable));
    };
    let pre = match before.recv_timeout(READ_TIMEOUT) {
        Ok(Ok(snapshot)) => Some(snapshot),
        Ok(Err(
            ReadError::NoValue
            | ReadError::CompositionActive
            | ReadError::TooLarge
            | ReadError::WindowChanged,
        )) => None,
        Ok(Err(error)) => return Err(BaselineFailure::read(worker, generation, error)),
        Err(_) => {
            return Err(BaselineFailure {
                status: Status::ReadFailed,
                reason: Reason::ReadTimeout,
                code: None,
                generation,
            })
        }
    };
    if pre
        .as_ref()
        .is_some_and(|pre| pre.process_id != app.process_id)
    {
        return Err(BaselineFailure::new(generation, Reason::TargetChanged));
    }
    let verified_pre = pre.as_ref().is_some_and(|pre| pre.read_at <= started);
    let mut native_reads = 1;
    for delay in [120, 300, 700] {
        if !wait_until(
            worker,
            generation,
            stop,
            finished + Duration::from_millis(delay),
            clock,
        ) {
            return Err(BaselineFailure::new(generation, Reason::Interrupted));
        }
        native_reads += 1;
        let read = if pre.is_some() {
            worker.read(generation, READ_TIMEOUT)
        } else {
            worker.capture_for_insertion(
                generation,
                app.process_id,
                injected.encode_utf16().count(),
                READ_TIMEOUT,
            )
        };
        let after =
            match read {
                Ok(snapshot) => snapshot,
                Err(
                    ReadError::NoValue | ReadError::CompositionActive | ReadError::WindowChanged,
                ) if delay < 700 => continue,
                Err(error) => return Err(BaselineFailure::read(worker, generation, error)),
            };
        if !current(worker, generation, attempt) {
            return Err(BaselineFailure::new(generation, Reason::Interrupted));
        }
        if after.composition == super::platform::CompositionState::Active {
            continue;
        }
        let tracker = if verified_pre {
            RangeTracker::from_before_after(
                &as_snapshot(pre.as_ref().unwrap()),
                &as_snapshot(&after),
                injected,
            )
        } else {
            RangeTracker::from_post_anchored(&as_snapshot(&after), injected)
        };
        if let Ok(tracker) = tracker {
            let evidence = match tracker.evidence {
                super::range::RangeEvidence::BeforeAfterVerified => "before_after_verified",
                _ => "post_anchored",
            };
            return Ok(Baseline {
                observation: Observation::new(generation, tracker, after.read_at),
                at: after.read_at,
                strategy: if after.event_driven {
                    "event_with_polling"
                } else {
                    "bounded_polling"
                },
                evidence,
                native_reads,
            });
        }
    }
    Err(BaselineFailure::new(
        generation,
        Reason::BaselineTransformed,
    ))
}

struct Completion {
    result: ObservationResult,
    reason: Reason,
    code: Option<i32>,
    generation: u64,
}

/// Successful subscriptions do not prove that a provider delivers events.
/// Every control receives independent rereads, with a faster initial window.
fn poll_interval(elapsed: Duration) -> Duration {
    if elapsed < Duration::from_secs(10) {
        Duration::from_secs(1)
    } else {
        Duration::from_secs(2)
    }
}
impl Completion {
    fn cached(observation: &Observation, generation: u64, end: EndReason) -> Self {
        let result = observation.progress(end);
        let reason = result_reason(&result);
        Self {
            result,
            reason,
            code: None,
            generation,
        }
    }
}

fn observe(
    worker: &ObserverWorker,
    generation: u64,
    attempt: u64,
    stop: &Stop,
    baseline: &mut Baseline,
    schedule: [Duration; 3],
    clock: &impl SessionClock,
) -> Completion {
    let observation = &mut baseline.observation;
    let end = baseline.at + schedule[2];
    let mut deadlines = vec![
        baseline.at + schedule[0],
        baseline.at + schedule[1],
        end.checked_sub(Duration::from_millis(800) + READ_TIMEOUT)
            .unwrap_or(baseline.at),
        end,
    ];
    deadlines.sort();
    deadlines.dedup();
    let mut checkpoint = 0;
    let mut last_read = baseline.at;
    let mut poll_due = last_read + poll_interval(Duration::ZERO);
    let mut dirty_due: Option<Instant> = None;
    let mut settle_due: Option<Instant> = None;
    loop {
        let stop_reason = *stop.lock().unwrap();
        if !current(worker, generation, attempt) {
            return Completion::cached(
                observation,
                generation,
                stop_reason.unwrap_or(EndReason::Interrupted),
            );
        }
        if matches!(
            stop_reason,
            Some(
                EndReason::SessionCancelled
                    | EndReason::NextInjection
                    | EndReason::SettingsDisabled
                    | EndReason::DataCleared
            )
        ) {
            return Completion::cached(observation, generation, stop_reason.unwrap());
        }
        let now = clock.now();
        let deadline = deadlines[checkpoint]
            .min(poll_due)
            .min(dirty_due.unwrap_or(end))
            .min(settle_due.unwrap_or(end));
        if stop_reason.is_none() && now < deadline {
            let events = clock.wait(
                &worker.events,
                generation,
                deadline.saturating_duration_since(now),
            );
            if events & TEXT_CHANGED != 0 {
                observation.text_changed();
                dirty_due.get_or_insert(clock.now().max(last_read + Duration::from_millis(350)));
            }
            if events & DESTROYED != 0 {
                let mut done = Completion::cached(observation, generation, EndReason::FocusLeft);
                done.reason = Reason::ElementDestroyed;
                return done;
            }
            if events & FOCUS_CHANGED != 0 {
                // A leaf focus change can stay inside the same editor. The
                // native reader verifies normalized focus before reading text.
                dirty_due.get_or_insert(clock.now().max(last_read + Duration::from_millis(350)));
            }
            continue;
        }
        if baseline.native_reads >= 90 {
            return Completion::cached(observation, generation, EndReason::BudgetExceeded);
        }
        baseline.native_reads += 1;
        let read = worker.read(generation, READ_TIMEOUT);
        let stop_reason = *stop.lock().unwrap();
        if matches!(
            stop_reason,
            Some(
                EndReason::SessionCancelled
                    | EndReason::NextInjection
                    | EndReason::SettingsDisabled
                    | EndReason::DataCleared
            )
        ) {
            return Completion::cached(observation, generation, stop_reason.unwrap());
        }
        let snapshot = match read {
            Ok(snapshot) => snapshot,
            Err(error @ (ReadError::CompositionActive | ReadError::WindowChanged)) => {
                observation.unresolved_input();
                if stop_reason.is_some() || clock.now() >= end {
                    let mut done = Completion::cached(
                        observation,
                        generation,
                        stop_reason.unwrap_or(EndReason::Timeout),
                    );
                    done.reason = if error == ReadError::WindowChanged {
                        done.result.status = ObservationStatus::Ambiguous;
                        Reason::RangeAmbiguous
                    } else {
                        Reason::CompositionActive
                    };
                    return done;
                }
                dirty_due = Some(clock.now() + Duration::from_millis(350));
                poll_due =
                    clock.now() + poll_interval(clock.now().saturating_duration_since(baseline.at));
                settle_due = None;
                while checkpoint + 1 < deadlines.len() && deadlines[checkpoint] <= clock.now() {
                    checkpoint += 1;
                }
                continue;
            }
            Err(error) => {
                let mut done = Completion::cached(observation, generation, EndReason::ReadFailed);
                let (_, reason, code) = error_details(error, true);
                done.reason = reason;
                done.code = code;
                done.generation = error_generation(worker, generation, error).unwrap_or(generation);
                return done;
            }
        };
        if !current(worker, generation, attempt) {
            return Completion::cached(
                observation,
                generation,
                stop_reason.unwrap_or(EndReason::Interrupted),
            );
        }
        last_read = snapshot.read_at;
        poll_due = clock.now().max(last_read)
            + poll_interval(last_read.saturating_duration_since(baseline.at));
        let terminal = stop_reason.is_some() || last_read >= end || clock.now() >= end;
        let sample = observation.sample_read(generation, &snapshot);
        if let Err(error) = sample {
            let (reason, retry) = match error {
                SampleError::CompositionActive => (Reason::CompositionActive, true),
                SampleError::Range(
                    super::range::RangeError::Ambiguous
                    | super::range::RangeError::InvalidSelection,
                ) => (Reason::RangeAmbiguous, true),
                SampleError::Range(super::range::RangeError::FieldCleared) => {
                    (Reason::FieldCleared, false)
                }
                SampleError::Range(super::range::RangeError::TargetChanged) => {
                    (Reason::TargetChanged, false)
                }
                SampleError::Range(super::range::RangeError::BudgetExceeded)
                | SampleError::ReadBudgetExceeded => (Reason::BudgetExceeded, false),
                _ => (Reason::Interrupted, false),
            };
            if retry && !terminal {
                dirty_due = Some(clock.now().max(last_read) + Duration::from_millis(350));
                settle_due = None;
                while checkpoint + 1 < deadlines.len() && deadlines[checkpoint] <= last_read {
                    checkpoint += 1;
                }
                continue;
            }
            let end_reason = match reason {
                Reason::FieldCleared => EndReason::FieldCleared,
                Reason::TargetChanged => EndReason::TargetChanged,
                Reason::BudgetExceeded => EndReason::BudgetExceeded,
                _ => EndReason::ReadFailed,
            };
            let mut done = Completion::cached(observation, generation, end_reason);
            done.reason = reason;
            if retry {
                done.result.status = ObservationStatus::Ambiguous;
            }
            return done;
        }
        if terminal {
            let result = observation.finish(stop_reason.unwrap_or(EndReason::Timeout));
            let reason = result_reason(&result);
            return Completion {
                result,
                reason,
                code: None,
                generation,
            };
        }
        let result = observation.progress(EndReason::Timeout);
        diagnostics::publish(attempt, Status::Observing, Reason::None, None, result.edits);
        diagnostics::context(
            attempt,
            baseline.strategy,
            baseline.evidence,
            last_read.saturating_duration_since(baseline.at).as_millis() as u64,
            result.had_intermediate_changes,
        );
        dirty_due = None;
        settle_due = (!observation.is_stable()).then(|| {
            observation
                .stable_at()
                .max(last_read + Duration::from_millis(350))
                .min(end)
        });
        while checkpoint + 1 < deadlines.len() && deadlines[checkpoint] <= last_read {
            checkpoint += 1;
        }
    }
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
    clock: &impl SessionClock,
) -> Option<super::records::Record> {
    // A pre-read's own timeout must still be reported after its generation changed.
    if !diagnostics::enabled() || !attempt_is_current(attempt) {
        return None;
    }
    if receipt.outcome != DispatchOutcome::CommandsPosted {
        skipped(attempt, Reason::DispatchFailed);
        return None;
    }
    let Some(app) = receipt.resolved_app.as_ref() else {
        skipped(attempt, Reason::TargetUnavailable);
        return None;
    };
    if app.remote_bridge {
        skipped(attempt, Reason::RemoteTargetUnsupported);
        return None;
    }
    let mut baseline = match establish_baseline(
        worker, generation, attempt, before, &stop, &receipt, &injected, clock,
    ) {
        Ok(baseline) => baseline,
        Err(failure) => {
            log::info!(
                "Post-edit baseline unavailable: injection_id={}, pid={}, status={:?}, reason={:?}, native_code={:?}",
                receipt.injection_id, app.process_id, failure.status, failure.reason, failure.code
            );
            if worker.is_current(failure.generation) {
                diagnostics::publish(attempt, failure.status, failure.reason, failure.code, None);
            }
            return None;
        }
    };
    drop(injected);
    diagnostics::publish(attempt, Status::Observing, Reason::None, None, None);
    diagnostics::context(attempt, baseline.strategy, baseline.evidence, 0, false);
    let done = observe(
        worker,
        generation,
        attempt,
        &stop,
        &mut baseline,
        schedule,
        clock,
    );
    log::info!(
        "Post-edit observation ended: injection_id={}, pid={}, status={:?}, reason={:?}, native_code={:?}, reads={}, confirmed={}",
        receipt.injection_id, app.process_id, done.result.status, done.reason, done.code,
        baseline.native_reads, done.result.confirmed.is_some()
    );
    publish_result(
        worker,
        done.generation,
        attempt,
        baseline.at,
        baseline.strategy,
        baseline.evidence,
        &done.result,
        Some(done.reason),
        done.code,
    );
    record_for(
        &receipt,
        app,
        receipt.dispatch_finished_at.unwrap(),
        &baseline.observation,
        &done.result,
        done.reason,
        baseline.strategy,
        baseline.evidence,
    )
}

pub(super) fn record_for(
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
    let original = observation.original_text();
    let confirmed = result.confirmed.as_ref()?;
    let edited = confirmed.text.as_str();
    if original == edited
        || matches!(
            result.reason,
            EndReason::SessionCancelled
                | EndReason::Interrupted
                | EndReason::SettingsDisabled
                | EndReason::DataCleared
        )
    {
        return None;
    }
    let at = Utc::now() - WallDuration::from_std(finished.elapsed()).unwrap();
    let last =
        at + WallDuration::from_std(confirmed.read_at.saturating_duration_since(finished)).unwrap();
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
        stable: true,
        composition: confirmed.composition,
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
        EndReason::SessionCancelled => Reason::SessionCancelled,
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
        ReadError::WindowChanged => (Status::Ambiguous, Reason::RangeAmbiguous, None),
        ReadError::CompositionActive => (partial, Reason::CompositionActive, None),
        ReadError::TimedOut { .. } | ReadError::WorkerBusy => (
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
#[cfg(test)]
pub(super) fn report_read_error(attempt: u64, error: ReadError, has_baseline: bool) {
    if error == ReadError::StaleGeneration {
        return;
    }
    let (status, reason, code) = error_details(error, has_baseline);
    diagnostics::publish(attempt, status, reason, code, None);
}

#[cfg(test)]
mod tests {
    use super::super::platform::{CompositionState, EventSignal, Reader};
    use super::*;
    use std::collections::VecDeque;

    #[derive(Clone)]
    struct VirtualClock(Arc<Mutex<Instant>>);
    impl SessionClock for VirtualClock {
        fn now(&self) -> Instant {
            *self.0.lock().unwrap()
        }
        fn wait(&self, events: &EventSignal, generation: u64, timeout: Duration) -> u8 {
            let flags = events.wait(generation, Duration::ZERO);
            if flags == 0 {
                *self.0.lock().unwrap() += timeout;
            }
            flags
        }
    }
    #[derive(Clone, Copy)]
    enum Step {
        Text(&'static str),
        TextThenNotify(&'static str),
        TextThenFocus(&'static str),
        Composing,
        WindowChanged,
        Cancel,
        Reinject,
    }
    struct ScriptedReader {
        clock: VirtualClock,
        steps: VecDeque<Step>,
        stop: Stop,
        events: Option<Arc<EventSignal>>,
        last: Option<Step>,
    }
    fn snapshot(text: &str, at: Instant) -> OwnedSnapshot {
        OwnedSnapshot {
            target: 1,
            process_id: 1,
            text: text.into(),
            selection_utf16: text.encode_utf16().count()..text.encode_utf16().count(),
            read_at: at,
            event_driven: false,
            composition: CompositionState::Inactive,
        }
    }
    impl Reader for ScriptedReader {
        fn capture(&mut self, _: u64, _: u32, _: usize) -> Result<OwnedSnapshot, ReadError> {
            self.read(0)
        }
        fn read(&mut self, generation: u64) -> Result<OwnedSnapshot, ReadError> {
            let step = self
                .steps
                .pop_front()
                .or(self.last)
                .expect("initial field state");
            self.last = Some(match step {
                Step::TextThenNotify(text) | Step::TextThenFocus(text) => Step::Text(text),
                other => other,
            });
            match step {
                Step::Text(text) => Ok(snapshot(text, self.clock.now())),
                Step::TextThenNotify(text) => {
                    self.events
                        .as_ref()
                        .unwrap()
                        .notify(generation, TEXT_CHANGED);
                    Ok(snapshot(text, self.clock.now()))
                }
                Step::TextThenFocus(text) => {
                    self.events
                        .as_ref()
                        .unwrap()
                        .notify(generation, FOCUS_CHANGED);
                    Ok(snapshot(text, self.clock.now()))
                }
                Step::Composing => Err(ReadError::CompositionActive),
                Step::WindowChanged => Err(ReadError::WindowChanged),
                Step::Cancel => {
                    *self.stop.lock().unwrap() = Some(EndReason::SessionCancelled);
                    cancel();
                    Err(ReadError::StaleGeneration)
                }
                Step::Reinject => {
                    *self.stop.lock().unwrap() = Some(EndReason::NextInjection);
                    ATTEMPT.fetch_add(1, Ordering::SeqCst);
                    Ok(snapshot("来自新听写的内容", self.clock.now()))
                }
            }
        }
        fn clear(&mut self) {}
        fn set_events(&mut self, events: Arc<EventSignal>) {
            self.events = Some(events);
        }
    }
    fn receipt(at: Instant) -> InjectionReceipt {
        InjectionReceipt {
            session_id: uuid::Uuid::new_v4(),
            record_id: uuid::Uuid::new_v4(),
            injection_id: uuid::Uuid::new_v4(),
            injection_epoch: 1,
            outcome: DispatchOutcome::CommandsPosted,
            dispatch_started_at: Some(at),
            dispatch_finished_at: Some(at),
            resolved_app: Some(crate::injector::ResolvedApp {
                process_id: 1,
                remote_bridge: false,
                app_name: Some("Fixture".into()),
            }),
        }
    }
    fn scripted_run(steps: Vec<Step>) -> Option<super::super::records::Record> {
        let clock = VirtualClock(Arc::new(Mutex::new(Instant::now())));
        let start = clock.now();
        let stop = Arc::new(Mutex::new(None));
        let reader = ScriptedReader {
            clock: clock.clone(),
            steps: steps.into(),
            stop: Arc::clone(&stop),
            events: None,
            last: None,
        };
        let worker = ObserverWorker::start_with(move || reader).unwrap();
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(snapshot("", start))).unwrap();
        let attempt = begin_attempt();
        run(
            &worker,
            0,
            attempt,
            rx,
            stop,
            receipt(start),
            "支座返力".into(),
            [
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
            ],
            &clock,
        )
    }

    struct SilentReader {
        clock: VirtualClock,
        start: Instant,
        advertises_events: bool,
        clear_at: Option<Duration>,
        reads: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Reader for SilentReader {
        fn capture(&mut self, _: u64, _: u32, _: usize) -> Result<OwnedSnapshot, ReadError> {
            self.read(0)
        }
        fn read(&mut self, _: u64) -> Result<OwnedSnapshot, ReadError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let elapsed = self.clock.now().duration_since(self.start);
            let text = if self.clear_at.is_some_and(|at| elapsed >= at) {
                ""
            } else if elapsed >= Duration::from_secs(2) {
                "支座反力"
            } else {
                "支座返力"
            };
            let mut sample = snapshot(text, self.clock.now());
            sample.event_driven = self.advertises_events;
            Ok(sample)
        }
        fn clear(&mut self) {}
    }
    #[test]
    fn independent_polling_catches_silent_edits_and_send_with_a_bounded_read_count() {
        let _guard = diagnostics::TEST_LOCK.lock().unwrap();
        diagnostics::set_enabled(true);
        for advertises_events in [false, true] {
            for clear_at in [None, Some(Duration::from_secs(5))] {
                let clock = VirtualClock(Arc::new(Mutex::new(Instant::now())));
                let start = clock.now();
                let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let reader = SilentReader {
                    clock: clock.clone(),
                    start,
                    advertises_events,
                    clear_at,
                    reads: Arc::clone(&reads),
                };
                let worker = ObserverWorker::start_with(move || reader).unwrap();
                let (tx, rx) = mpsc::channel();
                tx.send(Ok(snapshot("", start))).unwrap();
                let attempt = begin_attempt();
                let record = run(
                    &worker,
                    0,
                    attempt,
                    rx,
                    Arc::new(Mutex::new(None)),
                    receipt(start),
                    "支座返力".into(),
                    [
                        Duration::from_secs(10),
                        Duration::from_secs(30),
                        Duration::from_secs(60),
                    ],
                    &clock,
                )
                .unwrap();
                assert_eq!(record.edited_text, "支座反力");
                assert_eq!(record.complete, clear_at.is_none());
                assert_eq!(
                    record.end_reason,
                    if clear_at.is_some() {
                        "field_cleared"
                    } else {
                        "timeout"
                    }
                );
                assert!(record.stable);
                assert!(reads.load(Ordering::SeqCst) <= 45);
                if clear_at.is_some() {
                    assert!(clock.now().duration_since(start) < Duration::from_secs(7));
                }
            }
        }
        diagnostics::set_enabled(false);
    }
    #[test]
    fn descendant_focus_changes_are_verified_instead_of_ending_the_same_editor() {
        let _guard = diagnostics::TEST_LOCK.lock().unwrap();
        diagnostics::set_enabled(true);
        for first in [Step::Text("支座返力"), Step::TextThenFocus("支座返力")] {
            let record = scripted_run(vec![
                first,
                Step::TextThenFocus("支座反力"),
                Step::Text("支座反力"),
                Step::Text("支座反力"),
            ])
            .unwrap();
            assert_eq!(record.edited_text, "支座反力");
            assert!(record.stable && record.complete);
        }
        diagnostics::set_enabled(false);
    }

    #[test]
    fn stable_correction_survives_clear_with_or_without_a_text_change_notification() {
        let _guard = diagnostics::TEST_LOCK.lock().unwrap();
        diagnostics::set_enabled(true);
        for confirmation in [Step::Text("支座反力"), Step::TextThenNotify("支座反力")] {
            let record = scripted_run(vec![
                Step::Text("支座返力"),
                Step::Text("支座反力"),
                confirmation,
                Step::Text(""),
            ])
            .expect("sending must retain the previously confirmed correction");
            assert_eq!(record.original_text, "支座返力");
            assert_eq!(record.edited_text, "支座反力");
            assert_eq!(record.substitutions, Some(1));
            assert_eq!(record.end_reason, "field_cleared");
            assert!(record.stable && !record.complete);
        }
        diagnostics::set_enabled(false);
    }
    #[test]
    fn clear_cannot_resurrect_a_confirmation_superseded_by_newer_input() {
        let _guard = diagnostics::TEST_LOCK.lock().unwrap();
        diagnostics::set_enabled(true);
        for newer in [
            Step::Text("支座反"),
            Step::Text("支座返fan"),
            Step::Composing,
            Step::WindowChanged,
        ] {
            assert!(scripted_run(vec![
                Step::Text("支座返力"),
                Step::Text("支座反力"),
                Step::TextThenNotify("支座反力"),
                newer,
                Step::Text(""),
            ])
            .is_none());
        }
        diagnostics::set_enabled(false);
    }
    #[test]
    fn persistent_ambiguity_ends_without_saving_an_old_stable_correction() {
        let _guard = diagnostics::TEST_LOCK.lock().unwrap();
        diagnostics::set_enabled(true);
        for unresolved in [Step::Text("支座反力继续写"), Step::WindowChanged] {
            let mut steps = vec![
                Step::Text("支座返力"),
                Step::Text("支座反力"),
                Step::TextThenNotify("支座反力"),
            ];
            steps.extend((0..8).map(|_| unresolved));
            assert!(scripted_run(steps).is_none());
            let status = diagnostics::snapshot();
            assert_eq!(status.status, Status::Ambiguous);
            assert_eq!(status.reason, Reason::RangeAmbiguous);
        }
        diagnostics::set_enabled(false);
    }
    #[test]
    fn full_session_recovers_after_backspace_ambiguity_and_composition() {
        let _guard = diagnostics::TEST_LOCK.lock().unwrap();
        diagnostics::set_enabled(true);
        for middle in [
            Step::Composing,
            Step::WindowChanged,
            Step::Text("支座返fan"),
        ] {
            let record = scripted_run(vec![
                Step::Text("支座返力"),
                Step::Text("支座返"),
                middle,
                Step::Text("支座反力"),
                Step::Text("支座反力"),
                Step::Text("支座反力"),
                Step::Text("支座反力"),
            ])
            .unwrap();
            assert_eq!(
                (record.original_text.as_str(), record.edited_text.as_str()),
                ("支座返力", "支座反力")
            );
            assert!(record.stable && record.complete);
            assert_eq!(record.end_reason, "timeout");
        }
        diagnostics::set_enabled(false);
    }
    #[test]
    fn full_session_never_saves_an_unconfirmed_backspace_or_unresolved_composition() {
        let _guard = diagnostics::TEST_LOCK.lock().unwrap();
        diagnostics::set_enabled(true);
        assert!(scripted_run(vec![
            Step::Text("支座返力"),
            Step::Text("支座返"),
            Step::Text("")
        ])
        .is_none());
        assert!(scripted_run(vec![
            Step::Text("支座返力"),
            Step::Text("支座返"),
            Step::Text("支座返"),
            Step::Composing,
            Step::Composing,
            Step::Composing,
            Step::Composing,
            Step::Composing,
            Step::Composing,
            Step::Composing,
            Step::Composing
        ])
        .is_none());
        diagnostics::set_enabled(false);
    }
    #[test]
    fn full_session_recovers_even_if_the_deleted_state_was_previously_stable() {
        let _guard = diagnostics::TEST_LOCK.lock().unwrap();
        diagnostics::set_enabled(true);
        let record = scripted_run(vec![
            Step::Text("支座返力"),
            Step::Text("支座返"),
            Step::Text("支座返"),
            Step::Composing,
            Step::Text("支座反力"),
            Step::Text("支座反力"),
            Step::Text("支座反力"),
            Step::Text("支座反力"),
        ])
        .unwrap();
        assert_eq!(record.edited_text, "支座反力");
        assert!(record.stable && record.complete);
        diagnostics::set_enabled(false);
    }
    #[test]
    fn full_session_discards_cancelled_text_and_reinjection_cannot_enter_the_old_record() {
        let _guard = diagnostics::TEST_LOCK.lock().unwrap();
        diagnostics::set_enabled(true);
        assert!(scripted_run(vec![
            Step::Text("支座返力"),
            Step::Text("支座反力"),
            Step::Text("支座反力"),
            Step::Cancel
        ])
        .is_none());
        assert_eq!(diagnostics::snapshot().reason, Reason::SessionCancelled);
        let record = scripted_run(vec![
            Step::Text("支座返力"),
            Step::Text("支座反力"),
            Step::Text("支座反力"),
            Step::Reinject,
        ])
        .unwrap();
        assert_eq!(record.edited_text, "支座反力");
        assert_eq!(record.end_reason, "next_injection");
        assert!(record.stable && !record.complete);
        diagnostics::set_enabled(false);
    }

    #[test]
    fn failure_before_resolving_a_target_cannot_leave_the_status_verifying() {
        let _guard = diagnostics::TEST_LOCK.lock().unwrap();
        diagnostics::set_enabled(true);
        let attempt = begin_attempt();
        let mut failed = receipt(Instant::now());
        failed.outcome = DispatchOutcome::Failed {
            code: "modifiers_held",
        };
        failed.resolved_app = None;
        failed.dispatch_started_at = None;
        failed.dispatch_finished_at = None;
        finish_dispatch(attempt, failed, "未派发".into(), None);
        assert_eq!(
            (
                diagnostics::snapshot().status,
                diagnostics::snapshot().reason
            ),
            (Status::Skipped, Reason::DispatchFailed)
        );
        diagnostics::set_enabled(false);
    }
    struct Blocked {
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }
    impl super::super::platform::Reader for Blocked {
        fn capture(&mut self, _: u64, _: u32, _: usize) -> Result<OwnedSnapshot, ReadError> {
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
        observation
            .sample(1, &edited, at + Duration::from_secs(2))
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
            .sample(1, &after, at + Duration::from_secs(3))
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
        assert!(matches!(
            call.join().unwrap(),
            Err(ReadError::TimedOut { .. })
        ));
        let (tx, rx) = mpsc::channel();
        tx.send(Err(ReadError::TimedOut {
            invalidated_generation: 1,
        }))
        .unwrap();
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
            &SystemClock,
        );
        let status = diagnostics::snapshot();
        assert_eq!(
            error_generation(
                &worker,
                0,
                ReadError::TimedOut {
                    invalidated_generation: 1
                }
            ),
            Some(1)
        );
        worker.invalidate();
        assert_eq!(
            error_generation(
                &worker,
                0,
                ReadError::TimedOut {
                    invalidated_generation: 1
                }
            ),
            None
        );
        release_tx.send(()).unwrap();
        diagnostics::set_enabled(false);
        assert_eq!(
            (status.status, status.reason),
            (Status::ReadFailed, Reason::ReadTimeout)
        );
    }
}
