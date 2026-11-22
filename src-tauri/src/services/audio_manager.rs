use std::path::PathBuf;
use std::sync::{atomic::AtomicU64, Arc};

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

    pub fn start_capture_if_current(
        &self,
        epoch: u64,
        capture_refinement_pcm: bool,
        opened_epoch: &Arc<AtomicU64>,
        is_current_epoch: impl Fn() -> bool,
    ) -> Result<AudioCaptureHandle, AudioCaptureError> {
        let audio: tauri::State<'_, AudioService> = self.app_handle.state();
        audio.start_capture_if_current(
            epoch,
            capture_refinement_pcm,
            opened_epoch,
            is_current_epoch,
        )
    }

    pub fn stop_capture_for_epoch(
        &self,
        epoch: u64,
    ) -> Result<Option<AudioRecordingResult>, AudioCaptureError> {
        let audio: tauri::State<'_, AudioService> = self.app_handle.state();
        audio.stop_capture_for_epoch(epoch)
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
