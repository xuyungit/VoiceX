//! Dedicated native observation worker. The worker owns every native handle;
//! callers receive only bounded text snapshots and opaque generation tokens.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

static NEXT_TARGET: AtomicU64 = AtomicU64::new(1);

fn target_token() -> u64 {
    NEXT_TARGET.fetch_add(1, Ordering::Relaxed)
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(all(test, target_os = "macos"))]
mod macos_tests;
mod window;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "macos")]
use macos::NativeReader;
#[cfg(target_os = "windows")]
use windows::NativeReader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    Unsupported,
    PermissionDenied,
    SecureInput,
    SecurityUnknown,
    TargetChanged,
    NoValue,
    TooLarge,
    WindowChanged,
    Native(i32),
    WorkerBusy,
    TimedOut { invalidated_generation: u64 },
    CompositionActive,
    StaleGeneration,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionState {
    #[default]
    Unknown,
    Inactive,
    Active,
}

#[derive(Debug)]
pub struct OwnedSnapshot {
    pub target: u64,
    pub process_id: u32,
    pub text: String,
    pub selection_utf16: Range<usize>,
    pub read_at: Instant,
    pub event_driven: bool,
    pub composition: CompositionState,
}

pub const TEXT_CHANGED: u8 = 1;
pub const FOCUS_CHANGED: u8 = 2;
pub const DESTROYED: u8 = 4;
pub const CONTROL: u8 = 8;

