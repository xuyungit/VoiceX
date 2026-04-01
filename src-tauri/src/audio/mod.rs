//! Audio capture module
//!
//! Handles microphone input capture with platform-specific backends.

mod capture;
mod chunker;
mod device;

use std::{path::PathBuf, sync::Mutex};

pub use capture::{
    AudioCaptureError, AudioCaptureHandle, AudioCaptureService, AudioConfig,
    AudioRecordingResult, AudioVisualizationFrame,
};
pub use chunker::AudioChunker;
pub use device::{AudioDevice, AudioInputDeviceManager};

/// Wraps the capture service with shared recordings directory for Tauri state.
#[derive(Default)]
pub struct AudioService {
    capture: Mutex<AudioCaptureService>,
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

        let mut capture = self
            .capture
            .lock()
            .map_err(|_| AudioCaptureError::StartFailed("Audio service poisoned".into()))?;
        capture.start(dir.as_deref(), capture_refinement_pcm)
    }

    pub fn stop_capture(&self) -> Result<AudioRecordingResult, AudioCaptureError> {
        let mut capture = self
            .capture
            .lock()
            .map_err(|_| AudioCaptureError::StopFailed("Audio service poisoned".into()))?;
        capture.stop()
    }

    pub fn set_preferred_device(&self, uid: Option<String>) -> Result<(), AudioCaptureError> {
        let mut capture = self
            .capture
            .lock()
            .map_err(|_| AudioCaptureError::StartFailed("Audio service poisoned".into()))?;
        capture.set_preferred_device(uid);
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.capture.lock().map(|c| c.is_running()).unwrap_or(false)
    }

    pub fn recordings_dir(&self) -> Option<PathBuf> {
        self.recordings_dir.lock().ok().and_then(|d| d.clone())
    }
}
