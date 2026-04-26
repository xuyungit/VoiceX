//! Session controller: orchestrates hotkey-driven state machine, HUD lifecycle,
//! and timers for push-to-talk / hands-free modes.
mod handlers;
mod message;
mod utils;

pub use message::{CancelReason, SessionMessage};

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use crate::{
    injector::TextInjectionMode,
    services::{
        audio_manager::AudioManager,
        countdown_service::CountdownService,
        hud_service::HudService,
        llm_service::{LlmCorrectionResult, LlmService},
    },
    state::{AppState, HotkeySessionState, ProcessingIntent, TranslationTriggerMode},
};

const FINALIZE_HIDE_DELAY_MS: u64 = 1200;
const ERROR_HIDE_DELAY_MS: u64 = 2500;
const MIN_RECORDING_MS: u64 = 800;
const COUNTDOWN_THRESHOLD_SECS: u64 = 30;
const MIN_CORRECTION_CHARS: usize = 5;
const ASR_FINAL_WAIT_MS: u64 = 15_000;
const ASR_STARTUP_RETRY_MAX_ATTEMPTS: u32 = 3;
const ASR_RECONNECT_MAX_ATTEMPTS: u32 = 2;
const ASR_AUDIO_REPLAY_MAX_CHUNKS: usize = 600;
const ASR_REPLAY_OVERLAP_CHUNKS: usize = 5;

#[derive(Clone, Default)]
struct AsrAudioBridge {
    inner: Arc<Mutex<AsrAudioBridgeInner>>,
}

#[derive(Default)]
struct AsrAudioBridgeInner {
    current_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
    replay_buffer: VecDeque<Vec<u8>>,
    closed: bool,
}

impl AsrAudioBridge {
    fn push_chunk(&self, chunk: Vec<u8>) {
        let mut inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(_) => return,
        };

        inner.replay_buffer.push_back(chunk.clone());
        while inner.replay_buffer.len() > ASR_AUDIO_REPLAY_MAX_CHUNKS {
            inner.replay_buffer.pop_front();
        }

        if let Some(tx) = inner.current_tx.as_mut() {
            if tx.try_send(chunk).is_err() {
                log::debug!("ASR audio bridge dropped a live chunk while forwarding");
            }
        }
    }

    fn attach_stream(&self) -> tokio::sync::mpsc::Receiver<Vec<u8>> {
        let mut inner = self.inner.lock().expect("asr audio bridge lock");
        let capacity = (inner.replay_buffer.len() + 64).clamp(64, 1024);
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        for chunk in &inner.replay_buffer {
            let _ = tx.try_send(chunk.clone());
        }
        if inner.closed {
            drop(tx);
            inner.current_tx = None;
        } else {
            inner.current_tx = Some(tx);
        }
        rx
    }

    fn close(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.closed = true;
            inner.current_tx = None;
        }
    }

    fn clear_replay_buffer(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            if inner.replay_buffer.len() <= ASR_REPLAY_OVERLAP_CHUNKS {
                return;
            }
            let split_at = inner
                .replay_buffer
                .len()
                .saturating_sub(ASR_REPLAY_OVERLAP_CHUNKS);
            inner.replay_buffer.drain(..split_at);
        }
    }
}

/// Serializes session actions onto a single async task and owns AppState (actor style).
#[derive(Clone)]
pub struct SessionCoordinator {
    sender: UnboundedSender<SessionMessage>,
}

impl SessionCoordinator {
    pub fn new(controller: SessionController) -> Self {
        let (tx, mut rx) = unbounded_channel::<SessionMessage>();
        controller.attach_sender(tx.clone());
        tauri::async_runtime::spawn(async move {
            // Own the state inside the loop to avoid external locking.
            let mut state = AppState::new();
            while let Some(msg) = rx.recv().await {
                controller.handle_message(&mut state, msg);
            }
        });

        Self { sender: tx }
    }

    pub fn send(&self, msg: SessionMessage) {
        let _ = self.sender.send(msg);
    }
}

#[derive(Clone)]
pub struct SessionController {
    hud_service: Arc<Mutex<Option<HudService>>>,
    audio_manager: Arc<Mutex<Option<AudioManager>>>,
    countdown_service: Arc<Mutex<Option<CountdownService>>>,
    app_handle: Arc<Mutex<Option<AppHandle>>>,
    coordinator_tx: Arc<Mutex<Option<UnboundedSender<SessionMessage>>>>,
    hold_timer: Arc<Mutex<Option<JoinHandle<()>>>>,
    asr_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    asr_audio_bridge: Arc<Mutex<Option<AsrAudioBridge>>>,
    asr_audio_bridge_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    audio_level_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    asr_cancel_token: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
    asr_final_timeout: Arc<Mutex<Option<JoinHandle<()>>>>,
    injection_epoch: Arc<AtomicU64>,
    injection_cancel_flag: Arc<AtomicBool>,
    audio_epoch: Arc<AtomicU64>,
}

impl SessionController {
    pub fn new() -> Self {
        Self::default()
    }

    fn attach_sender(&self, sender: UnboundedSender<SessionMessage>) {
        if let Ok(mut guard) = self.coordinator_tx.lock() {
            *guard = Some(sender);
        }
    }

    fn send_message(&self, msg: SessionMessage) {
        if let Ok(guard) = self.coordinator_tx.lock() {
            if let Some(tx) = guard.as_ref() {
                let _ = tx.send(msg);
            }
        }
    }

    /// Attach the running app handle so background threads can emit events.
    pub fn init_with_handle(&self, app: &AppHandle) {
        if let Ok(mut guard) = self.hud_service.lock() {
            *guard = Some(HudService::new(app.clone()));
        }
        if let Ok(mut guard) = self.audio_manager.lock() {
            *guard = Some(AudioManager::new(app.clone()));
        }
        if let Ok(mut guard) = self.countdown_service.lock() {
            *guard = Some(CountdownService::new());
        }
        if let Ok(mut guard) = self.app_handle.lock() {
            *guard = Some(app.clone());
        }
    }

