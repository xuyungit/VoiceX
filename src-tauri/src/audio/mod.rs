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

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

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
        let dir = self
            .recordings_dir
            .lock()
            .map_err(|_| AudioCaptureError::StartFailed("Failed to lock recordings dir".into()))?
            .clone();

        let mut slot = self
            .capture
            .lock()
            .map_err(|_| AudioCaptureError::StartFailed("Audio service poisoned".into()))?;
        let handle = slot.service.start(dir.as_deref(), capture_refinement_pcm)?;
        slot.active_epoch = 0;
        Ok(handle)
    }

    /// Open microphone capture for `epoch`, atomically rolling back and
    /// releasing the device if `is_current_epoch` becomes false before or
    /// during startup. Publishes `opened_epoch` under the same mutex before
    /// the post-start check so cancellation never races with a newer session.
    pub fn start_capture_if_current(
        &self,
        epoch: u64,
        capture_refinement_pcm: bool,
        opened_epoch: &Arc<AtomicU64>,
        is_current_epoch: impl Fn() -> bool,
    ) -> Result<AudioCaptureHandle, AudioCaptureError> {
        let dir = self
            .recordings_dir
            .lock()
            .map_err(|_| AudioCaptureError::StartFailed("Failed to lock recordings dir".into()))?
            .clone();

        let mut slot = self
            .capture
            .lock()
            .map_err(|_| AudioCaptureError::StartFailed("Audio service poisoned".into()))?;
        if !is_current_epoch() {
            return Err(AudioCaptureError::Cancelled);
        }

        let handle = slot.service.start(dir.as_deref(), capture_refinement_pcm)?;
        slot.active_epoch = epoch;
        opened_epoch.store(epoch, Ordering::SeqCst);

        if !is_current_epoch() {
            log::info!(
                "Audio capture cancelled while opening (epoch={}); rolling back immediately",
                epoch
            );
            slot.active_epoch = 0;
            opened_epoch.store(0, Ordering::SeqCst);
            if let Ok(summary) = slot.service.stop() {
                discard_cancelled_files(&summary);
            }
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
        if !slot.service.is_running() || (slot.active_epoch != 0 && slot.active_epoch != epoch) {
            return Ok(None);
        }
        slot.active_epoch = 0;
        slot.service.stop().map(Some)
    }

    pub fn set_preferred_device(&self, uid: Option<String>) -> Result<(), AudioCaptureError> {
        let mut slot = self
            .capture
            .lock()
            .map_err(|_| AudioCaptureError::StartFailed("Audio service poisoned".into()))?;
        slot.service.set_preferred_device(uid);
        Ok(())
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

fn discard_cancelled_files(summary: &AudioRecordingResult) {
    if let Some(path) = summary.path.as_deref() {
        let _ = std::fs::remove_file(path);
    }
    if let Some(path) = summary.refinement_path.as_deref() {
        let _ = std::fs::remove_file(path);
    }
}
