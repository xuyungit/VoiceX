use serde_json::json;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter};

use crate::hud::{
    self, HudWidth, BATCH_HUD_HEIGHT, BATCH_HUD_WIDTH, CAPTION_HUD_HEIGHT, CAPTION_HUD_WIDTH,
    STREAM_HUD_HEIGHT, STREAM_HUD_WIDTH,
};

/// How long the page takes to fade the capsule out (`hud.ts`, `HUD_EXIT_MS`)
/// before the window is ordered out.
const HUD_EXIT_MS: u64 = 180;
/// How long the check mark stays after the text is typed, before the exit.
const HUD_INSERTED_HOLD_MS: u64 = 320;
use crate::state::{ProcessingIntent, RecordingStyle};
use crate::tts::SpeechProgress;

/// Which layout the HUD window shows. Chosen when the window is shown and
/// fixed for that session: the window is sized to match, and resizing it
/// mid-session would jump under the user's eyes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudPresentation {
    /// Streaming dictation: the live transcript in a capsule that grows
    /// with it, up to half the screen width.
    Stream,
    /// Batch dictation and reads without captions: the compact capsule,
    /// where the waveform or the reading icon is all there is to show.
    Batch,
    /// A read with captions: the sentence being spoken, drawn large, and
    /// nothing else — no icon, no chip, no bars.
    Caption,
}

impl HudPresentation {
    pub fn as_str(self) -> &'static str {
        match self {
            HudPresentation::Stream => "stream",
            HudPresentation::Batch => "batch",
            HudPresentation::Caption => "caption",
        }
    }

    /// Window size: width as a rule for the target screen, height in
    /// logical points.
    fn bounds(self) -> (HudWidth, f64) {
        match self {
            HudPresentation::Stream => (STREAM_HUD_WIDTH, STREAM_HUD_HEIGHT),
            HudPresentation::Batch => (BATCH_HUD_WIDTH, BATCH_HUD_HEIGHT),
            HudPresentation::Caption => (CAPTION_HUD_WIDTH, CAPTION_HUD_HEIGHT),
        }
    }
}

/// HUD helper to centralize window show/hide and event emissions.
#[derive(Clone)]
pub struct HudService {
    app_handle: AppHandle,
    hide_timer: std::sync::Arc<std::sync::Mutex<Option<JoinHandle<()>>>>,
    /// Bumped by every show and every exit. An exit hides the window only if
    /// nothing has shown it since; the lock orders that check against a show.
    window_generation: std::sync::Arc<std::sync::Mutex<u64>>,
    snapshot: std::sync::Arc<std::sync::Mutex<HudSnapshot>>,
}

/// What a selected-text read is doing, as the HUD shows it.
///
/// The two phases exist because a cloud provider takes 300–700 ms to produce
/// its first audio (plan §5.4). Without something on screen, that gap is
/// indistinguishable from the hotkey not having worked at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingPhase {
    /// Reading the selection and waiting for the engine's first audio.
    Preparing,
    /// The LLM stage (translation or preprocessing) is in flight. Shown
    /// distinctly because it is the one wait that can run for seconds.
    Translating,
    /// Audio is actually coming out.
    Speaking,
}

impl ReadingPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadingPhase::Preparing => "preparing",
            ReadingPhase::Translating => "translating",
            ReadingPhase::Speaking => "speaking",
        }
    }
}

/// Which reading feature owns the session; the HUD chip names it so a user
/// can tell "翻译朗读" from "朗读" at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingKind {
    Read,
    Translate,
}

impl ReadingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadingKind::Read => "read",
            ReadingKind::Translate => "translate",
        }
    }
}

/// Where the text being read came from. The chip says "剪贴板" for the
/// clipboard, because a read of the clipboard looks exactly like a read of
/// the selection otherwise, and the two can hold different text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingSource {
    Selection,
    Clipboard,
}

impl ReadingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadingSource::Selection => "selection",
            ReadingSource::Clipboard => "clipboard",
        }
    }
}

#[derive(Default, Clone)]
struct HudSnapshot {
    events: std::collections::BTreeMap<&'static str, serde_json::Value>,
}

impl HudService {
    pub fn new(app_handle: AppHandle) -> Self {
        Self {
            app_handle,
            hide_timer: std::sync::Arc::new(std::sync::Mutex::new(None)),
            window_generation: std::sync::Arc::new(std::sync::Mutex::new(0)),
            snapshot: std::sync::Arc::new(std::sync::Mutex::new(HudSnapshot::default())),
        }
    }

    pub fn show(&self, presentation: HudPresentation) {
        // Cancels an exit still fading out: this session keeps the window.
        *self
            .window_generation
            .lock()
            .expect("HUD window generation mutex poisoned") += 1;
        // Recreate/position in case display changed.
        if let Err(err) = hud::create_hud_window(&self.app_handle) {
            log::warn!("Failed to create HUD window: {}", err);
        }
        // Sized for its screen before it is shown: resizing a visible window
        // flashes it.
        self.sync_bounds(presentation);
        // The page takes the presentation as its cue to bring the capsule in.
        self.emit_presentation_mode(presentation);
        hud::show_hud(&self.app_handle);
    }