    /// Update runtime settings that affect timers and restart hands-free countdowns if needed.
    pub fn apply_settings(
        &self,
        hold_threshold_ms: u32,
        max_recording_minutes: u32,
        text_injection_mode: &str,
        text_injection_overrides: Vec<crate::foreground_app::TextInjectionAppOverride>,
        input_device_uid: Option<String>,
        remove_trailing_punctuation: bool,
        short_sentence_threshold: u32,
        replacement_rules: Vec<crate::commands::settings::ReplacementRule>,
        translation_enabled: bool,
        translation_trigger_mode: &str,
        double_tap_window_ms: u32,
    ) {
        // If coordinator is attached, route through message loop; otherwise apply directly (startup).
        if self
            .coordinator_tx
            .lock()
            .ok()
            .and_then(|g| g.as_ref().cloned())
            .is_some()
        {
            self.send_message(SessionMessage::ApplySettings {
                hold_threshold_ms,
                max_recording_minutes,
                text_injection_mode: text_injection_mode.to_string(),
                text_injection_overrides,
                input_device_uid,
                remove_trailing_punctuation,
                short_sentence_threshold,
                replacement_rules,
                translation_enabled,
                translation_trigger_mode: translation_trigger_mode.to_string(),
                double_tap_window_ms,
            });
            return;
        }

        // If coordinator not attached, apply directly (should be rare).
        let mut state = AppState::new();
        self.handle_apply_settings_state(
            &mut state,
            hold_threshold_ms,
            max_recording_minutes,
            text_injection_mode,
            text_injection_overrides,
            input_device_uid.as_deref(),
            remove_trailing_punctuation,
            short_sentence_threshold,
            replacement_rules,
            translation_enabled,
            translation_trigger_mode,
            double_tap_window_ms,
        );
    }

    fn handle_apply_settings_state(
        &self,
        state: &mut AppState,
        hold_threshold_ms: u32,
        max_recording_minutes: u32,
        text_injection_mode: &str,
        text_injection_overrides: Vec<crate::foreground_app::TextInjectionAppOverride>,
        input_device_uid: Option<&str>,
        remove_trailing_punctuation: bool,
        short_sentence_threshold: u32,
        replacement_rules: Vec<crate::commands::settings::ReplacementRule>,
        translation_enabled: bool,
        translation_trigger_mode: &str,
        double_tap_window_ms: u32,
    ) {
        state.hold_threshold_ms = hold_threshold_ms as u64;
        state.max_recording_minutes = max_recording_minutes;
        state.text_injection_mode = TextInjectionMode::from_str(text_injection_mode);
        state.text_injection_overrides = text_injection_overrides;
        state.remove_trailing_punctuation = remove_trailing_punctuation;
        state.short_sentence_threshold = short_sentence_threshold;
        state.replacement_rules = replacement_rules;
        state.translation_enabled = translation_enabled;
        state.translation_trigger_mode = TranslationTriggerMode::from_str(translation_trigger_mode);
        state.double_tap_window_ms = double_tap_window_ms as u64;
        if !state.translation_enabled
            || state.translation_trigger_mode == TranslationTriggerMode::Off
        {
            state.pending_translation_upgrade = false;
            state.double_tap_upgrade_deadline = None;
        }
        let should_restart_timeout =
            state.session_state == HotkeySessionState::HandsFree && state.is_recording;

        // Always cancel existing countdowns so new limits take effect immediately.
        self.cancel_recording_timeout();

        if should_restart_timeout {
            self.start_recording_timeout(self.effective_max_recording_minutes(state));
        }

        if let Some(manager) = self.audio_manager() {
            if let Err(err) = manager.set_preferred_device(input_device_uid.map(|s| s.to_string()))
            {
                log::warn!("Failed to set preferred audio device: {}", err);
            }
        }
    }

