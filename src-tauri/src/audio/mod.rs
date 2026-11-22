//! Audio capture module
//!
//! Handles microphone input capture with platform-specific backends, and
//! muting the system output while it records.

mod capture;
mod chunker;
pub mod cue;
mod device;
pub mod output_mute;
pub(crate) mod resample;

use std::{path::PathBuf, sync::Mutex};

pub use capture::{
    write_temp_wav, AudioCaptureError, AudioCaptureHandle, AudioCaptureService, AudioConfig,
    AudioRecordingResult, AudioVisualizationFrame,
};
pub use chunker::AudioChunker;
pub use device::{AudioDevice, AudioInputDeviceManager};

#[derive(Default)]
struct CaptureSlot {
    service: AudioCaptureService,
    active_epoch: u64,
}

/// Wraps the capture service with shared recordings directory for Tauri state.
#[derive(Default)]
pub struct AudioService {
    capture: Mutex<CaptureSlot>,
    recordings_dir: Mutex<Option<PathBuf>>,
    preferred_device_uid: Mutex<Option<String>>,
}

impl AudioService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn init_paths(&self, recordings_dir: PathBuf) -> Result<(), AudioCaptureError> {
        std::fs::create_dir_all(&recordings_dir).map_err(|e| {
            AudioCaptureError::StartFailed(format!("Failed to create recordings dir: {}", e))
        })?;