/// Native callbacks only coalesce flags. No text or native handles cross here.
#[derive(Default)]
pub struct EventSignal {
    state: Mutex<(u64, u8)>,
    wake: Condvar,
}
impl EventSignal {
    fn reset(&self, generation: u64) {
        *self.state.lock().unwrap() = (generation, 0);
        self.wake.notify_all();
    }
    pub fn notify(&self, generation: u64, flags: u8) {
        let mut state = self.state.lock().unwrap();
        if state.0 == generation {
            state.1 |= flags;
            self.wake.notify_all();
        }
    }
    pub fn wait(&self, generation: u64, timeout: Duration) -> u8 {
        let state = self.state.lock().unwrap();
        let (mut state, _) = self
            .wake
            .wait_timeout_while(state, timeout, |state| {
                state.0 == generation && state.1 == 0
            })
            .unwrap();
        if state.0 != generation {
            return 0;
        }
        std::mem::take(&mut state.1)
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
enum ReadTarget {
    Capture(u32, usize),
    Retained,
    Reframe(u64, usize),
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
enum Request {
    Read(
        u64,
        ReadTarget,
        mpsc::SyncSender<Result<OwnedSnapshot, ReadError>>,
    ),
    Reset(u64),
}

/// A single long-lived worker, with a one-entry queue and no replacement
/// threads when native calls time out. Drop closes its channel when idle.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub struct ObserverWorker {
    generation: Arc<AtomicU64>,
    busy: Arc<AtomicBool>,
    sender: mpsc::SyncSender<Request>,
    pub events: Arc<EventSignal>,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(super) trait Reader {
    fn capture(
        &mut self,
        generation: u64,
        expected_pid: u32,
        lookbehind: usize,
    ) -> Result<OwnedSnapshot, ReadError>;
    fn read(&mut self, generation: u64) -> Result<OwnedSnapshot, ReadError>;
    fn reframe(
        &mut self,
        generation: u64,
        target: u64,
        lookbehind: usize,
    ) -> Result<OwnedSnapshot, ReadError>;
    fn clear(&mut self);
    fn set_events(&mut self, _events: Arc<EventSignal>) {}
    fn pump(&mut self) {}
    fn watching(&self) -> bool {
        false
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl Reader for NativeReader {
    fn capture(
        &mut self,
        generation: u64,
        expected_pid: u32,
        lookbehind: usize,
    ) -> Result<OwnedSnapshot, ReadError> {
        NativeReader::capture(self, generation, expected_pid, lookbehind)
    }
    fn read(&mut self, generation: u64) -> Result<OwnedSnapshot, ReadError> {
        NativeReader::read(self, generation)
    }
    fn reframe(
        &mut self,
        generation: u64,
        target: u64,
        lookbehind: usize,
    ) -> Result<OwnedSnapshot, ReadError> {
        NativeReader::reframe(self, generation, target, lookbehind)
    }
    fn clear(&mut self) {
        NativeReader::clear(self);
    }
    #[cfg(target_os = "macos")]
    fn set_events(&mut self, events: Arc<EventSignal>) {
        self.set_events(events);
    }
    #[cfg(target_os = "macos")]
    fn pump(&mut self) {
        self.pump();
    }
    #[cfg(target_os = "macos")]
    fn watching(&self) -> bool {
        self.watching()
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl ObserverWorker {
    pub fn start() -> std::io::Result<Self> {
        Self::start_with(NativeReader::new)
    }

    pub(super) fn start_with<R: Reader + 'static>(
        create: impl FnOnce() -> R + Send + 'static,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let generation = Arc::new(AtomicU64::new(0));
        let current = Arc::clone(&generation);
        let busy = Arc::new(AtomicBool::new(false));
        let worker_busy = Arc::clone(&busy);
        let events = Arc::new(EventSignal::default());
        let worker_events = Arc::clone(&events);
        std::thread::Builder::new()
            .name("post-edit-observer".into())
            .spawn(move || {
                let mut reader = create();
                reader.set_events(worker_events);
                let mut owned_generation = current.load(Ordering::SeqCst);
                loop {
                    let generation = current.load(Ordering::SeqCst);
                    if generation != owned_generation {
                        reader.clear();
                        owned_generation = generation;
                    }
                    let request = if reader.watching() {
                        reader.pump();
                        match receiver.recv_timeout(Duration::from_millis(20)) {
                            Ok(request) => request,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(_) => break,
                        }
                    } else {
                        match receiver.recv() {
                            Ok(request) => request,
                            Err(_) => break,
                        }
                    };
                    let (id, target, reply) = match request {
                        Request::Read(id, target, reply) => (id, target, reply),
                        Request::Reset(generation) => {
                            if current.load(Ordering::SeqCst) == generation {
                                reader.clear();
                            }
                            continue;
                        }
                    };
                    if current.load(Ordering::SeqCst) == id {
                        let started = Instant::now();
                        let operation = match target {
                            ReadTarget::Capture(..) => "capture",
                            ReadTarget::Retained => "read",
                            ReadTarget::Reframe(..) => "reframe",
                        };
                        let result = match target {
                            ReadTarget::Capture(pid, context) => reader.capture(id, pid, context),
                            ReadTarget::Retained => reader.read(id),
                            ReadTarget::Reframe(token, context) => {
                                reader.reframe(id, token, context)
                            }
                        };
                        log::debug!("Post-edit native operation: generation={}, operation={}, elapsed_us={}, outcome={:?}", id, operation, started.elapsed().as_micros(), result.as_ref().map(|_| ()).map_err(|e| *e));
                        if current.load(Ordering::SeqCst) == id {
                            let _ = reply.try_send(result);
                        } else {
                            reader.clear();
                        }
                    } else {
                        reader.clear();
                    }
                    worker_busy.store(false, Ordering::SeqCst);
                }
            })?;
        Ok(Self {
            generation,
            busy,
            sender,
            events,
        })
    }

    /// Invalidates queued requests and in-flight responses without waiting for
    /// an OS call that cannot be cancelled.
    pub fn invalidate(&self) -> u64 {
        let generation = self
            .generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        let _ = self.sender.try_send(Request::Reset(generation));
        self.events.reset(generation);
        generation
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    pub fn invalidate_if_current(&self, generation: u64) {
        if self
            .generation
            .compare_exchange(
                generation,
                generation.wrapping_add(1),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            self.events.reset(generation.wrapping_add(1));
            let _ = self
                .sender
                .try_send(Request::Reset(generation.wrapping_add(1)));
        }
    }

    pub fn capture(
        &self,
        generation: u64,
        expected_pid: u32,
        timeout: Duration,
    ) -> Result<OwnedSnapshot, ReadError> {
        self.request(
            generation,
            timeout,
            ReadTarget::Capture(expected_pid, window::CONTEXT_UNITS),
        )
    }

    /// A post-only baseline must include the entire expected insertion before
    /// the caret, even when the document is much larger than the read budget.
    pub fn capture_for_insertion(
        &self,
        generation: u64,
        pid: u32,
        units: usize,
        timeout: Duration,
    ) -> Result<OwnedSnapshot, ReadError> {
        self.request(
            generation,
            timeout,
            ReadTarget::Capture(pid, units.saturating_add(window::CONTEXT_UNITS)),
        )
    }

    /// Expand a late pre-read's window without rebinding to another control.
    pub fn reframe_for_insertion(
        &self,
        generation: u64,
        target: u64,
        units: usize,
        timeout: Duration,
    ) -> Result<OwnedSnapshot, ReadError> {
        self.request(
            generation,
            timeout,
            ReadTarget::Reframe(target, units.saturating_add(window::CONTEXT_UNITS)),
        )
    }

    pub fn read(&self, generation: u64, timeout: Duration) -> Result<OwnedSnapshot, ReadError> {
        self.request(generation, timeout, ReadTarget::Retained)
    }

    fn request(
        &self,
        generation: u64,
        timeout: Duration,
        target: ReadTarget,
    ) -> Result<OwnedSnapshot, ReadError> {
        if self.generation.load(Ordering::SeqCst) != generation {
            return Err(ReadError::StaleGeneration);
        }
        if self
            .busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(ReadError::WorkerBusy);
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        let request = Request::Read(generation, target, reply);
        let deadline = Instant::now() + timeout;
        let mut request = request;
        loop {
            match self.sender.try_send(request) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(returned))
                    if Instant::now() < deadline && self.is_current(generation) =>
                {
                    request = returned;
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => {
                    self.busy.store(false, Ordering::SeqCst);
                    return Err(ReadError::WorkerBusy);
                }
            }
        }
        let result = match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(result) => result,
            Err(_) => {
                let invalidated = self.generation.compare_exchange(
                    generation,
                    generation.wrapping_add(1),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
                if invalidated.is_ok() {
                    self.events.reset(generation.wrapping_add(1));
                    let _ = self
                        .sender
                        .try_send(Request::Reset(generation.wrapping_add(1)));
                } else {
                    return Err(ReadError::StaleGeneration);
                }
                return Err(ReadError::TimedOut {
                    invalidated_generation: generation.wrapping_add(1),
                });
            }
        };
        if self.generation.load(Ordering::SeqCst) != generation {
            return Err(ReadError::StaleGeneration);
        }
        result
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
mod tests {
    use super::*;

    struct BlockingReader {
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        cleared: mpsc::Sender<()>,
    }
    impl Reader for BlockingReader {
        fn capture(
            &mut self,
            generation: u64,
            _: u32,
            _: usize,
        ) -> Result<OwnedSnapshot, ReadError> {
            self.started.send(()).unwrap();
            self.release.recv().unwrap();
            Ok(OwnedSnapshot {
                target: generation,
                process_id: 1,
                text: "synthetic".into(),
                selection_utf16: 0..0,
                read_at: Instant::now(),
                event_driven: false,
                composition: CompositionState::Unknown,
            })
        }
        fn read(&mut self, _: u64) -> Result<OwnedSnapshot, ReadError> {
            Err(ReadError::NoValue)
        }
        fn reframe(&mut self, _: u64, _: u64, _: usize) -> Result<OwnedSnapshot, ReadError> {
            Err(ReadError::Unsupported)
        }
        fn clear(&mut self) {
            let _ = self.cleared.send(());
        }
    }

    #[test]
    fn timed_out_native_call_blocks_replacement_and_discards_its_late_result() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (cleared_tx, cleared_rx) = mpsc::channel();
        let worker = Arc::new(
            ObserverWorker::start_with(move || BlockingReader {
                started: started_tx,
                release: release_rx,
                cleared: cleared_tx,
            })
            .unwrap(),
        );
        let other = Arc::clone(&worker);
        let call = std::thread::spawn(move || other.capture(0, 1, Duration::from_millis(50)));
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(
            call.join().unwrap(),
            Err(ReadError::TimedOut { .. })
        ));
        assert!(matches!(
            worker.capture(1, 1, Duration::from_millis(20)),
            Err(ReadError::WorkerBusy)
        ));
        release_tx.send(()).unwrap();
        cleared_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(!worker.is_current(0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "read-only focused-control metadata; does not read text or change focus"]
    fn focused_control_metadata() {
        let pid = std::env::var("VOICEX_POST_EDIT_METADATA_PID")
            .expect("target PID")
            .parse::<u32>()
            .unwrap();
        let before = super::macos::BODY_READS.load(Ordering::SeqCst);
        println!("metadata probe: {:?}", super::macos::metadata_probe(pid));
        assert_eq!(super::macos::BODY_READS.load(Ordering::SeqCst), before);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "reads only the focused synthetic browser fixture"]
    fn live_browser_fixture() {
        let pid = std::env::var("VOICEX_POST_EDIT_FIXTURE_PID")
            .expect("fixture PID")
            .parse::<u32>()
            .unwrap();
        let kind = std::env::var("VOICEX_POST_EDIT_FIXTURE_KIND").expect("fixture kind");
        println!(
            "focused role: {:?}, subrole attribute: {:?}",
            super::macos::fixture_role_diagnostic(pid),
            super::macos::fixture_subrole_diagnostic(pid)
        );
        println!(
            "synthetic focus attribute names: {:?}",
            super::macos::fixture_capabilities(pid)
        );
        let worker = ObserverWorker::start().unwrap();
        let before_reads = super::macos::BODY_READS.load(Ordering::SeqCst);
        let result = worker.capture(0, pid, Duration::from_millis(800));
        println!(
            "{kind}: {}",
            match &result {
                Ok(_) => "readable".to_owned(),
                Err(error) => format!("{error:?}"),
            }
        );
        if kind == "password" {
            assert!(result.is_err(), "password fixture must not expose text");
            assert_eq!(
                super::macos::BODY_READS.load(Ordering::SeqCst),
                before_reads,
                "password text must never be requested, including an incomplete tree"
            );
        } else if let Ok(snapshot) = result {
            assert_eq!(snapshot.text, "左返利右");
        } else if std::env::var("VOICEX_POST_EDIT_EXPECT_READABLE").as_deref() == Ok("1") {
            panic!("complete synthetic Chrome tree must be readable");
        }
        if std::env::var("VOICEX_POST_EDIT_BROWSER_SESSION").as_deref() == Ok("session") {
            use crate::post_edit::{
                coordinator,
                diagnostics::{self, Status},
            };
            diagnostics::set_enabled(true);
            let attempt = coordinator::begin_attempt();
            let (tx, rx) = mpsc::sync_channel(1);
            let receipt = crate::injector::inject_serialized_with_receipt(
                "反力",
                crate::injector::InjectionGuard::default(),
                crate::injector::AttemptIdentity {
                    session_id: uuid::Uuid::new_v4(),
                    record_id: uuid::Uuid::new_v4(),
                    injection_epoch: 1,
                },
                || {
                    tx.send(coordinator::prepare(attempt, pid).unwrap())
                        .unwrap();
                    (
                        crate::injector::TextInjector::with_mode(
                            crate::injector::TextInjectionMode::Typing,
                            false,
                        ),
                        Some(crate::injector::ResolvedApp {
                            process_id: pid,
                            remote_bridge: false,
                            app_name: Some("Fixture".into()),
                        }),
                    )
                },
            );
            rx.recv().unwrap().dispatched(receipt, "反力".into());
            let deadline = Instant::now() + Duration::from_secs(4);
            let status = loop {
                let status = diagnostics::snapshot();
                if status.substitutions == Some(1) {
                    break status;
                }
                assert!(
                    Instant::now() < deadline,
                    "Chrome synthetic session failed: {status:?}"
                );
                std::thread::sleep(Duration::from_millis(20));
            };
            assert_eq!(status.status, Status::Observing);
            assert_eq!(status.strategy, Some("event_with_polling"));
            diagnostics::set_enabled(false);
            println!("Chrome actual injection, baseline and edit notification: passed");
        }
    }

    #[test]
    fn event_storms_coalesce_without_losing_control_or_accepting_stale_callbacks() {
        let events = EventSignal::default();
        events.reset(3);
        for _ in 0..1000 {
            events.notify(3, TEXT_CHANGED);
        }
        events.notify(3, CONTROL | FOCUS_CHANGED);
        assert_eq!(
            events.wait(3, Duration::ZERO),
            TEXT_CHANGED | CONTROL | FOCUS_CHANGED
        );
        events.reset(4);
        events.notify(3, TEXT_CHANGED | DESTROYED);
        assert_eq!(events.wait(4, Duration::ZERO), 0);
        events.notify(4, CONTROL);
        assert_eq!(events.wait(4, Duration::ZERO), CONTROL);
    }
}
