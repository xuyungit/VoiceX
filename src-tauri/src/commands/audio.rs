//! Audio-related commands

use crate::audio::{AudioDevice, AudioInputDeviceManager, AudioRecordingResult, AudioService};
use serde::Serialize;
use tauri_plugin_opener::OpenerExt;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStartInfo {
    pub file_path: Option<String>,
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStopInfo {
    pub file_path: Option<String>,
    pub refinement_file_path: Option<String>,
    pub duration_ms: u64,
    pub bytes_written: u64,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Get list of available input devices
#[tauri::command]
pub fn get_input_devices() -> Vec<AudioDevice> {
    AudioInputDeviceManager::get_devices()
}

/// Set the preferred input device
#[tauri::command]
pub fn set_input_device(uid: String, audio: tauri::State<'_, AudioService>) -> Result<(), String> {
    log::info!("Setting input device to: {}", uid);
    audio
        .set_preferred_device(Some(uid))
        .map_err(|e| e.to_string())
}

/// Start microphone capture; streaming chunks are exposed on the returned channel (not yet consumed in UI).
#[tauri::command]
pub fn start_audio_capture(
    audio: tauri::State<'_, AudioService>,
) -> Result<CaptureStartInfo, String> {
    let handle = audio.start_capture(false).map_err(|e| e.to_string())?;
    let crate::audio::AudioCaptureHandle {
        receiver,
        viz_receiver,
        file_path,
        sample_rate,
        channels,
    } = handle;
    // Drop the receiver for now; ASR pipeline will consume it later.
    let _ = receiver;
    let _ = viz_receiver;

    Ok(CaptureStartInfo {
        file_path: file_path.map(|p| p.to_string_lossy().to_string()),
        sample_rate,
        channels,
    })
}

/// Stop capture and finalize the compressed recording.
#[tauri::command]
pub fn stop_audio_capture(
    audio: tauri::State<'_, AudioService>,
) -> Result<CaptureStopInfo, String> {
    let summary: AudioRecordingResult = audio.stop_capture().map_err(|e| e.to_string())?;
    Ok(CaptureStopInfo {
        file_path: summary.path.map(|p| p.to_string_lossy().to_string()),
        refinement_file_path: summary
            .refinement_path
            .map(|p| p.to_string_lossy().to_string()),
        duration_ms: summary.duration_ms,
        bytes_written: summary.bytes_written,
        sample_rate: summary.sample_rate,
        channels: summary.channels,
    })
}

/// Get the recordings directory path for file explorer access.
#[tauri::command]
pub fn get_recordings_dir(audio: tauri::State<'_, AudioService>) -> Result<String, String> {
    audio
        .recordings_dir()
        .map(|path| path.to_string_lossy().to_string())
        .ok_or_else(|| "Recordings directory not initialized".to_string())
}

/// Open the recordings directory in the system file explorer.
#[tauri::command]
pub fn open_recordings_dir(
    app: tauri::AppHandle,
    audio: tauri::State<'_, AudioService>,
) -> Result<(), String> {
    let dir = audio
        .recordings_dir()
        .ok_or_else(|| "Recordings directory not initialized".to_string())?;
    if !dir.exists() {
        return Err("Recordings directory does not exist".to_string());
    }
    app.opener()
        .open_path(dir.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}
