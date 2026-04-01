use serde_json::json;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter};

use crate::{
    hud,
    state::{ProcessingIntent, RecordingStyle},
};

/// HUD helper to centralize window show/hide and event emissions.
#[derive(Clone)]
pub struct HudService {
    app_handle: AppHandle,
    hide_timer: std::sync::Arc<std::sync::Mutex<Option<JoinHandle<()>>>>,
}

const STREAM_HUD_WIDTH: f64 = 256.0;
const STREAM_HUD_HEIGHT: f64 = 100.0;
const BATCH_HUD_WIDTH: f64 = 204.0;
const BATCH_HUD_HEIGHT: f64 = 78.0;

impl HudService {
    pub fn new(app_handle: AppHandle) -> Self {
        Self {
            app_handle,
            hide_timer: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn show(&self, is_batch: bool) {
        // Recreate/position in case display changed.
        if let Err(err) = hud::create_hud_window(&self.app_handle) {
            log::warn!("Failed to create HUD window: {}", err);
        }
        self.emit_presentation_mode(is_batch);
        self.sync_bounds(is_batch);
        hud::show_hud(&self.app_handle);
    }

    pub fn hide(&self) {
        hud::hide_hud(&self.app_handle);
    }

    /// Schedule a hide after delay_ms. Cancels any previous hide timer.
    pub fn schedule_hide(&self, delay_ms: u64, on_ready: impl FnOnce() + Send + 'static) {
        self.cancel_hide();

        let hide_handle = self.hide_timer.clone();
        let handle = tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            on_ready();
        });

        let _ = hide_handle.lock().map(|mut guard| *guard = Some(handle));
    }

    /// Cancel pending hide timer, if any.
    pub fn cancel_hide(&self) {
        if let Ok(mut guard) = self.hide_timer.lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
            }
        }
    }

    pub fn emit_transcript(&self, text: &str, is_final: bool) {
        let payload = json!({
            "text": text,
            "isFinal": is_final
        });
        let _ = self.app_handle.emit("asr:event", payload.clone());
        let _ = self.app_handle.emit_to("hud", "asr:event", payload);
    }

    pub fn emit_countdown(&self, seconds: Option<u32>) {
        let payload = json!({
            "seconds": seconds
        });

        if let Some(value) = seconds {
            log::debug!("Countdown: {}s remaining", value);
        }

        let _ = self.app_handle.emit("state:countdown", payload.clone());
        let _ = self.app_handle.emit_to("hud", "state:countdown", payload);
    }

    pub fn emit_recording_style(&self, style: Option<RecordingStyle>, is_batch: bool) {
        let style_str = match style {
            Some(RecordingStyle::PushToTalk) => Some("push_to_talk"),
            Some(RecordingStyle::HandsFree) => Some("hands_free"),
            None => None,
        };

        let _ = self.app_handle.emit(
            "state:recording_style",
            json!({ "style": style_str, "batch": is_batch }),
        );
    }

    pub fn emit_presentation_mode(&self, is_batch: bool) {
        let payload = json!({
            "mode": if is_batch { "batch" } else { "stream" }
        });
        let _ = self.app_handle.emit_to("hud", "state:hud_presentation", payload);
    }

    pub fn sync_bounds(&self, is_batch: bool) {
        let (width, height) = if is_batch {
            (BATCH_HUD_WIDTH, BATCH_HUD_HEIGHT)
        } else {
            (STREAM_HUD_WIDTH, STREAM_HUD_HEIGHT)
        };
        let _ = hud::set_hud_content_bounds(&self.app_handle, width, height);
    }

    pub fn emit_correcting(&self, is_correcting: bool) {
        let _ = self.app_handle.emit(
            "state:correcting",
            json!({ "is_correcting": is_correcting }),
        );
    }

    pub fn emit_recognizing(&self, is_recognizing: bool) {
        let _ = self.app_handle.emit(
            "state:recognizing",
            json!({ "is_recognizing": is_recognizing }),
        );
    }

    pub fn emit_recognition_stopped(&self) {
        let _ = self.app_handle.emit("recognition:stopped", json!({}));
    }

    pub fn emit_audio_level(&self, level: f32) {
        let payload = json!({
            "level": level.clamp(0.0, 1.0)
        });
        let _ = self.app_handle.emit_to("hud", "state:audio_level", payload);
    }

    pub fn emit_audio_spectrum(&self, bands: &[f32]) {
        let payload = json!({
            "bands": bands
        });
        let _ = self
            .app_handle
            .emit_to("hud", "state:audio_spectrum", payload);
    }

    pub fn emit_intent(&self, intent: ProcessingIntent) {
        let _ = self
            .app_handle
            .emit("state:intent", json!({ "intent": intent.as_str() }));
    }

    pub fn emit_recognition(&self, event_name: &str, payload: serde_json::Value) {
        let _ = self.app_handle.emit(event_name, payload.clone());
        let _ = self.app_handle.emit_to("hud", event_name, payload);
    }

    /// Reset HUD-visible state to a neutral baseline.
    pub fn reset_display(&self) {
        self.emit_countdown(None);
        self.emit_correcting(false);
        self.emit_intent(ProcessingIntent::Assistant);
        self.emit_transcript("", false);
        self.emit_audio_level(0.0);
        self.emit_audio_spectrum(&[]);
    }
}