    /// Message dispatcher: update state inside the coordinator loop.
    fn handle_message(&self, state: &mut AppState, msg: SessionMessage) {
        match msg {
            SessionMessage::HotkeyPressed => self.on_hotkey_pressed(state),
            SessionMessage::HotkeyReleased => self.on_hotkey_released(state),
            SessionMessage::HoldThresholdReached => self.on_hold_threshold_reached_state(state),
            SessionMessage::CancelSession(reason) => {
                if state.session_state == HotkeySessionState::Idle
                    && !state.is_recording
                    && !state.is_correcting
                {
                    log::debug!("Cancel ignored in idle state: {:?}", reason);
                    self.set_escape_swallowing(false);
                } else {
                    self.handle_cancel_session_state(state, reason)
                }
            }
            SessionMessage::RecordingCountdownTick(remaining) => {
                self.emit_countdown(Some(remaining))
            }
            SessionMessage::RecordingTimeout => self.on_recording_timeout_state(state),
            SessionMessage::FinalizeHideReady => self.on_finalize_hide_ready_state(state),
            SessionMessage::ErrorDisplayDone => self.on_error_display_done_state(state),
            SessionMessage::AsrFinalTimeout => self.on_asr_final_timeout(state),
            SessionMessage::AsrEvent(evt) => {
                if state.session_state == HotkeySessionState::Idle
                    && !state.is_recording
                    && !state.is_correcting
                {
                    log::debug!("Dropping stale ASR event after cancel");
                    return;
                }
                self.handle_asr_event_state(state, evt)
            }
            SessionMessage::AsrStreamFinished { error } => {
                if state.session_state == HotkeySessionState::Idle
                    && !state.is_recording
                    && !state.is_correcting
                {
                    log::debug!("Dropping stale ASR stream-finished after cancel");
                    return;
                }
                if let Some(reason) = error {
                    self.on_asr_stream_failed_state(state, reason);
                } else {
                    self.on_asr_stream_finished_state(state)
                }
            }
            SessionMessage::CorrectingStart => self.on_correcting_start(state),
            SessionMessage::CorrectingStop => self.on_correcting_stop(state),
            SessionMessage::ApplySettings {
                hold_threshold_ms,
                max_recording_minutes,
                text_injection_mode,
                text_injection_overrides,
                input_device_uid,
                remove_trailing_punctuation,
                short_sentence_threshold,
                replacement_rules,
                translation_enabled,
                translation_trigger_mode,
                double_tap_window_ms,
            } => self.handle_apply_settings_state(
                state,
                hold_threshold_ms,
                max_recording_minutes,
                &text_injection_mode,
                text_injection_overrides,
                input_device_uid.as_deref(),
                remove_trailing_punctuation,
                short_sentence_threshold,
                replacement_rules,
                translation_enabled,
                &translation_trigger_mode,
                double_tap_window_ms,
            ),
            SessionMessage::RetryAsrStartup => self.on_retry_asr_startup_state(state),
            SessionMessage::RetryAsrReconnect => self.on_retry_asr_reconnect_state(state),
            SessionMessage::AudioStarted {
                audio_epoch,
                sample_rate,
                channels,
                path,
            } => {
                if !self.is_current_audio_epoch(audio_epoch) {
                    log::debug!(
                        "Dropping stale AudioStarted (epoch={}, current={})",
                        audio_epoch,
                        self.current_audio_epoch()
                    );
                    return;
                }
                self.handle_audio_started_state(state, sample_rate, channels, path)
            }
            SessionMessage::AudioStopped {
                audio_epoch,
                path,
                refinement_path,
                duration_ms,
            } => {
                if !self.is_current_audio_epoch(audio_epoch) {
                    log::debug!(
                        "Dropping stale AudioStopped (epoch={}, current={})",
                        audio_epoch,
                        self.current_audio_epoch()
                    );
                    return;
                }
                self.handle_audio_stopped_state(state, path, refinement_path, duration_ms)
            }
            SessionMessage::AudioStartFailed {
                audio_epoch,
                reason,
            } => {
                if !self.is_current_audio_epoch(audio_epoch) {
                    log::debug!(
                        "Dropping stale AudioStartFailed (epoch={}, current={})",
                        audio_epoch,
                        self.current_audio_epoch()
                    );
                    return;
                }
                self.on_audio_start_failed_state(state, reason)
            }
            SessionMessage::AudioStopFailed {
                audio_epoch,
                reason,
            } => {
                if !self.is_current_audio_epoch(audio_epoch) {
                    log::debug!(
                        "Dropping stale AudioStopFailed (epoch={}, current={})",
                        audio_epoch,
                        self.current_audio_epoch()
                    );
                    return;
                }
                self.on_audio_stop_failed_state(state, reason)
            }
            SessionMessage::BatchAsrDone {
                text,
                model_name,
                batch_epoch,
            } => {
                if self.injection_epoch.load(Ordering::SeqCst) != batch_epoch {
                    log::debug!(
                        "Dropping stale batch ASR result after epoch change (epoch={}, current={})",
                        batch_epoch,
                        self.injection_epoch.load(Ordering::SeqCst)
                    );
                    return;
                }
                if state.session_state == HotkeySessionState::Idle
                    && !state.is_recording
                    && !state.is_correcting
                {
                    log::debug!("Dropping stale batch ASR result in idle state");
                    return;
                }
                self.on_batch_asr_done_state(state, text, model_name)
            }
            SessionMessage::BatchAsrFailed {
                reason,
                batch_epoch,
            } => {
                if self.injection_epoch.load(Ordering::SeqCst) != batch_epoch {
                    log::debug!(
                        "Dropping stale batch ASR failure after epoch change (epoch={}, current={})",
                        batch_epoch,
                        self.injection_epoch.load(Ordering::SeqCst)
                    );
                    return;
                }
                if state.session_state == HotkeySessionState::Idle
                    && !state.is_recording
                    && !state.is_correcting
                {
                    log::debug!("Dropping stale batch ASR failure in idle state");
                    return;
                }
                self.on_batch_asr_failed_state(state, reason)
            }
            SessionMessage::AsrRefinementDone {
                text,
                model_name,
                refinement_epoch,
            } => {
                if self.injection_epoch.load(Ordering::SeqCst) != refinement_epoch {
                    log::debug!(
                        "Dropping stale ASR refinement result after epoch change (epoch={}, current={})",
                        refinement_epoch,
                        self.injection_epoch.load(Ordering::SeqCst)
                    );
                    return;
                }
                if state.session_state == HotkeySessionState::Idle
                    && !state.is_recording
                    && !state.is_correcting
                {
                    log::debug!("Dropping stale ASR refinement result in idle state");
                    return;
                }
                self.on_asr_refinement_done_state(state, text, model_name)
            }
            SessionMessage::AsrRefinementFailed {
                reason,
                refinement_epoch,
            } => {
                if self.injection_epoch.load(Ordering::SeqCst) != refinement_epoch {
                    log::debug!(
                        "Dropping stale ASR refinement failure after epoch change (epoch={}, current={})",
                        refinement_epoch,
                        self.injection_epoch.load(Ordering::SeqCst)
                    );
                    return;
                }
                if state.session_state == HotkeySessionState::Idle
                    && !state.is_recording
                    && !state.is_correcting
                {
                    log::debug!("Dropping stale ASR refinement failure in idle state");
                    return;
                }
                self.on_asr_refinement_failed_state(state, reason)
            }
            SessionMessage::InjectDone {
                text,
                corrected,
                llm_invoked,
                recording_style,
                duration_ms,
                audio_path,
                original_for_history,
                injection_version,
                intent,
            } => self.handle_inject_done_state(
                state,
                text,
                corrected,
                llm_invoked,
                recording_style,
                duration_ms,
                audio_path,
                original_for_history,
                injection_version,
                intent,
            ),
        }
    }

    /// Hotkey pressed -> enter Pending, arm hold timer, show HUD.
    pub fn handle_hotkey_pressed(&self) {
        self.send_message(SessionMessage::HotkeyPressed);
    }

    /// Hotkey released -> resolve to hands-free or finalize depending on mode.
    pub fn handle_hotkey_released(&self) {
        self.send_message(SessionMessage::HotkeyReleased);
    }

    /// ESC or manual cancel -> abort current session.
    pub fn handle_escape_pressed(&self) {
        self.send_message(SessionMessage::CancelSession(CancelReason::EscapeKey));
    }

    /// Called when long-press threshold is reached while still held down.
    pub fn on_hold_threshold_reached(&self) {
        self.send_message(SessionMessage::HoldThresholdReached);
    }

    /// Apply LLM correction if enabled; returns LLM invocation details.
    async fn correct_text_if_enabled(
        &self,
        text: &str,
        intent: ProcessingIntent,
    ) -> LlmCorrectionResult {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return LlmCorrectionResult {
                text: text.to_string(),
                invoked: false,
                changed: false,
            };
        }
        if intent == ProcessingIntent::Assistant && trimmed.chars().count() <= MIN_CORRECTION_CHARS
        {
            log::info!(
                "LLM correction skipped for short text (len <= {}): {}",
                MIN_CORRECTION_CHARS,
                trimmed
            );
            return LlmCorrectionResult {
                text: text.to_string(),
                invoked: false,
                changed: false,
            };
        }

