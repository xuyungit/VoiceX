use crate::session::ASR_FINAL_WAIT_MS;
use crate::state::{AppState, HotkeySessionState, ProcessingIntent, RecordingStyle};

use super::super::SessionController;
impl SessionController {
    pub fn handle_inject_done_state(
        &self,
        state: &mut AppState,
        text: String,
        corrected: bool,
        llm_invoked: bool,
        recording_style: Option<RecordingStyle>,
        duration_ms: Option<u64>,
        audio_path: Option<String>,
        original_for_history: Option<String>,
        injection_version: u64,
        intent: ProcessingIntent,
    ) {
        // If a newer final arrived while this injection was running, redo injection with the latest text.
        if state.final_version > injection_version {
            log::info!(
                "Injection result stale (version {}), newer final {} exists; re-injecting",
                injection_version,
                state.final_version
            );
            state.injection_in_progress = false;
            state.final_injected = false;
            self.maybe_inject_final_state(state);
            return;
        }
        state.final_injected = true;
        state.injection_in_progress = false;

        let resolved_mode = match (intent, recording_style) {
            (ProcessingIntent::Assistant, Some(RecordingStyle::PushToTalk)) => {
                "assistant_push_to_talk".to_string()
            }
            (ProcessingIntent::Assistant, Some(RecordingStyle::HandsFree)) => {
                "assistant_hands_free".to_string()
            }
            (ProcessingIntent::TranslateEn, Some(RecordingStyle::HandsFree)) => {
                "translate_en_hands_free".to_string()
            }
            (ProcessingIntent::TranslateEn, Some(RecordingStyle::PushToTalk)) => {
                "translate_en_push_to_talk".to_string()
            }
            (ProcessingIntent::Assistant, None) => {
                if corrected {
                    "assistant_corrected".to_string()
                } else {
                    "assistant_raw".to_string()
                }
            }
            (ProcessingIntent::TranslateEn, None) => "translate_en".to_string(),
        };

        let history = crate::services::history_service::HistoryService::new();
        history.persist(
            text,
            original_for_history,
            corrected,
            llm_invoked,
            resolved_mode,
            duration_ms,
            audio_path,
            state.session_asr_model_name.clone(),
            crate::services::history_service::HistoryService::llm_model_for_record(
                llm_invoked,
                state.session_llm_model_name.clone(),
            ),
            self.app_handle(),
        );

        // The audio path has been handed off to history persistence; clear state so that
        // the safety-net discard in hide_hud_and_reset_state does not remove a file that
        // is now tracked by the database.
        state.session_audio_path = None;
        self.discard_session_refinement_audio_file(state, "refinement_complete");

        state.injection_in_progress = false;
        if state.session_state == HotkeySessionState::Finalizing {
            // We've displayed final text; hide/reset immediately to avoid lingering HUD.
            self.hide_hud_and_reset_state(state);
        }
    }

    pub fn on_asr_final_timeout(&self, state: &mut AppState) {
        if state.asr_stream_finished {
            return;
        }
        // Fallback: use latest transcript as final if none arrived.
        if !state.has_final_result && !state.transcript_text.is_empty() {
            state.session_final_text = state.transcript_text.clone();
            state.has_final_result = true;
        }
        log::warn!(
            "ASR final timeout reached ({} ms); proceeding with latest text (len={}, has_final={})",
            ASR_FINAL_WAIT_MS,
            state.session_final_text.chars().count(),
            state.has_final_result
        );
        state.asr_stream_finished = true;
        self.maybe_inject_final_state(state);
    }

    pub fn on_finalize_hide_ready_state(&self, state: &mut AppState) {
        // If still correcting, defer hide/reset until correction finishes.
        if state.session_state == HotkeySessionState::Finalizing {
            if !state.asr_stream_finished {
                log::info!("Finalize hide deferred; ASR stream still active");
                self.schedule_finalize_cleanup();
                return;
            }
            if state.asr_refinement_in_progress {
                log::info!("Finalize hide deferred; ASR refinement still in progress");
                self.schedule_finalize_cleanup();
                return;
            }
            if state.is_correcting {
                log::info!("Finalize hide deferred; correction still active");
                self.schedule_finalize_cleanup();
                return;
            }
            if state.injection_in_progress {
                log::info!("Finalize hide deferred; injection still in progress");
                self.schedule_finalize_cleanup();
                return;
            }
            if state.has_final_result && !state.final_injected {
                log::info!("Finalize hide deferred; final result pending injection");
                self.schedule_finalize_cleanup();
                return;
            }
        }
        // Hide the HUD before resetting state so users don't see the placeholder flash.
        self.hide_hud_and_reset_state(state);
    }

    pub fn on_hands_free_timeout_state(&self, state: &mut AppState) {
        if !state.is_recording {
            log::debug!("Hands-free timeout fired but capture already stopped; skipping finalize");
            return;
        }
        state.transition_to_finalizing();
        self.stop_audio_capture("hands_free_timeout");
        self.emit_state_from(state);
        self.emit_countdown(None);
        self.schedule_finalize_cleanup();
    }
}
