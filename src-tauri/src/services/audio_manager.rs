use std::path::PathBuf;

use tauri::{AppHandle, Manager};

use crate::audio::{AudioCaptureError, AudioCaptureHandle, AudioRecordingResult, AudioService};

/// Thin wrapper around `AudioService` so the session controller does not touch Tauri state directly.
#[derive(Clone)]
pub struct AudioManager {
    app_handle: AppHandle,
}

impl AudioManager {
    pub fn new(app_handle: AppHandle) -> Self {
        Self { app_handle }
    }

    pub fn start_capture(
        &self,
        capture_refinement_pcm: bool,
    ) -> Result<AudioCaptureHandle, AudioCaptureError> {
        let audio: tauri::State<'_, AudioService> = self.app_handle.state();
        audio.start_capture(capture_refinement_pcm)
    }

    pub fn stop_capture(&self) -> Result<AudioRecordingResult, AudioCaptureError> {
        let audio: tauri::State<'_, AudioService> = self.app_handle.state();
        audio.stop_capture()
    }

    pub fn set_preferred_device(&self, uid: Option<String>) -> Result<(), AudioCaptureError> {
        let audio: tauri::State<'_, AudioService> = self.app_handle.state();
        audio.set_preferred_device(uid)
    }

    pub fn recordings_dir(&self) -> Option<PathBuf> {
        let audio: tauri::State<'_, AudioService> = self.app_handle.state();
        audio.recordings_dir()
    }
}
