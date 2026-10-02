//! Mute the system output while dictation records.
//!
//! Music playing through the speakers leaks into the microphone and ends up
//! in the transcript. With the setting on, the session engages this module
//! once capture has actually started and releases it once capture stops.
//! Every way a recording ends goes through that stop (release, hands-free
//! stop, timeout, Escape, a terminal ASR error), so all of them give the
//! output back, and the app gives it back on exit too.
//!
//! Only the mute switch is touched. Volume is left alone and players keep
//! playing; they are just not heard for the length of the recording.
//!
//! The output is followed while recording: when the default output device
//! changes (headphones plugged in or dropping out), the new one is muted too.
//! Devices are remembered by their stable id (Core Audio UID, MMDevice
//! endpoint id) and unmuted on release; one that is disconnected at that
//! moment is unmuted when it comes back. A device that was already muted is
//! never touched, so it stays muted afterwards.
//!
//! Every platform call runs on one worker thread. Core Audio would take any
//! thread, but the Windows endpoint API needs a COM apartment, and a thread of
//! its own keeps a slow device (Bluetooth) off the session actor.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
use self::windows::PlatformBackend;
#[cfg(target_os = "macos")]
use macos::PlatformBackend;

use std::collections::{BTreeMap, HashSet};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use serde::Serialize;

/// How often the default output is checked while recording, to catch a
/// switch to another device.
const FOLLOW_INTERVAL: Duration = Duration::from_millis(250);
/// How often a device that was disconnected at release is looked for again.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(2);
/// Failed unmutes of a connected device before it is given up on. A device
/// that is going away fails once or twice and then reads as absent; one that
/// keeps failing while present is not going to start working.
const MAX_UNMUTE_FAILURES: u32 = 5;

/// What the current default output offers, for the settings page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "support", rename_all = "snake_case")]
pub enum OutputMuteStatus {
    Supported {
        device: String,
    },
    /// The device has no mute switch software can set, so it stays audible.
    Unsupported {
        device: String,
    },
    NoDevice,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone)]
struct OutputDevice {
    /// Stable across reconnects.
    id: String,
    name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceMute {
    /// Connected, with a settable mute switch in this position.
    Settable {
        muted: bool,
    },
    /// Connected, without a mute switch software can set: HDMI and
    /// DisplayPort outputs, multi-output devices, some audio interfaces.
    Unsupported,
    Absent,
}

trait Backend {
    fn default_output(&mut self) -> Result<Option<OutputDevice>, String>;
    fn read_mute(&mut self, id: &str) -> Result<DeviceMute, String>;
    fn set_mute(&mut self, id: &str, muted: bool) -> Result<(), String>;
}

struct MutedDevice {
    name: String,
    unmute_failures: u32,
}

struct Muter<B> {
    backend: B,
    engaged: bool,
    /// Devices muted here and not unmuted yet. Outlives the recording for a
    /// device that was disconnected at release.
    muted_here: BTreeMap<String, MutedDevice>,
    /// Devices already dealt with in this recording. Each is dealt with once,
    /// so a device the user unmutes mid-recording is not muted again and an
    /// unsupported one is reported once.
    seen: HashSet<String>,
}

impl<B: Backend> Muter<B> {
    fn new(backend: B) -> Self {
        Self {
            backend,
            engaged: false,
            muted_here: BTreeMap::new(),
            seen: HashSet::new(),
        }
    }

    fn engage(&mut self) {
        self.engaged = true;
        self.seen.clear();
        self.mute_default_output();
    }

    fn release(&mut self) {
        self.engaged = false;
        self.seen.clear();
        self.unmute_pending();
        for (id, device) in &self.muted_here {
            log::info!(
                "Output device {} ({}) is disconnected; it will be unmuted when it reconnects",
                device.name,
                id
            );
        }
    }

    fn tick(&mut self) {
        if self.engaged {
            self.mute_default_output();
        } else {
            self.unmute_pending();
        }
    }

    /// `None` when there is nothing to watch, so the worker can sleep.
    fn poll_interval(&self) -> Option<Duration> {
        if self.engaged {
            Some(FOLLOW_INTERVAL)
        } else if !self.muted_here.is_empty() {
            Some(RECONNECT_INTERVAL)
        } else {
            None
        }
    }

    fn mute_default_output(&mut self) {
        let device = match self.backend.default_output() {
            Ok(Some(device)) => device,
            Ok(None) => return,
            Err(err) => {
                log::warn!("Cannot read the default output device: {}", err);
                return;
            }
        };
        if !self.seen.insert(device.id.clone()) {
            return;
        }

        match self.backend.read_mute(&device.id) {
            Ok(DeviceMute::Settable { muted: false }) => {
                match self.backend.set_mute(&device.id, true) {
                    Ok(()) => {
                        log::info!("Muted output device {} for recording", device.name);
                        self.muted_here.insert(
                            device.id,
                            MutedDevice {
                                name: device.name,
                                unmute_failures: 0,
                            },
                        );
                    }
                    Err(err) => log::warn!("Failed to mute output device {}: {}", device.name, err),
                }
            }
            Ok(DeviceMute::Settable { muted: true }) => {
                // Either the user's own mute, which is theirs to keep, or still
                // ours from a recording that ended while it was disconnected.
                if !self.muted_here.contains_key(&device.id) {
                    log::info!(
                        "Output device {} is already muted; leaving it as it is",
                        device.name
                    );
                }
            }
            Ok(DeviceMute::Unsupported) => log::warn!(
                "Output device {} cannot be muted by software; it stays audible while recording",
                device.name
            ),
            // Switched away again between the two reads; the next tick sees
            // whatever replaced it.
            Ok(DeviceMute::Absent) => {
                self.seen.remove(&device.id);
            }
            Err(err) => log::warn!(
                "Failed to read the mute state of output device {}: {}",
                device.name,
                err
            ),
        }
    }