    /// Fade the capsule out, then order the window out.
    ///
    /// WebKit paints nothing while the window is hidden, so the frame it last
    /// painted is what the next show starts from. Ordered out at once, that
    /// was the previous session's capsule, shown for a frame before the new
    /// one replaced it; after the fade it is an empty frame.
    pub fn hide(&self) {
        self.exit_after(0);
    }

    /// The text is typed: show the check mark, then leave.
    pub fn finish_inserted(&self) {
        let _ = self.app_handle.emit_to("hud", "state:inserted", json!({}));
        self.exit_after(HUD_INSERTED_HOLD_MS);
    }

    /// Start the fade `delay_ms` from now, and order the window out once it
    /// has played.
    ///
    /// Every step checks the window generation under its lock and acts while
    /// still holding it, so a `show()` (which bumps the generation) lands
    /// either before a step, which then stands down, or after it, and its
    /// presentation event reaches the page after this exit and cancels it.
    /// Checking and then acting outside the lock would let a stale exit reach
    /// the page after the new session's presentation, and leave its capsule
    /// transparent.
    fn exit_after(&self, delay_ms: u64) {
        let generation = {
            let mut guard = self
                .window_generation
                .lock()
                .expect("HUD window generation mutex poisoned");
            *guard += 1;
            // An immediate exit is sent before this returns, so it reaches
            // the page ahead of the state reset the caller emits next: the
            // page freezes what it shows during the fade.
            if delay_ms == 0 {
                let _ = self.app_handle.emit_to("hud", "hud:exit", json!({}));
            }
            *guard
        };
        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            let lock = || {
                this.window_generation
                    .lock()
                    .expect("HUD window generation mutex poisoned")
            };
            if delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                let guard = lock();
                if *guard != generation {
                    return;
                }
                let _ = this.app_handle.emit_to("hud", "hud:exit", json!({}));
            }
            tokio::time::sleep(std::time::Duration::from_millis(HUD_EXIT_MS)).await;
            let guard = lock();
            if *guard == generation {
                hud::hide_hud(&this.app_handle);
            }
        });
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

    /// `committed_len` is the byte length of the prefix of `text` the
    /// provider will not revise; the HUD draws the rest as still changing.
    pub fn emit_transcript(&self, text: &str, committed_len: usize, is_final: bool) {
        let payload = json!({
            "text": text,
            "isFinal": is_final,
            // In UTF-16 code units, which is how the page indexes strings.
            "committedLength": text[..committed_len].encode_utf16().count(),
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

    pub fn emit_presentation_mode(&self, presentation: HudPresentation) {
        let payload = json!({
            "mode": presentation.as_str()
        });
        self.cache_event("state:hud_presentation", &payload);
        let _ = self
            .app_handle
            .emit_to("hud", "state:hud_presentation", payload);
    }

    pub fn sync_bounds(&self, presentation: HudPresentation) {
        let (width, height) = presentation.bounds();
        if let Err(err) = hud::set_hud_size(&self.app_handle, width, height) {
            log::warn!("Failed to size HUD window: {}", err);
        }
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

    /// Report the state of a read. `None` means it ended.
    pub fn emit_reading(
        &self,
        kind: ReadingKind,
        source: ReadingSource,
        phase: Option<ReadingPhase>,
    ) {
        let payload = json!({
            "phase": phase.map(ReadingPhase::as_str),
            "kind": kind.as_str(),
            "source": source.as_str(),
        });
        self.cache_event("state:reading", &payload);
        let _ = self.app_handle.emit("state:reading", payload.clone());
        let _ = self.app_handle.emit_to("hud", "state:reading", payload);
    }

    /// The sentence being spoken, for the caption presentation. `None` clears
    /// it: before the first piece is audible the HUD shows the phase
    /// placeholder instead, and after the read nothing.
    pub fn emit_caption(&self, caption: Option<&SpeechProgress>) {
        let payload = json!({
            "text": caption.map(|c| c.text.as_str()),
            "index": caption.map(|c| c.index),
            "total": caption.map(|c| c.total),
        });
        self.cache_event("state:caption", &payload);
        let _ = self.app_handle.emit_to("hud", "state:caption", payload);
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

#[cfg(test)]
mod tests {
    use super::{HudPresentation, ReadingKind, ReadingPhase, ReadingSource};

    #[test]
    fn reading_phase_tokens_match_what_the_hud_switches_on() {
        // `hud.ts` compares these strings literally; renaming one here without
        // renaming it there leaves the HUD stuck on its previous state with no
        // error anywhere.
        assert_eq!(ReadingPhase::Preparing.as_str(), "preparing");
        assert_eq!(ReadingPhase::Translating.as_str(), "translating");
        assert_eq!(ReadingPhase::Speaking.as_str(), "speaking");
        assert_eq!(ReadingKind::Read.as_str(), "read");
        assert_eq!(ReadingKind::Translate.as_str(), "translate");
        assert_eq!(ReadingSource::Selection.as_str(), "selection");
        assert_eq!(ReadingSource::Clipboard.as_str(), "clipboard");
        assert_eq!(HudPresentation::Stream.as_str(), "stream");
        assert_eq!(HudPresentation::Batch.as_str(), "batch");
        assert_eq!(HudPresentation::Caption.as_str(), "caption");
    }
}