        // Reflect correcting flag via coordinator-managed state.
        self.send_message(SessionMessage::CorrectingStart);
        // Keep HUD text visible during correction by re-emitting the last transcript.
        self.emit_transcript(trimmed, true);

        let service = LlmService::new();
        let result = service.correct_text_if_enabled(trimmed, intent).await;
        if result.changed {
            log::info!("LLM correction applied before injection");
        }

        self.send_message(SessionMessage::CorrectingStop);
        result
    }

    pub(crate) fn show_hud(&self) {
        if let Some(hud) = self.hud_service() {
            let is_batch = crate::storage::get_settings()
                .map(|s| crate::asr::AsrConfig::from(&s).is_batch())
                .unwrap_or(false);
            hud.show(is_batch);
        }
        self.set_escape_swallowing(true);
    }

    pub(crate) fn hide_hud(&self) {
        self.cancel_audio_level_task();
        if let Some(hud) = self.hud_service() {
            hud.hide();
        }
        self.set_escape_swallowing(false);
    }

    pub(crate) fn hide_hud_and_reset_state(&self, state: &mut AppState) {
        self.cancel_auto_hide();
        self.cancel_asr_final_timeout();
        self.stop_asr_audio_bridge();
        self.invalidate_audio_epoch();
        // If an audio file is still referenced in state, it was never persisted to the
        // database (e.g. ASR returned no result).  Delete it so it doesn't become an orphan.
        self.discard_session_audio_file(state, "session_reset");
        self.discard_session_refinement_audio_file(state, "session_reset");
        self.hide_hud();
        state.reset();
        self.emit_state_from(state);
    }

    pub(crate) fn discard_session_audio_file(&self, state: &mut AppState, reason: &str) {
        let Some(path) = state.session_audio_path.take() else {
            return;
        };

        match std::fs::remove_file(&path) {
            Ok(()) => {
                log::info!(
                    "Removed session audio file (reason: {}, path: {})",
                    reason,
                    path.display()
                );
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                log::debug!(
                    "Session audio file already removed (reason: {}, path: {})",
                    reason,
                    path.display()
                );
            }
            Err(err) => {
                log::warn!(
                    "Failed to remove session audio file (reason: {}, path: {}, err: {})",
                    reason,
                    path.display(),
                    err
                );
            }
        }
    }

    pub(crate) fn discard_session_refinement_audio_file(&self, state: &mut AppState, reason: &str) {
        let Some(path) = state.session_refinement_audio_path.take() else {
            return;
        };

        match std::fs::remove_file(&path) {
            Ok(()) => {
                log::info!(
                    "Removed refinement audio file (reason: {}, path: {})",
                    reason,
                    path.display()
                );
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                log::debug!(
                    "Refinement audio file already removed (reason: {}, path: {})",
                    reason,
                    path.display()
                );
            }
            Err(err) => {
                log::warn!(
                    "Failed to remove refinement audio file (reason: {}, path: {}, err: {})",
                    reason,
                    path.display(),
                    err
                );
            }
        }
    }

    fn start_hold_timer(&self, threshold_ms: u64) {
        self.cancel_hold_timer();
        self.cancel_asr_final_timeout();

        let controller = self.clone();
        let handle = tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(threshold_ms)).await;
            controller.send_message(SessionMessage::HoldThresholdReached);
        });

        if let Ok(mut guard) = self.hold_timer.lock() {
            *guard = Some(handle);
        }
    }

    fn cancel_hold_timer(&self) {
        if let Ok(mut guard) = self.hold_timer.lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
            }
        }
    }

    fn start_recording_timeout(&self, minutes: u32) {
        self.cancel_recording_timeout();
        self.cancel_asr_final_timeout();

        let controller = self.clone();
        let total_secs = (minutes as u64) * 60;

        if total_secs == 0 {
            log::debug!("Recording timeout disabled (minutes=0)");
            return;
        }

        log::debug!("Recording timeout scheduled for {} seconds", total_secs);
        if let Some(countdown) = self.countdown_service() {
            let controller_tick = controller.clone();
            countdown.start(
                total_secs,
                move |remaining| {
                    if remaining as u64 <= COUNTDOWN_THRESHOLD_SECS {
                        controller_tick
                            .send_message(SessionMessage::RecordingCountdownTick(remaining));
                    }
                },
                move || {
                    controller.send_message(SessionMessage::RecordingTimeout);
                },
            );
        }
    }

    fn cancel_recording_timeout(&self) {
        if let Some(countdown) = self.countdown_service() {
            if countdown.cancel() {
                log::debug!("Recording timeout cancelled");
            }
        }
        self.emit_countdown(None);
    }

    fn abort_asr_task(&self) {
        if let Ok(mut token_guard) = self.asr_cancel_token.lock() {
            if let Some(token) = token_guard.take() {
                token.cancel();
            }
        }
        if let Ok(mut guard) = self.asr_task.lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
            }
        }
    }

    fn stop_asr_audio_bridge(&self) {
        if let Ok(mut guard) = self.asr_audio_bridge.lock() {
            if let Some(bridge) = guard.take() {
                bridge.close();
            }
        }
        if let Ok(mut guard) = self.asr_audio_bridge_task.lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
            }
        }
    }

    fn start_asr_audio_bridge(&self, mut rx: tokio::sync::mpsc::Receiver<Vec<u8>>) {
        self.stop_asr_audio_bridge();
        let bridge = AsrAudioBridge::default();
        let bridge_for_task = bridge.clone();
        let handle = tauri::async_runtime::spawn(async move {
            while let Some(chunk) = rx.recv().await {
                bridge_for_task.push_chunk(chunk);
            }
            bridge_for_task.close();
        });

        if let Ok(mut guard) = self.asr_audio_bridge.lock() {
            *guard = Some(bridge);
        }
        if let Ok(mut guard) = self.asr_audio_bridge_task.lock() {
            *guard = Some(handle);
        }
    }

    fn take_asr_attempt_receiver(&self) -> Option<tokio::sync::mpsc::Receiver<Vec<u8>>> {
        self.asr_audio_bridge
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().cloned())
            .map(|bridge| bridge.attach_stream())
    }

    fn mark_asr_replay_checkpoint(&self) {
        if let Ok(guard) = self.asr_audio_bridge.lock() {
            if let Some(bridge) = guard.as_ref() {
                bridge.clear_replay_buffer();
            }
        }
    }

    fn spawn_audio_level_bridge(
        &self,
        mut rx: tokio::sync::mpsc::Receiver<crate::audio::AudioVisualizationFrame>,
    ) {
        self.cancel_audio_level_task();

        let controller = self.clone();
        let handle = tauri::async_runtime::spawn(async move {
            while let Some(frame) = rx.recv().await {
                if let Some(hud) = controller.hud_service() {
                    hud.emit_audio_level(frame.level);
                    hud.emit_audio_spectrum(&frame.bands);
                }
            }

            if let Some(hud) = controller.hud_service() {
                hud.emit_audio_level(0.0);
                hud.emit_audio_spectrum(&[]);
            }
        });

        if let Ok(mut guard) = self.audio_level_task.lock() {
            *guard = Some(handle);
        }
    }

    fn cancel_audio_level_task(&self) {
        if let Ok(mut guard) = self.audio_level_task.lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
            }
        }

        if let Some(hud) = self.hud_service() {
            hud.emit_audio_level(0.0);
            hud.emit_audio_spectrum(&[]);
        }
    }

    fn schedule_finalize_cleanup(&self) {
        self.cancel_auto_hide();

        let controller = self.clone();
        if let Some(hud) = self.hud_service() {
            hud.schedule_hide(FINALIZE_HIDE_DELAY_MS, move || {
                controller.send_message(SessionMessage::FinalizeHideReady);
            });
        }
    }

    fn schedule_error_cleanup(&self) {
        self.cancel_auto_hide();

        let controller = self.clone();
        if let Some(hud) = self.hud_service() {
            hud.schedule_hide(ERROR_HIDE_DELAY_MS, move || {
                controller.send_message(SessionMessage::ErrorDisplayDone);
            });
        }
    }

    pub(crate) fn cancel_auto_hide(&self) {
        if let Some(hud) = self.hud_service() {
            hud.cancel_hide();
        }
    }

    pub(crate) fn cancel_asr_final_timeout(&self) {
        if let Ok(mut guard) = self.asr_final_timeout.lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
            }
        }
    }

    pub(crate) fn emit_state_from(&self, state: &AppState) {
        if let Some(hud) = self.hud_service() {
            let recording_style_for_hud = match state.session_state {
                HotkeySessionState::Idle | HotkeySessionState::Finalizing => None,
                _ => state.recording_style,
            };
            let is_batch = crate::storage::get_settings()
                .map(|s| crate::asr::AsrConfig::from(&s).is_batch())
                .unwrap_or(false);
            hud.emit_recording_style(recording_style_for_hud, is_batch);
            hud.emit_correcting(state.is_correcting);
            hud.emit_intent(state.intent);
            let is_post_recording_batch_refine = state.asr_refinement_in_progress
                && matches!(
                    state.active_asr_refinement_provider,
                    Some(crate::asr::AsrProviderType::ElevenLabs)
                        | Some(crate::asr::AsrProviderType::OpenAI)
                        | Some(crate::asr::AsrProviderType::Qwen)
                );
            let is_recognizing = state.session_state == HotkeySessionState::Finalizing
                && ((!state.asr_stream_finished && !state.is_correcting)
                    || is_post_recording_batch_refine);
            hud.emit_recognizing(is_recognizing);
            if state.session_state == HotkeySessionState::Finalizing && state.asr_stream_finished {
                hud.emit_recognition_stopped();
            }
        }
    }

    fn emit_transcript(&self, text: &str, is_final: bool) {
        if let Some(hud) = self.hud_service() {
            hud.emit_transcript(text, is_final);
        }
    }

    fn emit_asr_error(&self, message: &str) {
        if let Some(hud) = self.hud_service() {
            hud.emit_error(Some(message));
        }
    }

    fn clear_asr_error(&self) {
        if let Some(hud) = self.hud_service() {
            hud.emit_error(None);
        }
    }

    fn emit_countdown(&self, seconds: Option<u32>) {
        if let Some(hud) = self.hud_service() {
            hud.emit_countdown(seconds);
        }
    }

    fn effective_max_recording_minutes(&self, state: &AppState) -> u32 {
        let user_limit = state.max_recording_minutes;
        let provider_limit = crate::storage::get_settings()
            .ok()
            .map(|settings| crate::asr::AsrConfig::from(&settings))
            .and_then(|config| config.max_recording_minutes_limit());

        match provider_limit {
            Some(limit) if user_limit == 0 => limit,
            Some(limit) => user_limit.min(limit),
            None => user_limit,
        }
    }

    fn schedule_asr_startup_retry(&self, retry_count: u32) {
        let delay_ms = match retry_count {
            1 => 300,
            2 => 1_000,
            _ => 2_000,
        };
        let controller = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            controller.send_message(SessionMessage::RetryAsrStartup);
        });
    }

    fn schedule_asr_reconnect_retry(&self, retry_count: u32) {
        let delay_ms = match retry_count {
            1 => 300,
            _ => 1_000,
        };
        let controller = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            controller.send_message(SessionMessage::RetryAsrReconnect);
        });
    }

    fn on_retry_asr_startup_state(&self, state: &mut AppState) {
        if state.terminal_error_message.is_some()
            || state.asr_received_event
            || state.asr_stream_finished
            || state.session_state == HotkeySessionState::Idle
        {
            return;
        }

        let (sample_rate, channels) = match (state.session_sample_rate, state.session_channels) {
            (Some(sample_rate), Some(channels)) => (sample_rate, channels),
            _ => return,
        };

        log::info!(
            "Retrying ASR startup (attempt {}/{})",
            state.asr_startup_retry_count + 1,
            ASR_STARTUP_RETRY_MAX_ATTEMPTS
        );
        self.spawn_asr(sample_rate, channels);
    }

    fn on_retry_asr_reconnect_state(&self, state: &mut AppState) {
        if state.terminal_error_message.is_some()
            || state.asr_stream_finished
            || state.session_state == HotkeySessionState::Idle
            || !state.asr_reconnect_in_progress
        {
            return;
        }

        let (sample_rate, channels) = match (state.session_sample_rate, state.session_channels) {
            (Some(sample_rate), Some(channels)) => (sample_rate, channels),
            _ => return,
        };

        log::info!(
            "Retrying ASR reconnect (attempt {}/{})",
            state.asr_reconnect_retry_count + 1,
            ASR_RECONNECT_MAX_ATTEMPTS
        );
        self.spawn_asr(sample_rate, channels);
    }

    fn set_escape_swallowing(&self, enabled: bool) {
        if let Ok(handle_guard) = self.app_handle.lock() {
            if let Some(app) = handle_guard.as_ref() {
                let manager: tauri::State<'_, crate::hotkey::HotkeyManager> = app.state();
                manager.set_escape_swallowing(enabled);
            }
        }
    }

    fn handle_cancel_session_state(&self, state: &mut AppState, reason: CancelReason) {
        log::info!("Cancelling session: {:?}", reason);
        // Invalidate any in-flight injection tasks.
        self.injection_epoch.fetch_add(1, Ordering::SeqCst);
        self.injection_cancel_flag.store(true, Ordering::SeqCst);
        self.cancel_hold_timer();
        self.cancel_recording_timeout();
        self.cancel_auto_hide();
        // Stop ASR task immediately to avoid stale events/stream-finished driving injection.
        self.abort_asr_task();
        self.stop_asr_audio_bridge();

        // Stop capture/asr first so downstream callbacks won't emit into a stale state.
        if state.is_recording {
            self.stop_audio_capture("cancelled");
        } else {
            self.abort_asr_task();
        }
        self.invalidate_audio_epoch();

        self.discard_session_audio_file(state, "session_cancelled");
        self.discard_session_refinement_audio_file(state, "session_cancelled");

        self.hide_hud();
        state.reset();
        self.emit_state_from(state);
        self.set_escape_swallowing(false);
    }

    fn start_audio_capture(&self) {
        let audio_epoch = self.next_audio_epoch();
        if let Some(manager) = self.audio_manager() {
            let is_batch = crate::storage::get_settings()
                .map(|settings| {
                    let config = crate::asr::AsrConfig::from(&settings);
                    config.is_batch()
                })
                .unwrap_or(false);
            let capture_refinement_pcm = if is_batch {
                // Batch mode always needs the full PCM buffer for post-recording recognition.
                true
            } else {
                crate::storage::get_settings()
                    .map(|settings| {
                        let config = crate::asr::AsrConfig::from(&settings);
                        config.provider_type == crate::asr::AsrProviderType::Coli
                            && config.coli_final_refinement_mode
                                != crate::asr::ColiRefinementMode::Off
                    })
                    .unwrap_or(false)
            };
            log::info!(
                "capture_buffer_enabled={} is_batch={}",
                capture_refinement_pcm,
                is_batch
            );
            match manager.start_capture(capture_refinement_pcm) {
                Ok(handle) => {
                    let crate::audio::AudioCaptureHandle {
                        receiver: rx,
                        viz_receiver: level_rx,
                        file_path,
                        sample_rate,
                        channels,
                    } = handle;
                    let path_str = file_path.as_ref().map(|p| p.to_string_lossy().to_string());
                    self.spawn_audio_level_bridge(level_rx);
                    self.send_message(SessionMessage::AudioStarted {
                        audio_epoch,
                        sample_rate,
                        channels,
                        path: path_str.clone(),
                    });
                    log::info!(
                        "Audio capture started ({} Hz, {} ch, batch={})",
                        sample_rate,
                        channels,
                        is_batch
                    );
                    if is_batch {
                        // In batch mode we do NOT spawn a streaming ASR task.
                        // The receiver is dropped so audio chunks are discarded;
                        // the refinement PCM buffer captures the full recording.
                        drop(rx);
                    } else {
                        self.start_asr_audio_bridge(rx);
                        self.spawn_asr(sample_rate, channels);
                    }
                }
                Err(err) => {
                    let reason = format!("Failed to start audio capture: {}", err);
                    log::error!("{}", reason);
                    self.send_message(SessionMessage::AudioStartFailed {
                        audio_epoch,
                        reason,
                    });
                }
            }
        } else {
            let reason = "Audio manager not initialized; cannot start capture".to_string();
            log::error!("{}", reason);
            self.send_message(SessionMessage::AudioStartFailed {
                audio_epoch,
                reason,
            });
        }
    }

    fn stop_audio_capture(&self, reason: &str) {
        // No further auto-stop after we intentionally stop capture.
        self.cancel_recording_timeout();
        let audio_epoch = self.current_audio_epoch();

        if let Some(manager) = self.audio_manager() {
            match manager.stop_capture() {
                Ok(summary) => {
                    let path_str = summary
                        .path
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| "<none>".to_string());
                    self.send_message(SessionMessage::AudioStopped {
                        audio_epoch,
                        path: summary
                            .path
                            .as_ref()
                            .map(|p| p.to_string_lossy().to_string()),
                        refinement_path: summary
                            .refinement_path
                            .as_ref()
                            .map(|p| p.to_string_lossy().to_string()),
                        duration_ms: Some(summary.duration_ms),
                    });
                    log::info!(
                        "COLI_REFINE audio_stop opus_path={} refinement_wav_path={}",
                        summary
                            .path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "<none>".to_string()),
                        summary
                            .refinement_path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "<none>".to_string())
                    );
                    log::info!(
                        "Audio capture stopped (reason: {}, duration {} ms, bytes {}, path={})",
                        reason,
                        summary.duration_ms,
                        summary.bytes_written,
                        path_str
                    );
                }
                Err(err) => {
                    log::error!("Failed to stop audio capture (reason: {}): {}", reason, err);
                    self.send_message(SessionMessage::AudioStopFailed {
                        audio_epoch,
                        reason: err.to_string(),
                    });
                }
            }
        } else {
            log::error!("Audio manager not initialized; cannot stop capture");
            self.send_message(SessionMessage::AudioStopFailed {
                audio_epoch,
                reason: "Audio manager not initialized; cannot stop capture".to_string(),
            });
        }
    }

    fn next_audio_epoch(&self) -> u64 {
        self.audio_epoch
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1)
    }

    fn current_audio_epoch(&self) -> u64 {
        self.audio_epoch.load(Ordering::SeqCst)
    }

    fn is_current_audio_epoch(&self, epoch: u64) -> bool {
        epoch != 0 && epoch == self.current_audio_epoch()
    }

    fn invalidate_audio_epoch(&self) {
        self.next_audio_epoch();
    }

    fn hud_service(&self) -> Option<HudService> {
        self.hud_service.lock().ok().and_then(|h| h.clone())
    }

    pub fn replay_hud_snapshot(&self) {
        if let Some(hud) = self.hud_service() {
            hud.replay_snapshot();
        }
    }

    fn audio_manager(&self) -> Option<AudioManager> {
        self.audio_manager.lock().ok().and_then(|m| m.clone())
    }

    fn countdown_service(&self) -> Option<CountdownService> {
        self.countdown_service.lock().ok().and_then(|c| c.clone())
    }

    fn app_handle(&self) -> Option<AppHandle> {
        self.app_handle.lock().ok().and_then(|h| h.clone())
    }

    fn capture_foreground_app(&self) -> Result<crate::foreground_app::ForegroundAppInfo, String> {
        let app = self
            .app_handle()
            .ok_or_else(|| "App handle is not initialized".to_string())?;
        crate::foreground_app::detect_foreground_app(&app)
    }

    fn handle_audio_started_state(
        &self,
        state: &mut AppState,
        sample_rate: u32,
        channels: u16,
        path: Option<String>,
    ) {
        state.session_audio_path = path.map(|p| p.into());
        state.session_start = Some(Instant::now());
        state.session_sample_rate = Some(sample_rate);
        state.session_channels = Some(channels);
    }

    fn handle_audio_stopped_state(
        &self,
        state: &mut AppState,
        path: Option<String>,
        refinement_path: Option<String>,
        duration_ms: Option<u64>,
    ) {
        state.session_audio_path = path.as_ref().map(|p| p.into());
        state.session_refinement_audio_path = refinement_path.as_ref().map(|p| p.into());
        state.session_duration_ms = duration_ms;
        state.session_start = None;

        if state.terminal_error_message.is_some() {
            log::info!("Audio capture stopped after ASR failure; skipping transcription pipeline");
            self.schedule_error_cleanup();
            return;
        }

        if let Some(ms) = duration_ms {
            if ms < MIN_RECORDING_MS {
                log::info!(
                    "Recording too short ({} ms); skipping ASR/LLM/injection",
                    ms
                );
                self.send_message(SessionMessage::CancelSession(CancelReason::TooShort {
                    duration_ms: ms,
                    audio_path: path.clone(),
                }));
                return;
            }
        }

        // Batch mode: no streaming ASR was running — hand off to batch recognition.
        let is_batch = crate::storage::get_settings()
            .map(|s| crate::asr::AsrConfig::from(&s).is_batch())
            .unwrap_or(false);
        if is_batch {
            log::info!(
                "Recording stopped (batch mode): duration_ms={:?}, starting batch ASR",
                duration_ms,
            );
            self.start_batch_asr(state);
            return;
        }

        // If no final result arrived but we have a transcript, promote the latest transcript to final as a fallback.
        if !state.has_final_result && !state.transcript_text.is_empty() {
            log::warn!(
                "No ASR final before stop; using last transcript as final (len={})",
                state.transcript_text.chars().count()
            );
            state.session_final_text = state.transcript_text.clone();
            state.last_injected_text = state.transcript_text.clone();
            state.has_final_result = true;
        }

        log::info!(
            "Recording stopped: duration_ms={:?}, has_final={}, final_len={}, is_recording={}, final_injected={}, asr_finished={}",
            duration_ms,
            state.has_final_result,
            state.session_final_text.chars().count(),
            state.is_recording,
            state.final_injected,
            state.asr_stream_finished
        );

        // If a final already arrived (we treat it as ASR finished), inject immediately.
        if state.has_final_result && state.asr_stream_finished {
            self.maybe_inject_final_state(state);
            return;
        }

        // Wait for ASR stream-finished; schedule a fallback in case the server never closes.
        if !state.asr_stream_finished {
            let controller = self.clone();
            let handle = tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(ASR_FINAL_WAIT_MS)).await;
                controller.send_message(SessionMessage::AsrFinalTimeout);
            });
            if let Ok(mut guard) = self.asr_final_timeout.lock() {
                *guard = Some(handle);
            }
        }
    }

    fn on_correcting_start(&self, state: &mut AppState) {
        // Keep HUD visible during correction; cancel pending auto-hide.
        self.cancel_auto_hide();
        state.is_correcting = true;
        self.emit_state_from(state);
    }

    fn on_correcting_stop(&self, state: &mut AppState) {
        state.is_correcting = false;
        self.emit_state_from(state);

        if state.session_state == HotkeySessionState::Finalizing {
            // If hide was cancelled during correction, reschedule it so final text stays visible briefly.
            self.schedule_finalize_cleanup();
        }
    }

    fn on_error_display_done_state(&self, state: &mut AppState) {
        if state.terminal_error_message.is_none() {
            return;
        }

        self.cancel_audio_level_task();
        self.stop_asr_audio_bridge();
        self.discard_session_audio_file(state, "asr_stream_failed");
        self.discard_session_refinement_audio_file(state, "asr_stream_failed");
        self.hide_hud_and_reset_state(state);
    }

    fn on_audio_start_failed_state(&self, state: &mut AppState, reason: String) {
        log::warn!("Audio start failed; resetting session: {}", reason);

        // Invalidate any in-flight operations just in case.
        self.injection_epoch.fetch_add(1, Ordering::SeqCst);
        self.injection_cancel_flag.store(true, Ordering::SeqCst);

        self.cancel_hold_timer();
        self.cancel_recording_timeout();
        self.cancel_auto_hide();
        self.abort_asr_task();
        self.stop_asr_audio_bridge();
        self.cancel_audio_level_task();
        self.invalidate_audio_epoch();

        self.discard_session_audio_file(state, "audio_start_failed");
        self.discard_session_refinement_audio_file(state, "audio_start_failed");
        self.hide_hud();
        state.reset();
        self.emit_state_from(state);
        self.set_escape_swallowing(false);
    }

    fn on_audio_stop_failed_state(&self, state: &mut AppState, reason: String) {
        let combined_reason = match state.terminal_error_message.as_deref() {
            Some(existing) => format!("{}\n{}", existing, reason),
            None => reason,
        };
        log::warn!("Audio stop failed; resetting session: {}", combined_reason);

        state.terminal_error_message = Some(combined_reason.clone());
        state.terminal_asr_failure = None;
        self.emit_asr_error(&combined_reason);
        self.cancel_audio_level_task();
        self.schedule_error_cleanup();
    }
}