    fn unmute_pending(&mut self) {
        let ids: Vec<String> = self.muted_here.keys().cloned().collect();
        for id in ids {
            let result = match self.backend.read_mute(&id) {
                Ok(DeviceMute::Absent) => continue,
                Ok(DeviceMute::Settable { muted: true }) => self.backend.set_mute(&id, false),
                // Unmuted by hand in the meantime: nothing left to restore.
                Ok(DeviceMute::Settable { muted: false }) => Ok(()),
                Ok(DeviceMute::Unsupported) => Err("the device no longer has a mute switch".into()),
                Err(err) => Err(err),
            };
            let Some(device) = self.muted_here.get_mut(&id) else {
                continue;
            };
            match result {
                Ok(()) => {
                    log::info!("Restored output device {}", device.name);
                    self.muted_here.remove(&id);
                }
                Err(err) => {
                    device.unmute_failures += 1;
                    log::warn!(
                        "Failed to unmute output device {} (attempt {}/{}): {}",
                        device.name,
                        device.unmute_failures,
                        MAX_UNMUTE_FAILURES,
                        err
                    );
                    if device.unmute_failures >= MAX_UNMUTE_FAILURES {
                        log::error!("Giving up on unmuting output device {}", device.name);
                        self.muted_here.remove(&id);
                    }
                }
            }
        }
    }

    fn status(&mut self) -> OutputMuteStatus {
        let device = match self.backend.default_output() {
            Ok(Some(device)) => device,
            Ok(None) => return OutputMuteStatus::NoDevice,
            Err(message) => return OutputMuteStatus::Error { message },
        };
        match self.backend.read_mute(&device.id) {
            Ok(DeviceMute::Settable { .. }) => OutputMuteStatus::Supported {
                device: device.name,
            },
            Ok(DeviceMute::Unsupported) => OutputMuteStatus::Unsupported {
                device: device.name,
            },
            Ok(DeviceMute::Absent) => OutputMuteStatus::NoDevice,
            Err(message) => OutputMuteStatus::Error { message },
        }
    }
}

enum Command {
    Engage,
    Release(Option<SyncSender<()>>),
    Status(SyncSender<OutputMuteStatus>),
}

static WORKER: OnceLock<Sender<Command>> = OnceLock::new();

fn worker() -> &'static Sender<Command> {
    WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("voicex-output-mute".to_string())
            .spawn(move || run(rx))
            .expect("failed to spawn the output mute worker thread");
        tx
    })
}

fn run(rx: Receiver<Command>) {
    let mut muter = Muter::new(PlatformBackend::new());
    loop {
        let command = match muter.poll_interval() {
            Some(interval) => match rx.recv_timeout(interval) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(command) => Some(command),
                Err(_) => return,
            },
        };
        match command {
            None => muter.tick(),
            Some(Command::Engage) => muter.engage(),
            Some(Command::Release(done)) => {
                muter.release();
                if let Some(done) = done {
                    let _ = done.send(());
                }
            }
            Some(Command::Status(reply)) => {
                let _ = reply.send(muter.status());
            }
        }
    }
}

/// Mute the default output for a recording that has started. Returns at
/// once; the worker does the muting.
pub fn engage() {
    let _ = worker().send(Command::Engage);
}

/// Give back what [`engage`] muted. Safe to call when nothing was engaged.
pub fn release() {
    if let Some(worker) = WORKER.get() {
        let _ = worker.send(Command::Release(None));
    }
}

/// [`release`], waiting up to `timeout` for it to finish so the process does
/// not exit with the output still muted.
pub fn release_before_exit(timeout: Duration) {
    let Some(worker) = WORKER.get() else {
        return;
    };
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    if worker.send(Command::Release(Some(done_tx))).is_ok()
        && done_rx.recv_timeout(timeout).is_err()
    {
        log::warn!("Output mute was not restored before exit");
    }
}

/// Whether the current default output can be muted. Blocking.
pub fn status() -> OutputMuteStatus {
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    let gone = || OutputMuteStatus::Error {
        message: "output mute worker is gone".to_string(),
    };
    if worker().send(Command::Status(reply_tx)).is_err() {
        return gone();
    }
    reply_rx.recv().unwrap_or_else(|_| gone())
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

    #[derive(Default)]
    struct FakeBackend {
        default: Option<String>,
        /// Connected devices; `None` means no mute switch.
        devices: HashMap<String, Option<bool>>,
        failing_sets: u32,
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
            Ok(self.default.clone().map(|id| OutputDevice {
                name: id.to_uppercase(),
                id,
            }))
        }

        fn read_mute(&mut self, id: &str) -> Result<DeviceMute, String> {
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
            muter.status(),
            OutputMuteStatus::Unsupported {
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
    fn a_failing_unmute_is_retried_then_given_up() {
        let mut muter = Muter::new(FakeBackend::with(&[("speakers", Some(false))]));
        muter.engage();
        muter.backend.failing_sets = 1;
        muter.release();
        assert_eq!(muted(&muter, "speakers"), Some(true), "first unmute failed");
        muter.tick();
        assert_eq!(
            muted(&muter, "speakers"),
            Some(false),
            "retried on the next tick"
        );

        muter.engage();
        muter.backend.failing_sets = MAX_UNMUTE_FAILURES;
        muter.release();
        for _ in 1..MAX_UNMUTE_FAILURES {
            muter.tick();
        }
        assert_eq!(muter.poll_interval(), None);
    }

    #[test]
    fn status_serializes_with_a_support_tag() {
        let status = OutputMuteStatus::Supported {
            device: "Speakers".to_string(),
        };
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::json!({ "support": "supported", "device": "Speakers" })
        );
    }
}
