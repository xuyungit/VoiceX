use crate::session::{InjectionOutcome, ASR_FINAL_WAIT_MS};
use crate::state::{AppState, HotkeySessionState, ProcessingIntent, RecordingStyle};

use super::super::SessionController;

/// Read by the HUD (`hud.ts`, error code messages).
const INJECTION_FAILED_CODE: &str = "injection_failed";
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
        outcome: InjectionOutcome,
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
        state.final_typed = outcome == InjectionOutcome::Dispatched;
        state.injection_in_progress = false;

        let resolved_mode = crate::services::history_service::HistoryService::resolve_mode(
            intent,
            recording_style,
            corrected,
        );

        let history = crate::services::history_service::HistoryService::new();
        let _ = history.persist_with_id(
            *state.session_record_id.get_or_insert_with(uuid::Uuid::new_v4),
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
            match outcome {
                // The text is in: confirm it, then leave without lingering.
                InjectionOutcome::Dispatched => self.confirm_inserted_and_reset_state(state),
                // The text is in history either way; say it did not arrive.
                InjectionOutcome::Failed => self.show_injection_failure(state),
                InjectionOutcome::NotAttempted => self.hide_hud_and_reset_state(state),
            }
        } else if outcome == InjectionOutcome::Failed {
            log::warn!("Injection failed after its session moved on; nothing to report it on");
        }
    }

    /// The final text did not reach the app it was meant for. It shows as an
    /// error for as long as any other, and the HUD names it by a stable code
    /// so it can say so in the UI language.
    fn show_injection_failure(&self, state: &mut AppState) {
        state.terminal_error_message = Some(INJECTION_FAILED_CODE.into());
        self.cancel_audio_level_task();
        self.emit_asr_error(INJECTION_FAILED_CODE);
        self.schedule_error_cleanup();
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
        if state.terminal_error_message.is_some() {
            return; // Only ErrorDisplayDone owns terminal-error cleanup.
        }
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
        if state.final_typed {
            self.confirm_inserted_and_reset_state(state);
        } else {
            self.hide_hud_and_reset_state(state);
        }
    }

    pub fn on_recording_timeout_state(&self, state: &mut AppState) {
        if !state.is_recording {
            log::debug!("Recording timeout fired but capture already stopped; skipping finalize");
            return;
        }
        state.transition_to_finalizing();
        self.stop_audio_capture("recording_timeout");
        self.emit_state_from(state);
        self.emit_countdown(None);
        self.schedule_finalize_cleanup();
    }
}
