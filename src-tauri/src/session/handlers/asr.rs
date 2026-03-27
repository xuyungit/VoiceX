use serde_json::json;

use crate::{
    asr::{AsrConfig, AsrEvent, AsrProviderType, ColiAsrClient, ColiRefinementMode},
    state::{AppState, HotkeySessionState, ProcessingIntent},
};

use super::super::SessionController;
use crate::session::utils::preview;
use crate::session::SessionMessage;

impl SessionController {
    pub fn handle_asr_event_state(&self, state: &mut AppState, evt: AsrEvent) {
        if evt.is_final {
            let final_changed = evt.text.trim() != state.last_injected_text.trim();
            // If a new final arrives while injection is in progress, cancel the in-flight one.
            if state.injection_in_progress {
                self.injection_epoch
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.injection_cancel_flag
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                state.injection_in_progress = false;
                state.final_injected = false;
            } else if final_changed {
                state.final_injected = false;
            }

            state.final_version = state.final_version.saturating_add(1);
            state.session_final_text = evt.text.clone();
            state.transcript_text = evt.text.clone();
            state.last_injected_text = evt.text.clone();
            state.has_final_result = true;
            // NOTE: do NOT set asr_stream_finished here — Google STT sends multiple
            // is_final events per stream. The stream is only truly finished when
            // on_asr_stream_finished_state is called via AsrStreamFinished message.
            self.cancel_asr_final_timeout();
            log::info!(
                "ASR final received (len={}, definite={}, prefetch={})",
                evt.text.chars().count(),
                evt.definite,
                evt.prefetch
            );
            log::debug!("ASR final preview: {}", preview(&evt.text));

            // Treat ASR final as the end of recognition and proceed to injection.
            self.maybe_inject_final_state(state);
        } else {
            state.transcript_text = evt.text.clone();
            log::debug!(
                "ASR partial (len={}): {}",
                evt.text.chars().count(),
                preview(&evt.text)
            );
        }
    }

    pub fn spawn_asr(
        &self,
        rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
        sample_rate: u32,
        channels: u16,
    ) {
        let controller = self.clone();
        if let Ok(mut guard) = self.asr_task.lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
            }

            let maybe_hud = self.hud_service();
            let controller_for_events = controller.clone();
            let controller_for_finish = controller.clone();
            let cancel_token = tokio_util::sync::CancellationToken::new();
            if let Ok(mut token_guard) = self.asr_cancel_token.lock() {
                *token_guard = Some(cancel_token.clone());
            }
            let task: tauri::async_runtime::JoinHandle<()> =
                tauri::async_runtime::spawn(async move {
                    if let Some(hud) = maybe_hud {
                        let settings = match crate::storage::get_settings() {
                            Ok(s) => s,
                            Err(e) => {
                                log::warn!("ASR skipped, failed to load settings: {}", e);
                                return;
                            }
                        };
                        let config = AsrConfig::from(&settings);
                        if !config.is_valid() {
                            log::warn!("ASR config invalid, skipping streaming");
                            return;
                        }

                        let asr = crate::services::asr_manager::AsrManager::new();
                        let hud_for_events = hud.clone();
                        let hud_for_recognition = hud.clone();
                        asr.stream(
                            sample_rate,
                            channels,
                            rx,
                            cancel_token,
                            move |evt| {
                                if evt.is_final {
                                    log::debug!("ASR event (final): {}", evt.text);
                                } else {
                                    log::debug!("ASR event (partial): {}", evt.text);
                                }
                                controller_for_events
                                    .send_message(SessionMessage::AsrEvent(evt.clone()));

                                hud_for_events.emit_transcript(&evt.text, evt.is_final);

                                let (event_name, recognition_payload) = if evt.is_final {
                                    ("recognition:final", json!({ "text": evt.text }))
                                } else {
                                    ("recognition:partial", json!({ "text": evt.text }))
                                };

                                hud_for_recognition
                                    .emit_recognition(event_name, recognition_payload);
                            },
                            move || {
                                controller_for_finish
                                    .send_message(SessionMessage::AsrStreamFinished);
                            },
                        )
                        .await;
                    }
                });

