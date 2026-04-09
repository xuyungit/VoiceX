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
    snapshot: std::sync::Arc<std::sync::Mutex<HudSnapshot>>,
}

const STREAM_HUD_WIDTH: f64 = 256.0;
const STREAM_HUD_HEIGHT: f64 = 100.0;
const BATCH_HUD_WIDTH: f64 = 204.0;
const BATCH_HUD_HEIGHT: f64 = 78.0;

#[derive(Default, Clone)]
struct HudSnapshot {
    events: std::collections::BTreeMap<&'static str, serde_json::Value>,
}

impl HudService {
    pub fn new(app_handle: AppHandle) -> Self {
        Self {
            app_handle,
            hide_timer: std::sync::Arc::new(std::sync::Mutex::new(None)),
            snapshot: std::sync::Arc::new(std::sync::Mutex::new(HudSnapshot::default())),
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

    pub fn replay_snapshot(&self) {
        let snapshot = self.snapshot.lock().ok().map(|guard| guard.clone());
        let Some(snapshot) = snapshot else {
            return;
        };

        for (event_name, payload) in snapshot.events {
            let _ = self.app_handle.emit_to("hud", event_name, payload);
        }
    }

    fn cache_event(&self, event_name: &'static str, payload: &serde_json::Value) {
        if let Ok(mut guard) = self.snapshot.lock() {
            guard.events.insert(event_name, payload.clone());
        }
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
        self.cache_event("asr:event", &payload);
        let _ = self.app_handle.emit("asr:event", payload.clone());
        let _ = self.app_handle.emit_to("hud", "asr:event", payload);
    }

    pub fn clear_transcript(&self) {
        let payload = json!({
            "text": "",
            "isFinal": false,
            "clear": true,
        });
        self.cache_event("asr:event", &payload);
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

        self.cache_event("state:countdown", &payload);
        let _ = self.app_handle.emit("state:countdown", payload.clone());
        let _ = self.app_handle.emit_to("hud", "state:countdown", payload);
    }

    pub fn emit_recording_style(&self, style: Option<RecordingStyle>, is_batch: bool) {
        let style_str = match style {
            Some(RecordingStyle::PushToTalk) => Some("push_to_talk"),
            Some(RecordingStyle::HandsFree) => Some("hands_free"),
            None => None,
        };

        let payload = json!({ "style": style_str, "batch": is_batch });
        self.cache_event("state:recording_style", &payload);
        let _ = self.app_handle.emit("state:recording_style", payload);
    }

    pub fn emit_presentation_mode(&self, is_batch: bool) {
        let payload = json!({
            "mode": if is_batch { "batch" } else { "stream" }
        });
        self.cache_event("state:hud_presentation", &payload);
        let _ = self
            .app_handle
            .emit_to("hud", "state:hud_presentation", payload);
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
        let payload = json!({ "is_correcting": is_correcting });
        self.cache_event("state:correcting", &payload);
        let _ = self.app_handle.emit("state:correcting", payload);
    }

    pub fn emit_recognizing(&self, is_recognizing: bool) {
        let payload = json!({ "is_recognizing": is_recognizing });
        self.cache_event("state:recognizing", &payload);
        let _ = self.app_handle.emit("state:recognizing", payload);
    }

    pub fn emit_recognition_stopped(&self) {
        let payload = json!({});
        self.cache_event("recognition:stopped", &payload);
        let _ = self.app_handle.emit("recognition:stopped", payload);
    }

    pub fn emit_audio_level(&self, level: f32) {
        let payload = json!({
            "level": level.clamp(0.0, 1.0)
        });
        self.cache_event("state:audio_level", &payload);
        let _ = self.app_handle.emit_to("hud", "state:audio_level", payload);
    }

    pub fn emit_audio_spectrum(&self, bands: &[f32]) {
        let payload = json!({
            "bands": bands
        });
        self.cache_event("state:audio_spectrum", &payload);
        let _ = self
            .app_handle
            .emit_to("hud", "state:audio_spectrum", payload);
    }

    pub fn emit_intent(&self, intent: ProcessingIntent) {
        let payload = json!({ "intent": intent.as_str() });
        self.cache_event("state:intent", &payload);
        let _ = self.app_handle.emit("state:intent", payload);
    }

    pub fn emit_recognition(&self, event_name: &str, payload: serde_json::Value) {
        let _ = self.app_handle.emit(event_name, payload.clone());
        let _ = self.app_handle.emit_to("hud", event_name, payload);
    }

    pub fn emit_error(&self, message: Option<&str>) {
        let payload = json!({
            "message": message,
        });
        self.cache_event("state:error", &payload);
        let _ = self.app_handle.emit("state:error", payload.clone());
        let _ = self.app_handle.emit_to("hud", "state:error", payload);
    }

    /// Reset HUD-visible state to a neutral baseline.
    pub fn reset_display(&self) {
        self.emit_countdown(None);
        self.emit_correcting(false);
        self.emit_intent(ProcessingIntent::Assistant);
        self.emit_error(None);
        self.clear_transcript();
        self.emit_audio_level(0.0);
        self.emit_audio_spectrum(&[]);
    }
}
