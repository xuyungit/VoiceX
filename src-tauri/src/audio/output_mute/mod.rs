//! Mute the default output during microphone capture, retaining ownership
//! until restoration succeeds. All platform calls run on one worker thread.
//! Recovery records are local (never synced) and survive exit or disconnect.
//! Only the mute switch changes; volume and player transport are untouched.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use self::windows::PlatformBackend;
#[cfg(target_os = "macos")]
use macos::PlatformBackend;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};
use tauri::Emitter;

const FOLLOW_INTERVAL: Duration = Duration::from_millis(250);
const RECONNECT_INTERVAL: Duration = Duration::from_secs(2);
const EXIT_RETRY_INTERVAL: Duration = Duration::from_millis(100);
/// Failed restores back off to avoid hammering an unavailable device.
const MAX_RESTORE_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "support", rename_all = "snake_case")]
pub enum OutputMuteSupport {
    Supported { device: String },
    Unsupported { device: String },
    NoDevice,
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputMuteStatus {
    pub revision: u64,
    #[serde(flatten)]
    pub output: OutputMuteSupport,
    pub recording: bool,
    pub recovery_error: Option<String>,
    pub pending_restores: Vec<PendingRestore>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingRestore {
    pub device: String,
    pub disconnected: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct OutputDevice {
    id: String,
    name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceMute {
    Settable { muted: bool },
    Unsupported,
    Absent,
}

trait Backend {
    fn default_output(&mut self) -> Result<Option<OutputDevice>, String>;
    fn read_mute(&mut self, id: &str) -> Result<DeviceMute, String>;
    fn set_mute(&mut self, id: &str, muted: bool) -> Result<(), String>;
}

#[derive(Clone, Deserialize, Serialize)]
struct MutedDevice {
    name: String,
    #[serde(skip)]
    unmute_failures: u32,
    #[serde(skip)]
    error: Option<String>,
    #[serde(skip)]
    disconnected: bool,
    #[serde(skip)]
    pending_restore: bool,
    #[serde(skip)]
    next_restore_attempt: Option<Instant>,
}

trait RecoveryStore: Send {
    fn load(&mut self) -> Result<BTreeMap<String, MutedDevice>, String>;
    fn save(&mut self, devices: &BTreeMap<String, MutedDevice>) -> Result<(), String>;
}

struct LocalRecoveryStore;
impl RecoveryStore for LocalRecoveryStore {
    fn load(&mut self) -> Result<BTreeMap<String, MutedDevice>, String> {
        match crate::storage::get_output_mute_recovery().map_err(|e| e.to_string())? {
            Some(json) => serde_json::from_str(&json).map_err(|e| e.to_string()),
            None => Ok(BTreeMap::new()),
        }
    }
    fn save(&mut self, devices: &BTreeMap<String, MutedDevice>) -> Result<(), String> {
        let json = serde_json::to_string(devices).map_err(|e| e.to_string())?;
        crate::storage::save_output_mute_recovery(&json).map_err(|e| e.to_string())
    }
}

struct Muter<B> {
    backend: B,
    store: Box<dyn RecoveryStore>,
    engaged: bool,
    mute_follow_failed: bool,
    muted_here: BTreeMap<String, MutedDevice>,
    /// Each device is attempted once per recording, including failures.
    /// Never fight a user who subsequently changes the mute switch.
    seen: HashSet<String>,
    recovery_error: Option<String>,
    recovery_loaded: bool,
    recovery_retry_at: Option<Instant>,
    revision: u64,
    last_status: Option<OutputMuteStatus>,
}

impl<B: Backend> Muter<B> {
    fn with_store(backend: B, store: impl RecoveryStore + 'static) -> Self {
        let mut this = Self {
            backend,
            store: Box::new(store),
            engaged: false,
            mute_follow_failed: false,
            muted_here: BTreeMap::new(),
            seen: HashSet::new(),
            recovery_error: None,
            recovery_loaded: false,
            recovery_retry_at: None,
            revision: 0,
            last_status: None,
        };
        this.load_recovery();
        this
    }

    fn load_recovery(&mut self) {
        if self
            .recovery_retry_at
            .is_some_and(|deadline| Instant::now() < deadline)
        {
            return;
        }
        match self.store.load() {
            Ok(mut devices) => {
                for device in devices.values_mut() {
                    device.pending_restore = true;
                }
                self.muted_here = devices;
                self.recovery_loaded = true;
                self.recovery_error = None;
                self.recovery_retry_at = None;
            }
            Err(err) => {
                log::error!("Cannot load output mute recovery: {err}");
                self.recovery_error = Some(err);
                self.recovery_retry_at = Some(Instant::now() + RECONNECT_INTERVAL);
            }
        }
    }

    fn persist(&mut self) -> bool {
        match self.store.save(&self.muted_here) {
            Ok(()) => {
                self.recovery_error = None;
                true
            }
            Err(err) => {
                log::error!("Cannot save output mute recovery: {err}");
                self.recovery_error = Some(err);
                false
            }
        }
    }

    fn engage(&mut self) {
        self.engaged = true;
        self.mute_follow_failed = false;
        self.seen.clear();
        self.mute_default_output();
    }

    fn release(&mut self) {
        self.engaged = false;
        self.seen.clear();
        for device in self.muted_here.values_mut() {
            device.pending_restore = true;
        }
        self.unmute_pending();
    }

    fn retry_restore(&mut self) -> Result<(), String> {
        if self.engaged {
            return Err("cannot restore output while recording".into());
        }
        if !self.recovery_loaded {
            self.recovery_retry_at = None;
            self.load_recovery();
        }
        for device in self.muted_here.values_mut() {
            device.next_restore_attempt = None;
        }
        self.unmute_pending();
        Ok(())
    }

    fn tick(&mut self) {
        if !self.recovery_loaded {
            self.load_recovery();
        }
        if self.engaged {
            self.mute_default_output();
        }
        // Old disconnected devices may return during another recording.
        // Restore only obligations not held by this recording.
        self.unmute_pending();
    }

    fn poll_interval(&self) -> Option<Duration> {
        if self.engaged {
            Some(FOLLOW_INTERVAL)
        } else if !self.muted_here.is_empty() || self.recovery_error.is_some() {
            Some(RECONNECT_INTERVAL)
        } else {
            None
        }
    }

    fn mute_default_output(&mut self) {
        if self.mute_follow_failed {
            return;
        }
        let device = match self.backend.default_output() {
            Ok(Some(device)) => device,
            Ok(None) => {
                return;
            }
            Err(err) => {
                log::warn!("Cannot read the default output device: {err}");
                self.mute_follow_failed = true;
                return;
            }
        };
        if !self.seen.insert(device.id.clone()) {
            return;
        }
        if !self.recovery_loaded {
            return;
        }
        match self.backend.read_mute(&device.id) {
            Ok(DeviceMute::Settable { muted: false }) => {
                // Persist intent before changing global state. If the OS reports
                // a failed write after applying it, restoration is still owned.
                self.muted_here
                    .entry(device.id.clone())
                    .or_insert(MutedDevice {
                        name: device.name.clone(),
                        unmute_failures: 0,
                        error: None,
                        disconnected: false,
                        pending_restore: false,
                        next_restore_attempt: None,
                    });
                if !self.persist() {
                    return;
                }
                match self.backend.set_mute(&device.id, true) {
                    Ok(()) => {
                        let owned = self.muted_here.get_mut(&device.id).unwrap();
                        owned.pending_restore = false;
                        owned.unmute_failures = 0;
                        owned.error = None;
                        owned.next_restore_attempt = None;
                        log::info!("Muted output device {} for recording", device.name);
                    }
                    Err(err) => {
                        log::warn!("Failed to mute output device {}: {err}", device.name);
                    }
                }
            }
            Ok(DeviceMute::Settable { muted: true }) => {
                if let Some(owned) = self.muted_here.get_mut(&device.id) {
                    owned.pending_restore = false;
                    owned.unmute_failures = 0;
                    owned.error = None;
                    owned.next_restore_attempt = None;
                }
            }
            Ok(DeviceMute::Unsupported) => {}
            Ok(DeviceMute::Absent) => {}
            Err(err) => {
                log::warn!("Failed to read output device {}: {err}", device.name);
            }
        }
    }

    fn unmute_pending(&mut self) {
        self.unmute_pending_at(Instant::now());
    }

    fn unmute_pending_at(&mut self, now: Instant) {
        let ids: Vec<_> = self
            .muted_here
            .iter()
            .filter(|(_, d)| d.pending_restore)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            let state = self.backend.read_mute(&id);
            let device = self.muted_here.get_mut(&id).unwrap();
            if matches!(state, Ok(DeviceMute::Absent)) {
                device.disconnected = true;
                device.error = None;
                continue;
            }
            if device.disconnected {
                device.disconnected = false;
                device.unmute_failures = 0;
                device.next_restore_attempt = None;
            }
            let result = match state {
                Ok(DeviceMute::Settable { muted: false }) => Ok(()),
                _ if device
                    .next_restore_attempt
                    .is_some_and(|deadline| now < deadline) =>
                {
                    continue
                }
                Ok(DeviceMute::Settable { muted: true }) => self.backend.set_mute(&id, false),
                Ok(DeviceMute::Unsupported) => Err("the device no longer has a mute switch".into()),
                Err(err) => Err(err),
                Ok(DeviceMute::Absent) => unreachable!(),
            };
            match result {
                Ok(()) => {
                    let device = self.muted_here.remove(&id).unwrap();
                    if !self.persist() {
                        // Keep the obligation until the durable record is cleared.
                        self.muted_here.insert(id, device);
                    } else {
                        log::info!("Restored output device {}", device.name);
                    }
                }
                Err(err) => {
                    let device = self.muted_here.get_mut(&id).unwrap();
                    device.unmute_failures = device.unmute_failures.saturating_add(1);
                    let delay = Duration::from_secs(2_u64.pow(device.unmute_failures.min(5)))
                        .min(MAX_RESTORE_BACKOFF);
                    device.next_restore_attempt = Some(now + delay);
                    log::warn!(
                        "Failed to restore output device {} (attempt {}, retry in {}s): {err}",
                        device.name,
                        device.unmute_failures,
                        delay.as_secs()
                    );
                    device.error = Some(err);
                }
            }
        }
    }

    fn status(&mut self) -> OutputMuteStatus {
        let output = match self.backend.default_output() {
            Ok(Some(device)) => match self.backend.read_mute(&device.id) {
                Ok(DeviceMute::Settable { .. }) => OutputMuteSupport::Supported {
                    device: device.name,
                },
                Ok(DeviceMute::Unsupported) => OutputMuteSupport::Unsupported {
                    device: device.name,
                },
                Ok(DeviceMute::Absent) => OutputMuteSupport::NoDevice,
                Err(message) => OutputMuteSupport::Error { message },
            },
            Ok(None) => OutputMuteSupport::NoDevice,
            Err(message) => OutputMuteSupport::Error { message },
        };
        let mut status = OutputMuteStatus {
            revision: self.revision,
            output,
            recording: self.engaged,
            recovery_error: self.recovery_error.clone(),
            pending_restores: self
                .muted_here
                .values()
                .filter(|d| d.pending_restore)
                .map(|d| PendingRestore {
                    device: d.name.clone(),
                    disconnected: d.disconnected,
                    error: d.error.clone(),
                })
                .collect(),
        };
        if self.last_status.as_ref() != Some(&status) {
            self.revision = self.revision.saturating_add(1);
            status.revision = self.revision;
            self.last_status = Some(status.clone());
        }
        status
    }
}

enum Command {
    Engage,
    Release,
    Shutdown {
        deadline: Instant,
        reply: SyncSender<OutputMuteStatus>,
    },
    Status(SyncSender<OutputMuteStatus>),
}
static WORKER: OnceLock<Sender<Command>> = OnceLock::new();
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

/// Called after the local database is initialized. Restore unfinished work
/// before any new recording can acquire an output device.
pub fn init(app: &tauri::AppHandle) {
    let _ = APP.set(app.clone());
    worker();
}

fn worker() -> &'static Sender<Command> {
    WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("voicex-output-mute".into())
            .spawn(move || {
                let mut muter = Muter::with_store(PlatformBackend::new(), LocalRecoveryStore);
                muter.unmute_pending();
                run(muter, rx, |status| {
                    if let Some(app) = APP.get() {
                        if let Err(err) = app.emit("audio:output-mute-status", status) {
                            log::warn!("Cannot publish output mute status: {err}");
                        }
                    }
                });
            })
            .expect("failed to spawn the output mute worker thread");
        tx
    })
}