            *guard = Some(task);
        }
    }

    pub fn on_asr_stream_finished_state(&self, state: &mut AppState) {
        state.is_recording = false;
        state.asr_stream_finished = true;
        self.cancel_asr_final_timeout();

        // Fallback: use latest transcript as final if none arrived yet
        if !state.has_final_result && !state.transcript_text.is_empty() {
            log::info!("ASR stream finished without final result; using transcript as fallback");
            state.session_final_text = state.transcript_text.clone();
            state.has_final_result = true;
        }

        let has_final = state.has_final_result;
        let final_text_len = state.session_final_text.chars().count();
        let final_text_prev = preview(&state.session_final_text);
        log::info!(
            "ASR stream finished (has_final={}, len={}, refinement_mode_active={})",
            has_final,
            final_text_len,
            self.should_run_coli_refinement(state),
        );
        if !final_text_prev.is_empty() {
            log::debug!("ASR final preview: {}", final_text_prev);
        }

        self.maybe_inject_final_state(state);
    }

    pub fn maybe_inject_final_state(&self, state: &mut AppState) {
        if state.final_injected {
            log::info!("Inject skip: already injected");
            return;
        }
        if state.injection_in_progress {
            log::info!("Inject skip: injection already in progress");
            return;
        }
        if state.asr_refinement_in_progress {
            log::info!("Inject skip: ASR refinement still in progress");
            return;
        }
        if self.should_defer_injection_until_asr_finished(state) {
            log::info!("Inject defer: waiting for ASR stream to finish before local refinement");
            return;
        }
        if self.should_run_coli_refinement(state) && !state.asr_refinement_done {
            self.start_coli_refinement(state);
            return;
        }
        if !state.has_final_result {
            log::info!("Inject skip: no final result yet");
            if state.asr_stream_finished
                && (state.session_state == HotkeySessionState::Finalizing
                    || state.session_state == HotkeySessionState::HandsFree)
            {
                log::info!("ASR finished with no final result; closing HUD");
                self.discard_session_audio_file(state, "no_final_result");
                self.discard_session_refinement_audio_file(state, "no_final_result");
                self.hide_hud_and_reset_state(state);
            }
            return;
        }
        if state.is_recording {
            log::info!("Inject skip: recording still running");
            return;
        }
        if state.session_final_text.is_empty() {
            log::info!("Inject skip: final text empty");
            if state.asr_stream_finished
                && (state.session_state == HotkeySessionState::Finalizing
                    || state.session_state == HotkeySessionState::HandsFree)
            {
                log::info!("ASR finished with empty text; closing HUD");
                self.discard_session_audio_file(state, "empty_final_text");
                self.discard_session_refinement_audio_file(state, "empty_final_text");
                self.hide_hud_and_reset_state(state);
            }
            return;
        }

        let text = state.session_final_text.clone();
        let mode = state.text_injection_mode;
        let duration_ms = state.session_duration_ms;
        let audio_path = state
            .session_audio_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string());
        let recording_style = state.recording_style;
        let intent = state.intent;
        let original_text_for_diff = text.clone();
        let controller = self.clone();
        let injection_epoch = self
            .injection_epoch
            .load(std::sync::atomic::Ordering::SeqCst);
        let injection_version = state.final_version;
        let remove_punctuation = state.remove_trailing_punctuation;
        let threshold = state.short_sentence_threshold;
        let rules = state.replacement_rules.clone();
        // Reset cancel flag for this injection.
        self.injection_cancel_flag
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let cancel_flag = self.injection_cancel_flag.clone();
        // Update local state with the to-be-injected final text so HUD has it if callbacks run before InjectDone.
        state.session_final_text = text.clone();
        state.transcript_text = text.clone();
        state.last_injected_text = text.clone();
        self.emit_transcript(&text, true);
        state.injection_in_progress = true;
        log::info!(
            "Inject attempt: len={}, mode={:?}, duration_ms={:?}, audio_path_set={}, version={}",
            text.chars().count(),
            mode,
            duration_ms,
            audio_path.is_some(),
            injection_version
        );

        tauri::async_runtime::spawn(async move {
            // Check if a newer injection has superseded this one.
            // IMPORTANT: do NOT set cancel_flag here — the flag is shared and may
            // already belong to the newer injection task.
            if controller
                .injection_epoch
                .load(std::sync::atomic::Ordering::SeqCst)
                != injection_epoch
            {
                log::info!(
                    "Injection skipped: epoch changed before correction (v{})",
                    injection_version
                );
                return;
            }
            if cancel_flag.load(std::sync::atomic::Ordering::SeqCst) {
                log::info!("Injection skipped: cancelled before correction");
                return;
            }

            let llm_result = controller.correct_text_if_enabled(&text, intent).await;
            let llm_text = llm_result.text;
            let corrected_by_llm = llm_result.changed;
            let llm_invoked = llm_result.invoked;
            if corrected_by_llm {
                log::info!(
                    "LLM before: \"{}\"",
                    text.chars().take(120).collect::<String>()
                );
                log::info!(
                    "LLM after:  \"{}\"",
                    llm_text.chars().take(120).collect::<String>()
                );
            }
            let (final_text, was_processed) = if intent == ProcessingIntent::Assistant {
                let processed =
                    crate::services::post_processing_service::PostProcessingService::process(
                        &llm_text,
                        remove_punctuation,
                        threshold,
                        &rules,
                    );
                let changed = processed != llm_text;
                (processed, changed)
            } else {
                (llm_text.clone(), false)
            };
            let corrected = corrected_by_llm || was_processed;

            if controller
                .injection_epoch
                .load(std::sync::atomic::Ordering::SeqCst)
                != injection_epoch
            {
                log::info!(
                    "Injection skipped: epoch changed during post-processing (v{})",
                    injection_version
                );
                return;
            }
            if cancel_flag.load(std::sync::atomic::Ordering::SeqCst) {
                log::info!("Injection skipped: cancelled during post-processing");
                return;
            }

            let original_trimmed = original_text_for_diff.trim();
            let final_trimmed = final_text.trim();

            // We use original text for history if either LLM or Post-processing changed the text.
            let original_for_history = if intent == ProcessingIntent::TranslateEn {
                Some(original_trimmed.to_string())
            } else if corrected && final_trimmed != original_trimmed {
                Some(original_trimmed.to_string())
            } else {
                None
            };

            log::info!(
                "Injecting final text (len={}, mode={:?}, corrected_by_llm={}, was_processed={})",
                final_text.chars().count(),
                mode,
                corrected_by_llm,
                was_processed
            );
            log::debug!("Injection preview: {}", preview(&final_text));
            // Guard again before emitting/injecting.
            if cancel_flag.load(std::sync::atomic::Ordering::SeqCst) {
                log::info!("Injection skipped: cancelled before emit");
                return;
            }
            controller.emit_transcript(&final_text, true);
            crate::services::text_injection_service::TextInjectionService::new()
                .inject_background_guarded(mode, final_text.clone(), cancel_flag.clone());
            if cancel_flag.load(std::sync::atomic::Ordering::SeqCst) {
                log::info!("Injection skipped: cancelled before InjectDone");
                return;
            }
            controller.send_message(SessionMessage::InjectDone {
                text: final_text,
                corrected,
                llm_invoked,
                recording_style,
                duration_ms,
                audio_path,
                original_for_history,
                injection_version,
                intent,
            });
        });
    }

    fn should_run_coli_refinement(&self, state: &AppState) -> bool {
        if state.is_recording || !state.asr_stream_finished {
            return false;
        }

        let audio_path = match state
            .session_refinement_audio_path
            .as_ref()
            .or(state.session_audio_path.as_ref())
        {
            Some(path) if path.is_file() => path,
            _ => return false,
        };
        if audio_path.as_os_str().is_empty() {
            return false;
        }

        let settings = match crate::storage::get_settings() {
            Ok(settings) => settings,
            Err(err) => {
                log::warn!("ASR refinement skipped, failed to load settings: {}", err);
                log::warn!(
                    "COLI_REFINE skipped reason=load_settings_failed err={}",
                    err
                );
                return false;
            }
        };
        let config = AsrConfig::from(&settings);
        config.provider_type == AsrProviderType::Coli
            && config.coli_final_refinement_mode != ColiRefinementMode::Off
    }

    fn should_defer_injection_until_asr_finished(&self, state: &AppState) -> bool {
        if state.is_recording || state.asr_stream_finished || state.asr_refinement_done {
            return false;
        }

        let settings = match crate::storage::get_settings() {
            Ok(settings) => settings,
            Err(err) => {
                log::warn!(
                    "Inject defer check skipped, failed to load settings: {}",
                    err
                );
                return false;
            }
        };
        let config = AsrConfig::from(&settings);
        config.provider_type == AsrProviderType::Coli
            && config.coli_final_refinement_mode != ColiRefinementMode::Off
    }

    fn start_coli_refinement(&self, state: &mut AppState) {
        let audio_path = match state
            .session_refinement_audio_path
            .clone()
            .or(state.session_audio_path.clone())
        {
            Some(path) => path,
            None => {
                log::info!("ASR refinement skipped: no audio path available");
                log::info!("COLI_REFINE skipped reason=no_audio_path");
                state.asr_refinement_done = true;
                self.maybe_inject_final_state(state);
                return;
            }
        };

        let settings = match crate::storage::get_settings() {
            Ok(settings) => settings,
            Err(err) => {
                log::warn!("ASR refinement skipped, failed to load settings: {}", err);
                log::warn!(
                    "COLI_REFINE skipped reason=load_settings_failed err={}",
                    err
                );
                state.asr_refinement_done = true;
                self.maybe_inject_final_state(state);
                return;
            }
        };
        let config = AsrConfig::from(&settings);
        if config.coli_final_refinement_mode == ColiRefinementMode::Off {
            log::info!("COLI_REFINE skipped reason=mode_off");
            state.asr_refinement_done = true;
            self.maybe_inject_final_state(state);
            return;
        }

        state.asr_refinement_in_progress = true;
        self.send_message(SessionMessage::CorrectingStart);
        if !state.session_final_text.trim().is_empty() {
            self.emit_transcript(&state.session_final_text, true);
        }
        log::info!(
            "Starting local ASR refinement with model={} on {}",
            config
                .coli_final_refinement_mode
                .display_name()
                .unwrap_or("unknown"),
            audio_path.display()
        );
        log::info!(
            "COLI_REFINE start model={} audio_path={} stream_final_len={}",
            config
                .coli_final_refinement_mode
                .display_name()
                .unwrap_or("unknown"),
            audio_path.display(),
            state.session_final_text.chars().count()
        );

        let controller = self.clone();
        let refinement_epoch = self
            .injection_epoch
            .load(std::sync::atomic::Ordering::SeqCst);
        tauri::async_runtime::spawn(async move {
            let client = ColiAsrClient::new(config);
            match client.refine_file(&audio_path).await {
                Ok(result) => {
                    let (text, model_name) = match result {
                        Some((text, model_name)) => (Some(text), Some(model_name)),
                        None => (None, None),
                    };
                    controller.send_message(SessionMessage::AsrRefinementDone {
                        text,
                        model_name,
                        refinement_epoch,
                    });
                }
                Err(err) => controller.send_message(SessionMessage::AsrRefinementFailed {
                    reason: err.to_string(),
                    refinement_epoch,
                }),
            }
        });
    }

    pub fn on_asr_refinement_done_state(
        &self,
        state: &mut AppState,
        text: Option<String>,
        model_name: Option<String>,
    ) {
        state.asr_refinement_in_progress = false;
        state.asr_refinement_done = true;

        match text.map(|text| text.trim().to_string()) {
            Some(refined) if !refined.is_empty() => {
                log::info!(
                    "ASR refinement completed (len={}, model={})",
                    refined.chars().count(),
                    model_name.as_deref().unwrap_or("unknown")
                );
                log::info!(
                    "COLI_REFINE success model={} refined_len={} stream_replaced=true",
                    model_name.as_deref().unwrap_or("unknown"),
                    refined.chars().count()
                );
                state.session_final_text = refined.clone();
                state.transcript_text = refined.clone();
                state.last_injected_text = refined.clone();
                state.has_final_result = true;
                if let Some(model_name) = model_name {
                    state.session_asr_model_name = Some(format!("Local / coli / {}", model_name));
                }
                self.emit_transcript(&refined, true);
            }
            _ => {
                log::info!("ASR refinement completed with empty result; keeping streaming final");
                log::info!("COLI_REFINE empty_result keeping_stream_final=true");
            }
        }

        self.send_message(SessionMessage::CorrectingStop);
        self.maybe_inject_final_state(state);
    }

    pub fn on_asr_refinement_failed_state(&self, state: &mut AppState, reason: String) {
        log::warn!(
            "ASR refinement failed; falling back to streaming final: {}",
            reason
        );
        log::warn!(
            "COLI_REFINE failed fallback_to_streaming=true reason={}",
            reason
        );
        state.asr_refinement_in_progress = false;
        state.asr_refinement_done = true;
        self.send_message(SessionMessage::CorrectingStop);
        self.maybe_inject_final_state(state);
    }

    // ── Batch ASR ───────────────────────────────────────────────────────

    /// Start batch ASR on the recorded audio file after capture stops.
    pub fn start_batch_asr(&self, state: &mut AppState) {
        let audio_path = match state
            .session_refinement_audio_path
            .clone()
            .or(state.session_audio_path.clone())
        {
            Some(path) => path,
            None => {
                log::warn!("Batch ASR skipped: no audio path available");
                self.hide_hud_and_reset_state(state);
                return;
            }
        };

        let settings = match crate::storage::get_settings() {
            Ok(s) => s,
            Err(e) => {
                log::warn!("Batch ASR skipped, failed to load settings: {}", e);
                self.hide_hud_and_reset_state(state);
                return;
            }
        };
        let config = AsrConfig::from(&settings);
        if !config.is_valid() {
            log::warn!("Batch ASR config invalid, skipping");
            self.hide_hud_and_reset_state(state);
            return;
        }

        // Show recognizing state in HUD.
        if let Some(hud) = self.hud_service() {
            hud.emit_recognizing(true);
        }

        let batch_epoch = self
            .injection_epoch
            .load(std::sync::atomic::Ordering::SeqCst);
        let controller = self.clone();

        log::info!(
            "Starting batch ASR on {} (provider={:?})",
            audio_path.display(),
            config.provider_type
        );

        tauri::async_runtime::spawn(async move {
            match config.provider_type {
                AsrProviderType::Coli => {
                    // Use coli's file-based recognition (refine_file) in batch mode.
                    let mut batch_config = config.clone();
                    // Force SenseVoice for batch recognition if refinement is off.
                    if batch_config.coli_final_refinement_mode == ColiRefinementMode::Off {
                        batch_config.coli_final_refinement_mode = ColiRefinementMode::SenseVoice;
                    }
                    let client = ColiAsrClient::new(batch_config);
                    match client.refine_file(&audio_path).await {
                        Ok(Some((text, model_name))) => {
                            controller.send_message(SessionMessage::BatchAsrDone {
                                text,
                                model_name: Some(format!("Local / coli / {}", model_name)),
                                batch_epoch,
                            });
                        }
                        Ok(None) => {
                            controller.send_message(SessionMessage::BatchAsrFailed {
                                reason: "Batch ASR returned empty result".to_string(),
                                batch_epoch,
                            });
                        }
                        Err(e) => {
                            controller.send_message(SessionMessage::BatchAsrFailed {
                                reason: e.to_string(),
                                batch_epoch,
                            });
                        }
                    }
                }
                _ => {
                    // Future batch providers (Gemini, Cohere, etc.) will be handled here.
                    controller.send_message(SessionMessage::BatchAsrFailed {
                        reason: format!(
                            "Batch mode not supported for provider {:?}",
                            config.provider_type
                        ),
                        batch_epoch,
                    });
                }
            }
        });
    }

    pub fn on_batch_asr_done_state(
        &self,
        state: &mut AppState,
        text: String,
        model_name: Option<String>,
    ) {
        // Clear recognizing indicator.
        if let Some(hud) = self.hud_service() {
            hud.emit_recognizing(false);
        }

        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            log::info!("Batch ASR returned empty result; closing HUD");
            self.discard_session_audio_file(state, "batch_asr_empty");
            self.discard_session_refinement_audio_file(state, "batch_asr_empty");
            self.hide_hud_and_reset_state(state);
            return;
        }

        log::info!(
            "Batch ASR completed (len={}, model={})",
            trimmed.chars().count(),
            model_name.as_deref().unwrap_or("unknown")
        );

        state.session_final_text = trimmed.clone();
        state.transcript_text = trimmed.clone();
        state.last_injected_text = trimmed.clone();
        state.has_final_result = true;
        state.asr_stream_finished = true;
        if let Some(name) = model_name {
            state.session_asr_model_name = Some(name);
        }
        self.emit_transcript(&trimmed, true);
        self.maybe_inject_final_state(state);
    }

    pub fn on_batch_asr_failed_state(&self, state: &mut AppState, reason: String) {
        log::warn!("Batch ASR failed: {}", reason);

        if let Some(hud) = self.hud_service() {
            hud.emit_recognizing(false);
        }

        self.discard_session_audio_file(state, "batch_asr_failed");
        self.discard_session_refinement_audio_file(state, "batch_asr_failed");
        self.hide_hud_and_reset_state(state);
    }
}