impl Default for SessionController {
    fn default() -> Self {
        Self {
            hud_service: Arc::new(Mutex::new(None)),
            audio_manager: Arc::new(Mutex::new(None)),
            countdown_service: Arc::new(Mutex::new(None)),
            app_handle: Arc::new(Mutex::new(None)),
            coordinator_tx: Arc::new(Mutex::new(None)),
            hold_timer: Arc::new(Mutex::new(None)),
            asr_task: Arc::new(Mutex::new(None)),
            asr_audio_bridge: Arc::new(Mutex::new(None)),
            asr_audio_bridge_task: Arc::new(Mutex::new(None)),
            audio_level_task: Arc::new(Mutex::new(None)),
            asr_cancel_token: Arc::new(Mutex::new(None)),
            asr_final_timeout: Arc::new(Mutex::new(None)),
            injection_epoch: Arc::new(AtomicU64::new(0)),
            injection_cancel_flag: Arc::new(AtomicBool::new(false)),
            audio_epoch: Arc::new(AtomicU64::new(0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::HotkeySessionState;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn path_string(path: Option<&std::path::PathBuf>) -> Option<String> {
        path.map(|value| value.to_string_lossy().to_string())
    }

    #[test]
    fn current_audio_epoch_messages_update_session_state() {
        let controller = SessionController::default();
        let mut state = AppState::new();
        let epoch = controller.next_audio_epoch();

        controller.handle_message(
            &mut state,
            SessionMessage::AudioStarted {
                audio_epoch: epoch,
                sample_rate: 16_000,
                channels: 1,
                path: Some("active.ogg".to_string()),
            },
        );

        assert_eq!(
            path_string(state.session_audio_path.as_ref()),
            Some("active.ogg".into())
        );
        assert_eq!(state.session_sample_rate, Some(16_000));
        assert_eq!(state.session_channels, Some(1));

        state.asr_stream_finished = true;
        controller.handle_message(
            &mut state,
            SessionMessage::AudioStopped {
                audio_epoch: epoch,
                path: Some("active.ogg".to_string()),
                refinement_path: Some("active.wav".to_string()),
                duration_ms: Some(1_500),
            },
        );

        assert_eq!(
            path_string(state.session_audio_path.as_ref()),
            Some("active.ogg".into())
        );
        assert_eq!(
            path_string(state.session_refinement_audio_path.as_ref()),
            Some("active.wav".into())
        );
        assert_eq!(state.session_duration_ms, Some(1_500));
    }

    #[test]
    fn stale_audio_stopped_message_is_ignored_after_cancel() {
        let controller = SessionController::default();
        let mut state = AppState::new();
        let t0 = Instant::now();

        state.handle_hotkey_pressed_at(t0);
        state.handle_hotkey_released_at(t0 + ms(80));
        assert_eq!(state.session_state, HotkeySessionState::HandsFree);
        assert!(state.is_recording);

        let epoch = controller.next_audio_epoch();
        controller.handle_message(
            &mut state,
            SessionMessage::AudioStarted {
                audio_epoch: epoch,
                sample_rate: 16_000,
                channels: 1,
                path: Some("session.ogg".to_string()),
            },
        );

        controller.handle_message(
            &mut state,
            SessionMessage::CancelSession(CancelReason::EscapeKey),
        );

        assert_eq!(state.session_state, HotkeySessionState::Idle);
        assert!(!state.is_recording);
        assert!(state.session_audio_path.is_none());
        assert!(controller.current_audio_epoch() > epoch);

        controller.handle_message(
            &mut state,
            SessionMessage::AudioStopped {
                audio_epoch: epoch,
                path: Some("stale.ogg".to_string()),
                refinement_path: Some("stale.wav".to_string()),
                duration_ms: Some(2_000),
            },
        );

        assert_eq!(state.session_state, HotkeySessionState::Idle);
        assert!(state.session_audio_path.is_none());
        assert!(state.session_refinement_audio_path.is_none());
        assert!(state.session_duration_ms.is_none());
    }

    #[test]
    fn stale_audio_start_failed_message_is_ignored_after_epoch_advance() {
        let controller = SessionController::default();
        let mut state = AppState::new();
        let stale_epoch = controller.next_audio_epoch();

        state.session_state = HotkeySessionState::HandsFree;
        state.is_recording = true;
        controller.invalidate_audio_epoch();

        controller.handle_message(
            &mut state,
            SessionMessage::AudioStartFailed {
                audio_epoch: stale_epoch,
                reason: "stale failure".to_string(),
            },
        );

        assert_eq!(state.session_state, HotkeySessionState::HandsFree);
        assert!(state.is_recording);
        assert!(state.terminal_error_message.is_none());
    }

    #[test]
    fn asr_stream_finished_keeps_recording_active_until_hotkey_stop() {
        let controller = SessionController::default();
        let mut state = AppState::new();
        let t0 = Instant::now();

        state.handle_hotkey_pressed_at(t0);
        state.handle_hotkey_released_at(t0 + ms(80));
        assert_eq!(state.session_state, HotkeySessionState::HandsFree);
        assert!(state.is_recording);
        state.transcript_text = "partial transcript".to_string();

        controller.on_asr_stream_finished_state(&mut state);

        assert_eq!(state.session_state, HotkeySessionState::HandsFree);
        assert!(state.is_recording);
        assert!(state.asr_stream_finished);

        state.handle_hotkey_pressed_at(t0 + ms(550));
        state.handle_hotkey_released_at(t0 + ms(620));

        assert_eq!(state.session_state, HotkeySessionState::Finalizing);
        assert!(!state.is_recording);
    }
}