fn run<B: Backend>(
    mut muter: Muter<B>,
    rx: Receiver<Command>,
    mut publish: impl FnMut(&OutputMuteStatus),
) {
    let mut previous = None;
    let mut next_tick = Instant::now();
    loop {
        // Poll capability while idle too, so a device change updates the UI.
        let interval = muter.poll_interval().unwrap_or(RECONNECT_INTERVAL);
        next_tick = next_tick.min(Instant::now() + interval);
        let command = match rx.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
            Ok(command) => Some(command),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        match command {
            None => {
                muter.tick();
                next_tick = Instant::now() + interval;
            }
            Some(Command::Engage) => muter.engage(),
            Some(Command::Release) => muter.release(),
            Some(Command::Status(reply)) => {
                let _ = reply.send(muter.status());
            }
            Some(Command::Shutdown { deadline, reply }) => {
                muter.engaged = false;
                for device in muter.muted_here.values_mut() {
                    device.pending_restore = true;
                }
                let _ = muter.retry_restore();
                while !muter.muted_here.is_empty() && Instant::now() < deadline {
                    thread::sleep(
                        EXIT_RETRY_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
                    );
                    if Instant::now() >= deadline {
                        break;
                    }
                    for device in muter.muted_here.values_mut() {
                        device.next_restore_attempt = None;
                    }
                    muter.unmute_pending();
                }
                let status = muter.status();
                publish(&status);
                let _ = reply.send(status);
                return;
            }
        }
        let status = muter.status();
        if previous.as_ref() != Some(&status) {
            publish(&status);
            previous = Some(status);
        }
    }
}

