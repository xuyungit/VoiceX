//! Application state machine

use crate::asr::{AsrFailure, AsrProviderType};
use crate::foreground_app::ForegroundAppInfo;
use crate::injector::TextInjectionMode;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Recording session state
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum HotkeySessionState {
    Idle,
    Pending,
    PushToTalk,
    HandsFree,
    Finalizing,
}

impl Default for HotkeySessionState {
    fn default() -> Self {
        Self::Idle
    }
}

/// Recording style
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum RecordingStyle {
    PushToTalk,
    HandsFree,
}

/// Processing intent for the current session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessingIntent {
    Assistant,
    TranslateEn,
}

impl Default for ProcessingIntent {
    fn default() -> Self {
        Self::Assistant
    }
}

impl ProcessingIntent {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Assistant => "assistant",
            Self::TranslateEn => "translate_en",
        }
    }
}

/// Translation trigger mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranslationTriggerMode {
    Off,
    DoubleTap,
}

impl Default for TranslationTriggerMode {
    fn default() -> Self {
        Self::DoubleTap
    }
}

impl TranslationTriggerMode {
    pub fn from_str(value: &str) -> Self {
        match value {
            "off" => Self::Off,
            _ => Self::DoubleTap,
        }
    }
}

/// Application state
pub struct AppState {
    // Session state
    pub session_state: HotkeySessionState,
    pub recording_style: Option<RecordingStyle>,
    pub intent: ProcessingIntent,
    pub is_recording: bool,
    pub is_correcting: bool,
    pub transcript_text: String,

    // Session data
    pub(crate) session_start: Option<Instant>,
    pub(crate) session_audio_path: Option<PathBuf>,
    pub(crate) session_refinement_audio_path: Option<PathBuf>,
    pub(crate) session_duration_ms: Option<u64>,
    pub(crate) session_sample_rate: Option<u32>,
    pub(crate) session_channels: Option<u16>,
    pub(crate) session_final_text: String,
    pub(crate) session_asr_model_name: Option<String>,
    pub(crate) session_llm_model_name: Option<String>,
    pub(crate) session_target_app: Option<ForegroundAppInfo>,
    pub(crate) terminal_error_message: Option<String>,
    pub(crate) terminal_asr_failure: Option<AsrFailure>,

    // Control flags
    is_hotkey_down: bool,
    hands_free_stop_armed: bool,
    did_receive_audio: bool,

    // Configuration
    pub(crate) hold_threshold_ms: u64,
    pub(crate) max_recording_minutes: u32,
    pub(crate) max_recording_countdown: Option<u32>,
    pub(crate) text_injection_mode: TextInjectionMode,
    pub(crate) has_final_result: bool,
    pub(crate) final_injected: bool,
    pub(crate) last_injected_text: String,
    pub(crate) final_version: u64,
    pub(crate) injection_in_progress: bool,
    pub(crate) asr_stream_finished: bool,
    pub(crate) asr_received_event: bool,
    pub(crate) asr_startup_retry_count: u32,
    pub(crate) asr_reconnect_retry_count: u32,
    pub(crate) asr_reconnect_in_progress: bool,
    pub(crate) asr_reconnect_prefix_text: String,
    pub(crate) asr_refinement_in_progress: bool,
    pub(crate) asr_refinement_done: bool,
    pub(crate) active_asr_refinement_provider: Option<AsrProviderType>,

    // Post-processing
    pub(crate) remove_trailing_punctuation: bool,
    pub(crate) short_sentence_threshold: u32,
    pub(crate) replacement_rules: Vec<crate::commands::settings::ReplacementRule>,

    // Gesture/intent settings
    pub(crate) translation_enabled: bool,
    pub(crate) translation_trigger_mode: TranslationTriggerMode,
    pub(crate) double_tap_window_ms: u64,

