use crate::state::{AppState, HotkeySessionState, RecordingStyle};

use super::super::SessionController;

impl SessionController {
    pub fn on_hotkey_pressed(&self, state: &mut AppState) {
        self.cancel_auto_hide();

        let mut start_hold_timer = None;
        let mut start_recording = false;
        let prev_state = state.session_state;
        state.handle_hotkey_pressed();
        if state.session_state == HotkeySessionState::Pending
            && prev_state == HotkeySessionState::Idle
        {
            start_hold_timer = Some(state.hold_threshold_ms);
            start_recording = true;
        }

        if start_recording {
            let (asr_model_name, llm_model_name) =
                crate::services::history_service::HistoryService::capture_model_snapshot();
            state.session_asr_model_name = asr_model_name;
            state.session_llm_model_name = llm_model_name;

            // Default HUD to hands-free icon immediately; will switch to push-to-talk if hold threshold is reached.
            state.recording_style = Some(RecordingStyle::HandsFree);
            self.start_audio_capture();
            if let Some(hud) = self.hud_service() {
                hud.reset_display();
            } else {
                self.emit_transcript("", false);
            }
        }

        self.show_hud();
        if let Some(threshold) = start_hold_timer {
            self.start_hold_timer(threshold);
        }
        self.emit_state_from(state);
    }

    pub fn on_hotkey_released(&self, state: &mut AppState) {
        self.cancel_hold_timer();

        let prev = state.session_state;
        let was_recording = state.is_recording;
        state.handle_hotkey_released();

        let mut schedule_hands_free = None;
        let mut schedule_finalize = false;
        if state.session_state == HotkeySessionState::HandsFree
            && prev == HotkeySessionState::Pending
        {
            schedule_hands_free = Some(state.max_recording_minutes);
        } else if state.session_state == HotkeySessionState::Finalizing {
            schedule_finalize = true;
        }

        if was_recording && !state.is_recording {
            self.stop_audio_capture("hotkey_release");
        }

        if let Some(minutes) = schedule_hands_free {
            self.start_hands_free_timeout(minutes);
        }

        if schedule_finalize {
            self.emit_countdown(None);
            self.schedule_finalize_cleanup();
        }

        self.emit_state_from(state);
        if state.session_state == HotkeySessionState::Idle {
            self.set_escape_swallowing(false);
        }
    }

    pub fn on_hold_threshold_reached_state(&self, state: &mut AppState) {
        state.on_hold_threshold_reached();
        self.emit_state_from(state);
    }
}