        let mut guard = self
            .recordings_dir
            .lock()
            .map_err(|_| AudioCaptureError::StartFailed("Failed to lock recordings dir".into()))?;
        *guard = Some(recordings_dir);
        Ok(())
    }

    pub fn start_capture(
        &self,
        capture_refinement_pcm: bool,
    ) -> Result<AudioCaptureHandle, AudioCaptureError> {
        self.start_capture_if_current(0, capture_refinement_pcm, || true)
    }

    /// Serialize release of a superseded session with opening the next one.
    /// Epoch zero belongs to direct capture callers outside the session.
    pub fn start_capture_if_current(
        &self,
        epoch: u64,
        capture_refinement_pcm: bool,
        is_current_epoch: impl Fn() -> bool,
    ) -> Result<AudioCaptureHandle, AudioCaptureError> {
        let dir = self
            .recordings_dir
            .lock()
            .map_err(|_| AudioCaptureError::StartFailed("Failed to lock recordings dir".into()))?
            .clone();

        self.open_capture_in_slot(epoch, is_current_epoch, |service| {
            service.set_preferred_device(self.preferred_device()?);
            service.start(dir.as_deref(), capture_refinement_pcm)
        })
    }

    fn open_capture_in_slot(
        &self,
        epoch: u64,
        is_current_epoch: impl Fn() -> bool,
        open: impl FnOnce(&mut AudioCaptureService) -> Result<AudioCaptureHandle, AudioCaptureError>,
    ) -> Result<AudioCaptureHandle, AudioCaptureError> {
        let mut slot = self
            .capture
            .lock()
            .map_err(|_| AudioCaptureError::StartFailed("Audio service poisoned".into()))?;
        if !is_current_epoch() {
            return Err(AudioCaptureError::Cancelled);
        }

        if epoch != 0 && slot.active_epoch != 0 && slot.active_epoch != epoch {
            slot.active_epoch = 0;
            discard_cancelled_files(&slot.service.stop()?);
            if !is_current_epoch() {
                return Err(AudioCaptureError::Cancelled);
            }
        }

        let handle = open(&mut slot.service)?;
        slot.active_epoch = epoch;
        if !is_current_epoch() {
            log::info!(
                "Audio capture cancelled while opening (epoch={}); rolling back immediately",
                epoch
            );
            slot.active_epoch = 0;
            discard_cancelled_files(&slot.service.stop()?);
            return Err(AudioCaptureError::Cancelled);
        }

        Ok(handle)
    }

    pub fn stop_capture(&self) -> Result<AudioRecordingResult, AudioCaptureError> {
        let mut slot = self
            .capture
            .lock()
            .map_err(|_| AudioCaptureError::StopFailed("Audio service poisoned".into()))?;
        slot.active_epoch = 0;
        slot.service.stop()
    }

    /// Stop capture only if it still belongs to `epoch`. Returns `Ok(None)`
    /// when that epoch was already stopped, rolled back, or superseded.
    pub fn stop_capture_for_epoch(
        &self,
        epoch: u64,
    ) -> Result<Option<AudioRecordingResult>, AudioCaptureError> {
        let mut slot = self
            .capture
            .lock()
            .map_err(|_| AudioCaptureError::StopFailed("Audio service poisoned".into()))?;
        if slot.active_epoch != epoch || !slot.service.is_running() {
            return Ok(None);
        }
        slot.active_epoch = 0;
        slot.service.stop().map(Some)
    }

    pub fn discard_capture_for_epoch(&self, epoch: u64) -> Result<(), AudioCaptureError> {
        if let Some(summary) = self.stop_capture_for_epoch(epoch)? {
            discard_cancelled_files(&summary);
        }
        Ok(())
    }

    pub fn set_preferred_device(&self, uid: Option<String>) -> Result<(), AudioCaptureError> {
        *self
            .preferred_device_uid
            .lock()
            .map_err(|_| AudioCaptureError::StartFailed("Preferred audio device lock poisoned".into()))? = uid;
        Ok(())
    }

    fn preferred_device(&self) -> Result<Option<String>, AudioCaptureError> {
        self.preferred_device_uid
            .lock()
            .map(|uid| uid.clone())
            .map_err(|_| AudioCaptureError::StartFailed("Preferred audio device lock poisoned".into()))
    }

    pub fn is_running(&self) -> bool {
        self.capture
            .lock()
            .map(|c| c.service.is_running())
            .unwrap_or(false)
    }

    pub fn recordings_dir(&self) -> Option<PathBuf> {
        self.recordings_dir.lock().ok().and_then(|d| d.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    };
    use std::time::Duration;

    #[test]
    fn new_capture_opens_before_delayed_cleanup_and_keeps_its_audio() {
        let audio = AudioService::new();
        let current = AtomicU64::new(1);
        audio
            .open_capture_in_slot(
                1,
                || current.load(Ordering::SeqCst) == 1,
                |service| service.start_synthetic_capture(&[10; 1600]),
            )
            .unwrap();

        // Force the replacement to acquire the slot before the old cleanup.
        current.store(2, Ordering::SeqCst);
        let mut replacement = audio
            .open_capture_in_slot(
                2,
                || current.load(Ordering::SeqCst) == 2,
                |service| service.start_synthetic_capture(&[123; 1600]),
            )
            .unwrap();
        audio.discard_capture_for_epoch(1).unwrap();

        assert!(audio.is_running());
        let stopped = audio.stop_capture_for_epoch(2).unwrap().unwrap();
        assert_eq!(stopped.duration_ms, 100);
        let pcm = replacement.receiver.blocking_recv().unwrap();
        assert_eq!(pcm, 123i16.to_le_bytes().repeat(1600));
        assert!(!audio.is_running());
    }

    #[test]
    fn cancelled_capture_is_released_when_the_blocked_device_open_returns() {
        let audio = AudioService::new();
        let current = AtomicU64::new(1);
        let (opening, reached_open) = mpsc::channel();
        let (unblock, continue_open) = mpsc::channel();
        std::thread::scope(|scope| {
            let startup = scope.spawn(|| {
                audio.open_capture_in_slot(
                    1,
                    || current.load(Ordering::SeqCst) == 1,
                    move |service| {
                        opening.send(()).unwrap();
                        continue_open.recv_timeout(Duration::from_secs(5)).unwrap();
                        service.start_synthetic_capture(&[0; 1600])
                    },
                )
            });

            reached_open.recv_timeout(Duration::from_secs(5)).unwrap();
            current.store(2, Ordering::SeqCst);
            unblock.send(()).unwrap();
            assert!(matches!(
                startup.join().unwrap(),
                Err(AudioCaptureError::Cancelled)
            ));
        });
        assert!(!audio.is_running());
        assert!(audio.stop_capture_for_epoch(1).unwrap().is_none());
    }
}

fn discard_cancelled_files(summary: &AudioRecordingResult) {
    if let Some(path) = summary.path.as_deref() {
        let _ = std::fs::remove_file(path);
    }
    if let Some(path) = summary.refinement_path.as_deref() {
        let _ = std::fs::remove_file(path);
    }
}