    // Gesture runtime
    pub(crate) gesture_press_started_at: Option<Instant>,
    pub(crate) double_tap_upgrade_deadline: Option<Instant>,
    pub(crate) pending_translation_upgrade: bool,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            session_state: HotkeySessionState::Idle,
            recording_style: None,
            intent: ProcessingIntent::Assistant,
            is_recording: false,
            is_correcting: false,
            transcript_text: String::new(),
            session_start: None,
            session_audio_path: None,
            session_refinement_audio_path: None,
            session_duration_ms: None,
            session_sample_rate: None,
            session_channels: None,
            session_final_text: String::new(),
            session_asr_model_name: None,
            session_llm_model_name: None,
            session_target_app: None,
            terminal_error_message: None,
            terminal_asr_failure: None,
            is_hotkey_down: false,
            hands_free_stop_armed: false,
            did_receive_audio: false,
            hold_threshold_ms: 1000,
            max_recording_minutes: 5,
            max_recording_countdown: None,
            text_injection_mode: TextInjectionMode::Pasteboard,
            has_final_result: false,
            final_injected: false,
            last_injected_text: String::new(),
            final_version: 0,
            injection_in_progress: false,
            asr_stream_finished: false,
            asr_received_event: false,
            asr_startup_retry_count: 0,
            asr_reconnect_retry_count: 0,
            asr_reconnect_in_progress: false,
            asr_reconnect_prefix_text: String::new(),
            asr_refinement_in_progress: false,
            asr_refinement_done: false,
            active_asr_refinement_provider: None,
            remove_trailing_punctuation: true,
            short_sentence_threshold: 5,
            replacement_rules: Vec::new(),
            translation_enabled: true,
            translation_trigger_mode: TranslationTriggerMode::DoubleTap,
            double_tap_window_ms: 400,
            gesture_press_started_at: None,
            double_tap_upgrade_deadline: None,
            pending_translation_upgrade: false,
        }
    }

    /// Handle hotkey pressed event
    pub fn handle_hotkey_pressed(&mut self) {
        self.handle_hotkey_pressed_at(Instant::now());
    }

    pub fn handle_hotkey_pressed_at(&mut self, now: Instant) {
        log::debug!("handle_hotkey_pressed: state={:?}", self.session_state);

        if self.session_state == HotkeySessionState::Finalizing {
            log::debug!("Ignoring press during Finalizing");
            return;
        }

        self.is_hotkey_down = true;
        self.gesture_press_started_at = Some(now);

        match self.session_state {
            HotkeySessionState::Idle => {
                self.session_state = HotkeySessionState::Pending;
                self.hands_free_stop_armed = false;
                self.recording_style = None;
                self.intent = ProcessingIntent::Assistant;
                self.is_recording = true;
                self.session_start = Some(now);
                self.session_audio_path = None;
                self.session_refinement_audio_path = None;
                self.session_duration_ms = None;
                self.session_sample_rate = None;
                self.session_channels = None;
                self.session_final_text.clear();
                self.transcript_text.clear();
                self.terminal_error_message = None;
                self.terminal_asr_failure = None;
                self.session_target_app = None;
                self.max_recording_countdown = None;
                self.final_injected = false;
                self.has_final_result = false;
                self.last_injected_text.clear();
                self.double_tap_upgrade_deadline = None;
                self.pending_translation_upgrade = false;
                self.asr_received_event = false;
                self.asr_startup_retry_count = 0;
                self.asr_reconnect_retry_count = 0;
                self.asr_reconnect_in_progress = false;
                self.asr_reconnect_prefix_text.clear();
                self.asr_refinement_in_progress = false;
                self.asr_refinement_done = false;
                self.active_asr_refinement_provider = None;
                // TODO: Start recording and schedule hold threshold check
            }
            HotkeySessionState::HandsFree => {
                if self.should_upgrade_to_translate(now) {
                    self.pending_translation_upgrade = true;
                    self.hands_free_stop_armed = false;
                } else {
                    self.pending_translation_upgrade = false;
                    self.hands_free_stop_armed = true;
                }
            }
            _ => {}
        }
    }

    /// Handle hotkey released event
    pub fn handle_hotkey_released(&mut self) {
        self.handle_hotkey_released_at(Instant::now());
    }

    pub fn handle_hotkey_released_at(&mut self, now: Instant) {
        log::debug!("handle_hotkey_released: state={:?}", self.session_state);

        self.is_hotkey_down = false;
        let press_duration_ms = self
            .gesture_press_started_at
            .take()
            .map(|start| now.saturating_duration_since(start).as_millis() as u64)
            .unwrap_or(0);

        match self.session_state {
            HotkeySessionState::Pending => {
                // Short press -> Hands-free mode
                self.transition_to_hands_free(now);
            }
            HotkeySessionState::PushToTalk => {
                self.transition_to_finalizing();
            }
            HotkeySessionState::HandsFree => {
                if self.pending_translation_upgrade {
                    let is_short_press = press_duration_ms < self.hold_threshold_ms;
                    if is_short_press {
                        self.intent = ProcessingIntent::TranslateEn;
                        self.pending_translation_upgrade = false;
                        self.hands_free_stop_armed = false;
                        self.double_tap_upgrade_deadline = None;
                        return;
                    }
                    self.pending_translation_upgrade = false;
                    // Fallback: a long second press inside the double-tap window
                    // should behave like a normal stop, not a no-op.
                    self.hands_free_stop_armed = true;
                }

                if self.hands_free_stop_armed {
                    self.transition_to_finalizing();
                } else {
                    self.hands_free_stop_armed = false;
                }
            }
            _ => {}
        }
    }

    /// Called when hold threshold is reached (long press)
    pub fn on_hold_threshold_reached(&mut self) {
        if self.session_state == HotkeySessionState::Pending && self.is_hotkey_down {
            self.transition_to_push_to_talk();
        }
    }

    fn transition_to_push_to_talk(&mut self) {
        log::info!("Transitioning to PushToTalk mode");
        self.session_state = HotkeySessionState::PushToTalk;
        self.recording_style = Some(RecordingStyle::PushToTalk);
        self.hands_free_stop_armed = false;
        self.pending_translation_upgrade = false;
        self.double_tap_upgrade_deadline = None;
        self.is_recording = true;
    }

    fn transition_to_hands_free(&mut self, now: Instant) {
        log::info!("Transitioning to HandsFree mode");
        self.session_state = HotkeySessionState::HandsFree;
        self.recording_style = Some(RecordingStyle::HandsFree);
        self.hands_free_stop_armed = false;
        self.pending_translation_upgrade = false;
        if self.can_open_double_tap_window() {
            self.double_tap_upgrade_deadline =
                Some(now + Duration::from_millis(self.double_tap_window_ms));
        } else {
            self.double_tap_upgrade_deadline = None;
        }
        // TODO: Schedule max recording timeout
    }

    pub fn transition_to_finalizing(&mut self) {
        log::info!("Transitioning to Finalizing");
        self.session_state = HotkeySessionState::Finalizing;
        self.is_hotkey_down = false;
        self.hands_free_stop_armed = false;
        self.pending_translation_upgrade = false;
        self.double_tap_upgrade_deadline = None;
        self.is_recording = false;
        self.max_recording_countdown = None;
        // TODO: Stop recording
    }

    fn should_upgrade_to_translate(&self, now: Instant) -> bool {
        if !self.can_open_double_tap_window() {
            return false;
        }
        if self.intent != ProcessingIntent::Assistant {
            return false;
        }

        match self.double_tap_upgrade_deadline {
            Some(deadline) => now <= deadline,
            None => false,
        }
    }

    fn can_open_double_tap_window(&self) -> bool {
        self.translation_enabled
            && self.translation_trigger_mode == TranslationTriggerMode::DoubleTap
            && self.double_tap_window_ms > 0
    }

    /// Reset to idle state
    pub fn reset(&mut self) {
        self.session_state = HotkeySessionState::Idle;
        self.recording_style = None;
        self.intent = ProcessingIntent::Assistant;
        self.is_recording = false;
        self.is_correcting = false;
        self.transcript_text.clear();
        self.session_start = None;
        self.session_audio_path = None;
        self.session_refinement_audio_path = None;
        self.session_duration_ms = None;
        self.session_sample_rate = None;
        self.session_channels = None;
        self.session_final_text.clear();
        self.session_asr_model_name = None;
        self.session_llm_model_name = None;
        self.session_target_app = None;
        self.terminal_error_message = None;
        self.terminal_asr_failure = None;
        self.is_hotkey_down = false;
        self.hands_free_stop_armed = false;
        self.did_receive_audio = false;
        self.max_recording_countdown = None;
        self.final_injected = false;
        self.has_final_result = false;
        self.last_injected_text.clear();
        self.final_version = 0;
        self.injection_in_progress = false;
        self.asr_stream_finished = false;
        self.asr_received_event = false;
        self.asr_startup_retry_count = 0;
        self.asr_reconnect_retry_count = 0;
        self.asr_reconnect_in_progress = false;
        self.asr_reconnect_prefix_text.clear();
        self.asr_refinement_in_progress = false;
        self.asr_refinement_done = false;
        self.active_asr_refinement_provider = None;
        self.gesture_press_started_at = None;
        self.double_tap_upgrade_deadline = None;
        self.pending_translation_upgrade = false;
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn single_tap_starts_hands_free_assistant() {
        let mut state = AppState::new();
        let t0 = Instant::now();

        state.handle_hotkey_pressed_at(t0);
        state.handle_hotkey_released_at(t0 + ms(120));

        assert_eq!(state.session_state, HotkeySessionState::HandsFree);
        assert_eq!(state.recording_style, Some(RecordingStyle::HandsFree));
        assert_eq!(state.intent, ProcessingIntent::Assistant);
    }

    #[test]
    fn long_press_enters_push_to_talk() {
        let mut state = AppState::new();
        let t0 = Instant::now();

        state.handle_hotkey_pressed_at(t0);
        state.on_hold_threshold_reached();
        assert_eq!(state.session_state, HotkeySessionState::PushToTalk);

        state.handle_hotkey_released_at(t0 + ms(1500));
        assert_eq!(state.session_state, HotkeySessionState::Finalizing);
    }

    #[test]
    fn double_tap_upgrades_to_translate_mode() {
        let mut state = AppState::new();
        let t0 = Instant::now();

        state.handle_hotkey_pressed_at(t0);
        state.handle_hotkey_released_at(t0 + ms(80));
        assert_eq!(state.session_state, HotkeySessionState::HandsFree);
        assert_eq!(state.intent, ProcessingIntent::Assistant);

        state.handle_hotkey_pressed_at(t0 + ms(220));
        state.handle_hotkey_released_at(t0 + ms(280));

        assert_eq!(state.session_state, HotkeySessionState::HandsFree);
        assert_eq!(state.intent, ProcessingIntent::TranslateEn);
    }

    #[test]
    fn second_tap_after_window_stops_hands_free() {
        let mut state = AppState::new();
        let t0 = Instant::now();

        state.handle_hotkey_pressed_at(t0);
        state.handle_hotkey_released_at(t0 + ms(80));
        assert_eq!(state.session_state, HotkeySessionState::HandsFree);

        state.handle_hotkey_pressed_at(t0 + ms(550));
        state.handle_hotkey_released_at(t0 + ms(620));

        assert_eq!(state.session_state, HotkeySessionState::Finalizing);
        assert_eq!(state.intent, ProcessingIntent::Assistant);
    }

    #[test]
    fn translation_trigger_off_treats_second_tap_as_stop() {
        let mut state = AppState::new();
        state.translation_trigger_mode = TranslationTriggerMode::Off;
        let t0 = Instant::now();

        state.handle_hotkey_pressed_at(t0);
        state.handle_hotkey_released_at(t0 + ms(80));
        assert_eq!(state.session_state, HotkeySessionState::HandsFree);

        state.handle_hotkey_pressed_at(t0 + ms(180));
        state.handle_hotkey_released_at(t0 + ms(240));

        assert_eq!(state.session_state, HotkeySessionState::Finalizing);
    }

    #[test]
    fn long_second_press_in_upgrade_window_falls_back_to_stop() {
        let mut state = AppState::new();
        let t0 = Instant::now();

        state.handle_hotkey_pressed_at(t0);
        state.handle_hotkey_released_at(t0 + ms(80));
        assert_eq!(state.session_state, HotkeySessionState::HandsFree);
        assert_eq!(state.intent, ProcessingIntent::Assistant);

        // Second press happens inside double-tap window but lasts longer than hold threshold.
        state.handle_hotkey_pressed_at(t0 + ms(220));
        state.handle_hotkey_released_at(t0 + ms(1450));

        assert_eq!(state.session_state, HotkeySessionState::Finalizing);
    }
}