pub fn engage() {
    if worker().send(Command::Engage).is_err() {
        log::error!("Output mute worker is gone");
    }
}
pub fn release() {
    if let Some(worker) = WORKER.get() {
        if worker.send(Command::Release).is_err() {
            log::error!("Output mute worker is gone");
        }
    }
}

/// Acknowledges actual restoration, or reports the unresolved devices at the
/// deadline. Offline devices retain a durable recovery record for next launch.
pub fn release_before_exit(timeout: Duration) {
    let Some(worker) = WORKER.get() else {
        return;
    };
    let (reply, rx) = mpsc::sync_channel(1);
    if worker
        .send(Command::Shutdown {
            deadline: Instant::now() + timeout,
            reply,
        })
        .is_err()
    {
        log::error!("Output mute worker is gone during exit");
        return;
    }
    match rx.recv_timeout(timeout) {
        Ok(status) if status.pending_restores.is_empty() && status.recovery_error.is_none() => {}
        Ok(status) => log::error!("Output restoration unfinished at exit: {:?}", status),
        Err(err) => log::error!("Output restoration did not complete before exit: {err}"),
    }
}

pub fn status() -> Result<OutputMuteStatus, String> {
    let (reply, rx) = mpsc::sync_channel(1);
    worker()
        .send(Command::Status(reply))
        .map_err(|_| "output mute worker is gone")?;
    rx.recv_timeout(Duration::from_secs(3))
        .map_err(|e| e.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
struct PlatformBackend;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl PlatformBackend {
    fn new() -> Self {
        Self
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl Backend for PlatformBackend {
    fn default_output(&mut self) -> Result<Option<OutputDevice>, String> {
        Err("muting the output is not supported on this platform".to_string())
    }

    fn read_mute(&mut self, _id: &str) -> Result<DeviceMute, String> {
        Err("muting the output is not supported on this platform".to_string())
    }

    fn set_mute(&mut self, _id: &str, _muted: bool) -> Result<(), String> {
        Err("muting the output is not supported on this platform".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    impl<B: Backend> Muter<B> {
        fn new(backend: B) -> Self {
            Self::with_store(backend, MemoryStore::default())
        }
    }

    #[derive(Default, Clone)]
    struct MemoryStore {
        devices: std::sync::Arc<std::sync::Mutex<BTreeMap<String, MutedDevice>>>,
        fail: bool,
    }
    impl RecoveryStore for MemoryStore {
        fn load(&mut self) -> Result<BTreeMap<String, MutedDevice>, String> {
            if self.fail {
                return Err("storage unavailable".into());
            }
            Ok(self.devices.lock().unwrap().clone())
        }
        fn save(&mut self, devices: &BTreeMap<String, MutedDevice>) -> Result<(), String> {
            if self.fail {
                return Err("storage unavailable".into());
            }
            *self.devices.lock().unwrap() = devices.clone();
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeBackend {
        default: Option<String>,
        /// Connected devices; `None` means no mute switch.
        devices: HashMap<String, Option<bool>>,
        failing_sets: u32,
        failing_reads: u32,
        failing_defaults: u32,
        sets: Vec<(String, bool)>,
    }

    impl FakeBackend {
        fn with(devices: &[(&str, Option<bool>)]) -> Self {
            Self {
                default: devices.first().map(|(id, _)| id.to_string()),
                devices: devices
                    .iter()
                    .map(|(id, mute)| (id.to_string(), *mute))
                    .collect(),
                ..Self::default()
            }
        }
    }

    impl Backend for FakeBackend {
        fn default_output(&mut self) -> Result<Option<OutputDevice>, String> {
            if self.failing_defaults > 0 {
                self.failing_defaults -= 1;
                return Err("busy".into());
            }
            Ok(self.default.clone().map(|id| OutputDevice {
                name: id.to_uppercase(),
                id,
            }))
        }

        fn read_mute(&mut self, id: &str) -> Result<DeviceMute, String> {
            if self.failing_reads > 0 {
                self.failing_reads -= 1;
                return Err("busy".into());
            }
            Ok(match self.devices.get(id) {
                Some(Some(muted)) => DeviceMute::Settable { muted: *muted },
                Some(None) => DeviceMute::Unsupported,
                None => DeviceMute::Absent,
            })
        }

        fn set_mute(&mut self, id: &str, muted: bool) -> Result<(), String> {
            if self.failing_sets > 0 {
                self.failing_sets -= 1;
                return Err("busy".to_string());
            }
            self.sets.push((id.to_string(), muted));
            self.devices.insert(id.to_string(), Some(muted));
            Ok(())
        }
    }

    fn muted(muter: &Muter<FakeBackend>, id: &str) -> Option<bool> {
        muter.backend.devices.get(id).copied().flatten()
    }

    #[test]
    fn mutes_for_the_recording_and_restores_after() {
        let mut muter = Muter::new(FakeBackend::with(&[("speakers", Some(false))]));
        muter.engage();
        assert_eq!(muted(&muter, "speakers"), Some(true));
        muter.release();
        assert_eq!(muted(&muter, "speakers"), Some(false));
        assert_eq!(muter.poll_interval(), None);
    }

    #[test]
    fn a_device_already_muted_is_never_touched() {
        let mut muter = Muter::new(FakeBackend::with(&[("speakers", Some(true))]));
        muter.engage();
        muter.release();
        assert!(muter.backend.sets.is_empty());
        assert_eq!(muted(&muter, "speakers"), Some(true));
    }

    #[test]
    fn a_device_without_a_mute_switch_is_reported_and_left_alone() {
        let mut muter = Muter::new(FakeBackend::with(&[("hdmi", None)]));
        assert_eq!(
            muter.status().output,
            OutputMuteSupport::Unsupported {
                device: "HDMI".to_string()
            }
        );
        muter.engage();
        muter.release();
        assert!(muter.backend.sets.is_empty());
    }

    #[test]
    fn follows_a_switch_of_output_and_restores_both_devices() {
        let mut muter = Muter::new(FakeBackend::with(&[
            ("speakers", Some(false)),
            ("headphones", Some(false)),
        ]));
        muter.engage();
        muter.backend.default = Some("headphones".to_string());
        muter.tick();
        assert_eq!(muted(&muter, "headphones"), Some(true));
        muter.release();
        assert_eq!(muted(&muter, "speakers"), Some(false));
        assert_eq!(muted(&muter, "headphones"), Some(false));
    }

    #[test]
    fn a_device_unmuted_by_hand_mid_recording_stays_unmuted() {
        let mut muter = Muter::new(FakeBackend::with(&[("speakers", Some(false))]));
        muter.engage();
        muter
            .backend
            .devices
            .insert("speakers".to_string(), Some(false));
        muter.tick();
        assert_eq!(muted(&muter, "speakers"), Some(false));
        assert_eq!(muter.backend.sets.len(), 1, "muted once, at engage");
    }

    #[test]
    fn a_device_disconnected_at_release_is_unmuted_when_it_returns() {
        let mut muter = Muter::new(FakeBackend::with(&[("airpods", Some(false))]));
        muter.engage();
        muter.backend.devices.remove("airpods");
        muter.release();
        assert_eq!(muter.poll_interval(), Some(RECONNECT_INTERVAL));

        muter
            .backend
            .devices
            .insert("airpods".to_string(), Some(true));
        muter.tick();
        assert_eq!(muted(&muter, "airpods"), Some(false));
        assert_eq!(muter.poll_interval(), None);
    }

    #[test]
    fn failing_restore_keeps_ownership_and_recovers_automatically() {
        let mut muter = Muter::new(FakeBackend::with(&[("speakers", Some(false))]));
        muter.engage();
        muter.backend.failing_sets = 1;
        muter.release();
        assert_eq!(muted(&muter, "speakers"), Some(true), "first unmute failed");
        muter.unmute_pending_at(Instant::now() + Duration::from_secs(3));
        assert_eq!(
            muted(&muter, "speakers"),
            Some(false),
            "retried on the next tick"
        );

        muter.engage();
        muter.backend.failing_sets = 5;
        muter.release();
        let now = Instant::now();
        for attempt in 1..5 {
            muter.unmute_pending_at(now + Duration::from_secs(attempt * 31));
        }
        assert_eq!(muted(&muter, "speakers"), Some(true));
        assert_eq!(muter.poll_interval(), Some(RECONNECT_INTERVAL));
        assert_eq!(muter.status().pending_restores.len(), 1);
        muter.unmute_pending_at(now + Duration::from_secs(5 * 31));
        assert_eq!(muted(&muter, "speakers"), Some(false));
        assert!(muter.status().pending_restores.is_empty());
    }

    #[test]
    fn status_serializes_recovery_state() {
        let mut m = Muter::new(FakeBackend::with(&[("speakers", Some(false))]));
        m.engage();
        m.backend.failing_sets = 1;
        m.release();
        let json = serde_json::to_value(m.status()).unwrap();
        assert_eq!(json["support"], "supported");
        assert_eq!(json["recording"], false);
        assert_eq!(json["pendingRestores"][0]["error"], "busy");
    }

    #[test]
    fn failed_mute_is_not_retried_within_a_recording() {
        for failure in ["default", "read", "write"] {
            let mut m = Muter::new(FakeBackend::with(&[("speakers", Some(false))]));
            match failure {
                "default" => m.backend.failing_defaults = 1,
                "read" => m.backend.failing_reads = 1,
                _ => m.backend.failing_sets = 1,
            }
            m.engage();
            for _ in 0..4 {
                m.tick();
            }
            assert_eq!(muted(&m, "speakers"), Some(false));
            assert!(m.backend.sets.is_empty());
            m.release();
            m.engage();
            assert_eq!(muted(&m, "speakers"), Some(true));
            m.release();
            assert_eq!(muted(&m, "speakers"), Some(false));
        }
    }

    #[test]
    fn restart_recovers_a_disconnected_device_from_local_store() {
        let store = MemoryStore::default();
        let mut first = Muter::with_store(
            FakeBackend::with(&[("airpods", Some(false))]),
            store.clone(),
        );
        first.engage();
        first.backend.devices.clear();
        first.release();
        assert!(first.status().pending_restores[0].disconnected);
        drop(first);
        let mut restarted =
            Muter::with_store(FakeBackend::with(&[("airpods", Some(true))]), store.clone());
        restarted.tick();
        assert_eq!(muted(&restarted, "airpods"), Some(false));
        assert!(store.devices.lock().unwrap().is_empty());
    }

    #[test]
    fn recovery_storage_failure_prevents_new_global_mute() {
        let mut m = Muter::with_store(
            FakeBackend::with(&[("speakers", Some(false))]),
            MemoryStore {
                fail: true,
                ..Default::default()
            },
        );
        m.engage();
        assert_eq!(muted(&m, "speakers"), Some(false));
        assert!(m.status().recovery_error.is_some());
    }

    #[test]
    fn restore_backoff_still_observes_manual_unmute() {
        let mut m = Muter::new(FakeBackend::with(&[("speakers", Some(false))]));
        m.engage();
        m.backend.failing_sets = 5;
        m.release();
        let now = Instant::now();
        for attempt in 1..5 {
            m.unmute_pending_at(now + Duration::from_secs(attempt * 31));
        }
        m.backend.devices.insert("speakers".into(), Some(false));
        m.tick();
        assert!(m.status().pending_restores.is_empty());
    }

    #[test]
    fn restore_waits_for_backoff_but_reconnect_retries_immediately() {
        let mut m = Muter::new(FakeBackend::with(&[("speakers", Some(false))]));
        m.engage();
        m.backend.failing_sets = 1;
        m.release();
        m.tick();
        assert_eq!(muted(&m, "speakers"), Some(true), "no immediate retry");
        m.backend.devices.remove("speakers");
        m.tick();
        m.backend.devices.insert("speakers".into(), Some(true));
        m.tick();
        assert_eq!(muted(&m, "speakers"), Some(false));
    }

    #[test]
    fn returning_old_device_does_not_unmute_current_recording() {
        let mut m = Muter::new(FakeBackend::with(&[
            ("speakers", Some(false)),
            ("headphones", Some(false)),
        ]));
        m.engage();
        m.backend.devices.remove("speakers");
        m.release();
        m.backend.default = Some("headphones".into());
        m.engage();
        m.backend.devices.insert("speakers".into(), Some(true));
        m.tick();
        assert_eq!(muted(&m, "speakers"), Some(false));
        assert_eq!(muted(&m, "headphones"), Some(true));
        m.release();
        assert_eq!(muted(&m, "headphones"), Some(false));
    }

    #[test]
    fn restore_cannot_unmute_an_active_recording() {
        let mut m = Muter::new(FakeBackend::with(&[("speakers", Some(false))]));
        m.engage();
        assert!(m.retry_restore().is_err());
        assert_eq!(muted(&m, "speakers"), Some(true));
    }

    fn shutdown_with_failures(failures: u32, timeout: Duration) -> OutputMuteStatus {
        let mut m = Muter::new(FakeBackend::with(&[("speakers", Some(false))]));
        m.engage();
        m.backend.failing_sets = failures;
        let (tx, rx) = mpsc::channel();
        let (reply, result) = mpsc::sync_channel(1);
        tx.send(Command::Shutdown {
            deadline: Instant::now() + timeout,
            reply,
        })
        .unwrap();
        let handle = thread::spawn(move || run(m, rx, |_| {}));
        let status = result.recv_timeout(Duration::from_secs(1)).unwrap();
        handle.join().unwrap();
        status
    }

    #[test]
    fn exit_acknowledges_success_only_after_transient_error_recovers() {
        let status = shutdown_with_failures(1, Duration::from_millis(300));
        assert!(status.pending_restores.is_empty());
        assert!(status.recovery_error.is_none());
    }

    #[test]
    fn exit_deadline_reports_unresolved_output() {
        let status = shutdown_with_failures(10, Duration::from_millis(10));
        assert_eq!(status.pending_restores.len(), 1);
        assert!(status.pending_restores[0].error.is_some());
    }
}
