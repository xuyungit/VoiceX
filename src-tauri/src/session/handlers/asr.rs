use serde_json::json;
use std::path::{Path, PathBuf};

use crate::{
    asr::{
        transcribe_audio_path_detailed, AsrConfig, AsrEvent, AsrFailure, AsrProviderType,
        ColiAsrClient, ColiRefinementMode, ElevenLabsTranscriptionClient,
        OpenAITranscriptionClient, QwenTranscriptionClient,
    },
    state::{AppState, HotkeySessionState, ProcessingIntent},
};

use super::super::SessionController;
use crate::logging::preview;
use crate::session::SessionMessage;

impl SessionController {
    pub fn handle_asr_event_state(&self, state: &mut AppState, evt: AsrEvent) {
        if !state.asr_received_event {
            state.asr_received_event = true;
        }
        if state.asr_startup_retry_count > 0 || state.asr_reconnect_in_progress {
            self.clear_asr_error();
            state.asr_startup_retry_count = 0;
            state.asr_reconnect_retry_count = 0;
        }

        let prefix = if state.asr_reconnect_in_progress {
            state.asr_reconnect_prefix_text.as_str()
        } else {
            ""
        };
        let merged_text = if prefix.is_empty() {
            evt.text.clone()
        } else {
            format!("{}{}", prefix, evt.text)
        };

        if evt.is_final {
            let final_changed = merged_text.trim() != state.last_injected_text.trim();
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
            state.session_final_text = merged_text.clone();
            state.transcript_text = merged_text.clone();
            state.last_injected_text = merged_text.clone();
            state.has_final_result = true;
            state.asr_reconnect_in_progress = false;
            // NOTE: do NOT set asr_stream_finished here — Google STT sends multiple
            // is_final events per stream. The stream is only truly finished when
            // on_asr_stream_finished_state is called via AsrStreamFinished message.
            self.cancel_asr_final_timeout();
            self.mark_asr_replay_checkpoint();
            // A debug trace, not an event: one final per sentence is the normal
            // rhythm of a read, and the length that matters is logged once, at
            // stream end. `definite` is always true on this branch — every
            // backend sets it together with `is_final`, and the two-pass path
            // forces it when the last packet arrives — so it is kept only for
            // the case that invariant ever breaks.
            log::debug!(
                "ASR final (len={}, definite={}, prefetch={}): {}",
                merged_text.chars().count(),
                evt.definite,
                evt.prefetch,
                preview(&merged_text)
            );

            // Treat ASR final as the end of recognition and proceed to injection.
            self.maybe_inject_final_state(state);
        } else {
            state.transcript_text = merged_text.clone();
            log::debug!(
                "ASR partial (len={}): {}",
                merged_text.chars().count(),
                preview(&merged_text)
            );
        }
    }

    pub fn spawn_asr(&self, sample_rate: u32, channels: u16) {
        let Some(rx) = self.take_asr_attempt_receiver() else {
            log::warn!("ASR spawn skipped: audio bridge not available");
            return;
        };
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
                            move |error| {
                                controller_for_finish
                                    .send_message(SessionMessage::AsrStreamFinished { error });
                            },
                        )
                        .await;
                    }
                });

            *guard = Some(task);
        }
    }

    pub fn on_asr_stream_finished_state(&self, state: &mut AppState) {
        // ASR stream completion is not the same as microphone capture stopping.
        // Keep the recording flag owned by the hotkey/audio stop path so we do not
        // accidentally skip a required stop_capture() after a reconnect/close race.
        state.asr_stream_finished = true;
        state.asr_reconnect_in_progress = false;
        state.asr_reconnect_retry_count = 0;
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
            self.should_run_post_recording_refinement(state),
        );
        if !final_text_prev.is_empty() {
            log::debug!("ASR final preview: {}", final_text_prev);
        }

        self.maybe_inject_final_state(state);
    }

    pub fn on_asr_stream_failed_state(&self, state: &mut AppState, failure: AsrFailure) {
        log::warn!(
            "ASR stream failed: provider={} phase={} kind={} retryable={} message={}",
            failure.provider.display_name(),
            failure.phase.as_str(),
            failure.kind.as_str(),
            failure.retryable,
            failure.technical_message
        );
        if self.should_retry_asr_startup_failure(state, &failure) {
            state.asr_startup_retry_count = state.asr_startup_retry_count.saturating_add(1);
            state.terminal_asr_failure = Some(failure.clone());
            self.cancel_asr_final_timeout();
            let retry_message = format!(
                "{} 当前服务异常，正在重试 ({}/{})…",
                failure.provider.display_name(),
                state.asr_startup_retry_count,
                super::super::ASR_STARTUP_RETRY_MAX_ATTEMPTS
            );
            self.emit_asr_error(&retry_message);
            self.schedule_asr_startup_retry(state.asr_startup_retry_count);
            return;
        }

        if self.should_retry_asr_reconnect_failure(state, &failure) {
            state.asr_reconnect_retry_count = state.asr_reconnect_retry_count.saturating_add(1);
            state.asr_reconnect_in_progress = true;
            state.asr_reconnect_prefix_text = if state.has_final_result {
                state.session_final_text.clone()
            } else {
                String::new()
            };
            state.transcript_text = state.asr_reconnect_prefix_text.clone();
            state.terminal_asr_failure = Some(failure.clone());
            self.cancel_asr_final_timeout();
            let retry_message = format!(
                "{} 连接中断，正在重试 ({}/{})…",
                failure.provider.display_name(),
                state.asr_reconnect_retry_count,
                super::super::ASR_RECONNECT_MAX_ATTEMPTS
            );
            self.emit_asr_error(&retry_message);
            self.schedule_asr_reconnect_retry(state.asr_reconnect_retry_count);
            return;
        }

        state.terminal_error_message = Some(failure.display_message.clone());
        state.terminal_asr_failure = Some(failure.clone());
        state.has_final_result = false;
        state.asr_stream_finished = true;
        state.asr_reconnect_in_progress = false;
        self.cancel_asr_final_timeout();
        self.cancel_recording_timeout();
        self.cancel_auto_hide();
        self.emit_asr_error(&failure.display_message);

        if state.is_recording {
            self.stop_audio_capture("asr_stream_failed");
        } else {
            self.cancel_audio_level_task();
            self.schedule_error_cleanup();
        }
    }

    fn should_retry_asr_startup_failure(&self, state: &AppState, failure: &AsrFailure) -> bool {
        if !failure.retryable {
            return false;
        }
        if state.terminal_error_message.is_some()
            || state.asr_received_event
            || state.has_final_result
            || state.asr_stream_finished
        {
            return false;
        }
        if !matches!(
            failure.phase,
            crate::asr::AsrPhase::Connect | crate::asr::AsrPhase::Handshake
        ) {
            return false;
        }
        state.asr_startup_retry_count < super::super::ASR_STARTUP_RETRY_MAX_ATTEMPTS
    }

    fn should_retry_asr_reconnect_failure(&self, state: &AppState, failure: &AsrFailure) -> bool {
        if !failure.retryable || state.terminal_error_message.is_some() || state.asr_stream_finished
        {
            return false;
        }
        if state.session_state == HotkeySessionState::Idle {
            return false;
        }
        if !state.asr_received_event {
            return false;
        }
        if state.asr_reconnect_retry_count >= super::super::ASR_RECONNECT_MAX_ATTEMPTS {
            return false;
        }
        true
    }

    /// The "Inject skip/defer" lines below are a decision trace: every one of
    /// them is the ordinary "not yet" branch, hit once per final for the whole
    /// duration of a hands-free read. They stay at debug; what the user or a
    /// bug report needs is the outcome, and each outcome (closing the HUD,
    /// discarding audio, injecting or failing) is logged at info where it
    /// happens.
    pub fn maybe_inject_final_state(&self, state: &mut AppState) {
        if state.final_injected {
            log::debug!("Inject skip: already injected");
            return;
        }
        if state.injection_in_progress {
            log::debug!("Inject skip: injection already in progress");
            return;
        }
        if state.asr_refinement_in_progress {
            log::debug!("Inject skip: ASR refinement still in progress");
            return;
        }
        if self.should_defer_injection_until_asr_finished(state) {
            log::debug!(
                "Inject defer: waiting for ASR stream to finish before post-recording refinement"
            );
            return;
        }
        if !state.asr_refinement_done {
            if let Some(provider) = self.configured_post_recording_refine_provider() {
                if !state.is_recording && state.asr_stream_finished {
                    if should_silent_exit_before_post_recording_refinement(state, provider) {
                        log::info!(
                            "Skipping {} post-recording refinement for probable no-speech session",
                            provider.display_name()
                        );
                        self.discard_session_audio_file(
                            state,
                            "skip_post_recording_refinement_probable_no_speech",
                        );
                        self.discard_session_refinement_audio_file(
                            state,
                            "skip_post_recording_refinement_probable_no_speech",
                        );
                        self.hide_hud_and_reset_state(state);
                        return;
                    }
                    self.start_post_recording_refinement(state);
                    return;
                }
            }
        }
        if !state.has_final_result {
            log::debug!("Inject skip: no final result yet");
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
            log::debug!("Inject skip: recording still running");
            return;
        }
        if state.session_final_text.is_empty() {
            log::debug!("Inject skip: final text empty");
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
        let matched_override = state
            .session_target_app
            .as_ref()
            .and_then(|app| {
                crate::foreground_app::match_text_injection_override(
                    app,
                    &state.text_injection_overrides,
                )
            })
            .cloned();
        let mode = matched_override
            .as_ref()
            .map(|override_item| crate::injector::TextInjectionMode::from_str(&override_item.mode))
            .unwrap_or(state.text_injection_mode);
        // Only overrides explicitly marked `skip_clipboard_restore` (targets that
        // bridge the clipboard over a variable-latency channel, e.g. remote-desktop
        // clients) keep the injected text on the clipboard; ordinary override
        // targets get the normal backup → paste → restore behavior.
        let skip_clipboard_restore_for_override = matched_override
            .as_ref()
            .map(|override_item| override_item.skip_clipboard_restore)
            .unwrap_or(false);
        // Clipboard-bridge targets additionally need the focus round-trip:
        // their Mac-side client only announces the clipboard when it becomes
        // active again, so we bounce activation through VoiceX before pasting
        // (macOS only; injectors on other platforms ignore the pid).
        let focus_roundtrip_pid = if skip_clipboard_restore_for_override {
            state
                .session_target_app
                .as_ref()
                .filter(|app| !app.is_self)
                .map(|app| app.process_id)
        } else {
            None
        };
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
            "Inject attempt: len={}, mode={:?}, duration_ms={:?}, audio_path_set={}, version={}, target_app={:?}, override_match={:?}",
            text.chars().count(),
            mode,
            duration_ms,
            audio_path.is_some(),
            injection_version,
            state
                .session_target_app
                .as_ref()
                .and_then(|app| app.display_name.clone().or_else(|| app.process_name.clone())),
            matched_override.as_ref().map(|override_item| override_item.app_name.clone())
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
            log::info!("Injection preview: \"{}\"", preview(&final_text));
            // Guard again before emitting/injecting.
            if cancel_flag.load(std::sync::atomic::Ordering::SeqCst) {
                log::info!("Injection skipped: cancelled before emit");
                return;
            }
            controller.emit_transcript(&final_text, true);
            let inject_handle =
                crate::services::text_injection_service::TextInjectionService::new()
                    .inject_background_guarded(
                        mode,
                        final_text.clone(),
                        cancel_flag.clone(),
                        skip_clipboard_restore_for_override,
                        focus_roundtrip_pid,
                    );
            // Wait for the blocking injection to complete before signalling InjectDone.
            // This prevents a new injection from starting while the previous one is
            // still writing to the clipboard or typing characters.
            if let Some(handle) = inject_handle {
                let _ = handle.await;
            }
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

    fn should_run_post_recording_refinement(&self, state: &AppState) -> bool {
        if state.is_recording || !state.asr_stream_finished {
            return false;
        }

        self.configured_post_recording_refine_provider().is_some()
    }

    fn should_defer_injection_until_asr_finished(&self, state: &AppState) -> bool {
        if state.is_recording || state.asr_stream_finished || state.asr_refinement_done {
            return false;
        }

        self.configured_post_recording_refine_provider().is_some()
    }

    fn configured_post_recording_refine_provider(&self) -> Option<PostRecordingRefineProvider> {
        let settings = match crate::storage::get_settings() {
            Ok(settings) => settings,
            Err(err) => {
                log::warn!(
                    "Inject defer check skipped, failed to load settings: {}",
                    err
                );
                return None;
            }
        };
        let config = AsrConfig::from(&settings);
        PostRecordingRefineProvider::from_config(&config)
    }

    fn start_post_recording_refinement(&self, state: &mut AppState) {
        let settings = match crate::storage::get_settings() {
            Ok(settings) => settings,
            Err(err) => {
                log::warn!("ASR refinement skipped, failed to load settings: {}", err);
                self.on_asr_refinement_failed_state(state, err.to_string());
                return;
            }
        };
        let config = AsrConfig::from(&settings);
        let Some(provider) = PostRecordingRefineProvider::from_config(&config) else {
            log::info!("ASR_REFINE skipped reason=mode_off");
            state.asr_refinement_done = true;
            self.maybe_inject_final_state(state);
            return;
        };

        let audio_path = match provider.audio_path(state) {
            Some(path) => path,
            None => {
                log::warn!("{} skipped reason=no_audio_path", provider.log_tag());
                state.active_asr_refinement_provider = Some(provider.as_asr_provider_type());
                self.on_asr_refinement_failed_state(state, provider.missing_audio_message());
                return;
            }
        };

        state.asr_refinement_in_progress = true;
        state.active_asr_refinement_provider = Some(provider.as_asr_provider_type());
        if provider.uses_recognizing_hud() {
            state.session_asr_model_name = provider.streaming_model_name(&config);
            self.emit_state_from(state);
        } else {
            self.send_message(SessionMessage::CorrectingStart);
        }
        if !state.session_final_text.trim().is_empty() {
            self.emit_transcript(&state.session_final_text, true);
        }

        let model_name = provider.target_model_name(&config);
        log::info!(
            "Starting {} post-recording refinement with model={} on {}",
            provider.display_name(),
            model_name.as_deref().unwrap_or("unknown"),
            audio_path.display()
        );
        log::info!(
            "{} start model={} audio_path={} stream_final_len={}",
            provider.log_tag(),
            model_name.as_deref().unwrap_or("unknown"),
            audio_path.display(),
            state.session_final_text.chars().count()
        );

        let controller = self.clone();
        let refinement_epoch = self
            .injection_epoch
            .load(std::sync::atomic::Ordering::SeqCst);
        tauri::async_runtime::spawn(async move {
            match provider.refine_file(config, &audio_path).await {
                Ok((text, resolved_model_name)) => {
                    controller.send_message(SessionMessage::AsrRefinementDone {
                        text,
                        model_name: resolved_model_name.or(model_name),
                        refinement_epoch,
                    });
                }
                Err(err) => controller.send_message(SessionMessage::AsrRefinementFailed {
                    reason: err,
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
        let refinement_provider = state.active_asr_refinement_provider.take();
        state.asr_refinement_in_progress = false;
        state.asr_refinement_done = true;

        match text.map(|text| text.trim().to_string()) {
            Some(refined) if !refined.is_empty() => {
                match refinement_provider {
                    Some(AsrProviderType::Coli) => {
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
                    }
                    Some(AsrProviderType::ElevenLabs) => {
                        log::info!(
                            "ElevenLabs batch refine completed (len={}, model={})",
                            refined.chars().count(),
                            model_name.as_deref().unwrap_or("unknown")
                        );
                        log::info!(
                            "ELEVENLABS_REFINE success model={} refined_len={} stream_replaced=true",
                            model_name.as_deref().unwrap_or("unknown"),
                            refined.chars().count()
                        );
                    }
                    Some(AsrProviderType::OpenAI) => {
                        log::info!(
                            "OpenAI batch refine completed (len={}, model={})",
                            refined.chars().count(),
                            model_name.as_deref().unwrap_or("unknown")
                        );
                        log::info!(
                            "OPENAI_REFINE success model={} refined_len={} stream_replaced=true",
                            model_name.as_deref().unwrap_or("unknown"),
                            refined.chars().count()
                        );
                    }
                    Some(AsrProviderType::Qwen) => {
                        log::info!(
                            "Qwen batch refine completed (len={}, model={})",
                            refined.chars().count(),
                            model_name.as_deref().unwrap_or("unknown")
                        );
                        log::info!(
                            "QWEN_REFINE success model={} refined_len={} stream_replaced=true",
                            model_name.as_deref().unwrap_or("unknown"),
                            refined.chars().count()
                        );
                    }
                    _ => {
                        log::info!(
                            "ASR refinement completed (len={}, model={})",
                            refined.chars().count(),
                            model_name.as_deref().unwrap_or("unknown")
                        );
                    }
                }
                state.session_final_text = refined.clone();
                state.transcript_text = refined.clone();
                state.last_injected_text = refined.clone();
                state.has_final_result = true;
                if let Some(model_name) = model_name {
                    state.session_asr_model_name = Some(match refinement_provider {
                        Some(AsrProviderType::Coli) => {
                            format!("Local / coli / {}", model_name)
                        }
                        _ => model_name,
                    });
                }
                self.emit_transcript(&refined, true);
            }
            _ => match refinement_provider {
                Some(AsrProviderType::Coli) => {
                    log::info!(
                        "ASR refinement completed with empty result; keeping streaming final"
                    );
                    log::info!("COLI_REFINE empty_result keeping_stream_final=true");
                }
                Some(AsrProviderType::ElevenLabs) => {
                    log::info!(
                            "ElevenLabs batch refine completed with empty result; keeping streaming final"
                        );
                    log::info!("ELEVENLABS_REFINE empty_result keeping_stream_final=true");
                }
                Some(AsrProviderType::OpenAI) => {
                    log::info!(
                        "OpenAI batch refine completed with empty result; keeping streaming final"
                    );
                    log::info!("OPENAI_REFINE empty_result keeping_stream_final=true");
                }
                Some(AsrProviderType::Qwen) => {
                    log::info!(
                        "Qwen batch refine completed with empty result; keeping streaming final"
                    );
                    log::info!("QWEN_REFINE empty_result keeping_stream_final=true");
                }
                _ => {
                    log::info!(
                        "ASR refinement completed with empty result; keeping streaming final"
                    );
                }
            },
        }

        if matches!(
            refinement_provider,
            Some(AsrProviderType::ElevenLabs)
                | Some(AsrProviderType::OpenAI)
                | Some(AsrProviderType::Qwen)
        ) {
            self.emit_state_from(state);
        } else {
            self.send_message(SessionMessage::CorrectingStop);
        }
        self.maybe_inject_final_state(state);
    }

    pub fn on_asr_refinement_failed_state(&self, state: &mut AppState, reason: String) {
        let refinement_provider = state.active_asr_refinement_provider.take();
        if should_silent_exit_after_empty_refine_failure(state, refinement_provider, &reason) {
            log::info!(
                "Suppressing {} empty refine failure for probable no-speech session",
                refinement_provider
                    .and_then(PostRecordingRefineProvider::from_asr_provider)
                    .map(|provider| provider.display_name())
                    .unwrap_or("post-recording")
            );
            state.asr_refinement_in_progress = false;
            state.asr_refinement_done = true;
            self.discard_session_audio_file(state, "empty_refine_probable_no_speech");
            self.discard_session_refinement_audio_file(state, "empty_refine_probable_no_speech");
            self.hide_hud_and_reset_state(state);
            return;
        }
        match refinement_provider {
            Some(AsrProviderType::Coli) => {
                log::warn!(
                    "ASR refinement failed; falling back to streaming final: {}",
                    reason
                );
                log::warn!(
                    "COLI_REFINE failed fallback_to_streaming=true reason={}",
                    reason
                );
            }
            Some(AsrProviderType::ElevenLabs)
            | Some(AsrProviderType::OpenAI)
            | Some(AsrProviderType::Qwen) => {
                let provider =
                    PostRecordingRefineProvider::from_asr_provider(refinement_provider.unwrap())
                        .expect("post-recording refine provider");
                log::warn!(
                    "{} batch refine failed; falling back to streaming final: {}",
                    provider.display_name(),
                    reason
                );
                log::warn!(
                    "{} failed fallback_to_streaming={} reason={}",
                    provider.log_tag(),
                    has_stream_fallback_result(state),
                    reason
                );
                if has_stream_fallback_result(state) {
                    state.session_asr_model_name =
                        crate::storage::get_settings().ok().and_then(|settings| {
                            provider.streaming_model_name_from_settings(&settings)
                        });
                }
                let message = provider.failure_message(state);
                self.emit_asr_error(message);
                if should_surface_terminal_batch_post_refine_failure(state) {
                    state.asr_refinement_in_progress = false;
                    state.asr_refinement_done = true;
                    state.terminal_error_message = Some(message.to_string());
                    state.terminal_asr_failure = None;
                    self.emit_state_from(state);
                    self.cancel_audio_level_task();
                    self.schedule_error_cleanup();
                    return;
                }
            }
            _ => {
                log::warn!(
                    "ASR refinement failed; falling back to streaming final: {}",
                    reason
                );
            }
        }
        state.asr_refinement_in_progress = false;
        state.asr_refinement_done = true;
        if matches!(
            refinement_provider,
            Some(AsrProviderType::ElevenLabs)
                | Some(AsrProviderType::OpenAI)
                | Some(AsrProviderType::Qwen)
        ) {
            self.emit_state_from(state);
        } else {
            self.send_message(SessionMessage::CorrectingStop);
        }
        self.maybe_inject_final_state(state);
    }

    // ── Batch ASR ───────────────────────────────────────────────────────

    /// Start batch ASR on the recorded audio file after capture stops.
    pub fn start_batch_asr(&self, state: &mut AppState) {
        let settings = match crate::storage::get_settings() {
            Ok(s) => s,
            Err(e) => {
                log::warn!("Batch ASR skipped, failed to load settings: {}", e);
                self.fail_batch_asr_state(state, "无法加载设置，请稍后重试。".to_string());
                return;
            }
        };
        let mut config = AsrConfig::from(&settings);
        if !config.is_valid() {
            log::warn!("Batch ASR config invalid, skipping");
            self.fail_batch_asr_state(
                state,
                build_batch_asr_hud_message(Some(config.provider_type), "Invalid ASR configuration"),
            );
            return;
        }
        if let Err(message) = config.validate_model_selection() {
            log::warn!("Batch ASR model validation failed: {}", message);
            self.fail_batch_asr_state(
                state,
                build_batch_asr_hud_message(Some(config.provider_type), &message),
            );
            return;
        }
        state.session_asr_model_name =
            crate::services::history_service::HistoryService::resolve_asr_model_name(&settings);

        let audio_path = match select_batch_audio_path(state, &config) {
            Some(path) => path,
            None => {
                log::warn!("Batch ASR skipped: no usable audio path available");
                self.fail_batch_asr_state(state, "录音文件缺失，请重试。".to_string());
                return;
            }
        };

        if recording_looks_like_no_speech(
            state,
            &[
                state.session_refinement_audio_path.as_deref(),
                state.session_audio_path.as_deref(),
            ],
        ) {
            log::info!(
                "Skipping batch ASR upload for probable no-speech session (provider={})",
                config.provider_type.display_name()
            );
            self.discard_session_audio_file(state, "skip_batch_asr_probable_no_speech");
            self.discard_session_refinement_audio_file(state, "skip_batch_asr_probable_no_speech");
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
            match transcribe_audio_path_detailed(
                &audio_path,
                &mut config,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            {
                Ok(outcome) if !outcome.text.trim().is_empty() => {
                    controller.send_message(SessionMessage::BatchAsrDone {
                        text: outcome.text,
                        model_name: outcome.model_name,
                        batch_epoch,
                    });
                }
                Ok(_) => {
                    controller.send_message(SessionMessage::BatchAsrFailed {
                        reason: "Batch ASR returned empty result".to_string(),
                        batch_epoch,
                    });
                }
                Err(reason) => {
                    controller.send_message(SessionMessage::BatchAsrFailed {
                        reason,
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
            log::info!("Batch ASR returned empty result; surfacing visible failure");
            let provider = crate::storage::get_settings()
                .ok()
                .map(|settings| AsrConfig::from(&settings).provider_type);
            self.fail_batch_asr_state(
                state,
                build_batch_asr_hud_message(provider, "Batch ASR returned empty result"),
            );
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

        if should_silent_exit_after_empty_batch_failure(state, &reason) {
            log::info!("Suppressing batch empty-result failure for probable no-speech session");
            self.discard_session_audio_file(state, "empty_batch_probable_no_speech");
            self.discard_session_refinement_audio_file(state, "empty_batch_probable_no_speech");
            self.hide_hud_and_reset_state(state);
            return;
        }

        let provider = crate::storage::get_settings()
            .ok()
            .map(|settings| AsrConfig::from(&settings).provider_type);
        self.fail_batch_asr_state(state, build_batch_asr_hud_message(provider, reason.as_str()));
    }

    fn fail_batch_asr_state(&self, state: &mut AppState, message: String) {
        if state.session_audio_path.is_some() {
            let history = crate::services::history_service::HistoryService::new();
            history.persist_failed_asr(
                crate::services::history_service::HistoryService::resolve_mode(
                    state.intent,
                    state.recording_style,
                    false,
                ),
                state.session_duration_ms,
                state
                    .session_audio_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().to_string()),
                state.session_asr_model_name.clone(),
                message.clone(),
                self.app_handle(),
            );
            // Hand off the audio file to persisted history so cleanup does not remove it.
            state.session_audio_path = None;
            self.discard_session_refinement_audio_file(state, "batch_asr_failed_history_persisted");
        }

        state.terminal_error_message = Some(message.clone());
        state.terminal_asr_failure = None;
        state.has_final_result = false;
        state.asr_stream_finished = true;
        self.emit_asr_error(&message);
        self.cancel_audio_level_task();
        self.schedule_error_cleanup();
    }
}

fn select_batch_audio_path(state: &AppState, config: &AsrConfig) -> Option<PathBuf> {
    let opus_path = state
        .session_audio_path
        .clone()
        .filter(|path| path.is_file());
    let refinement_path = state
        .session_refinement_audio_path
        .clone()
        .filter(|path| path.is_file());

    match config.provider_type {
        // These batch providers accept ogg/opus directly. Prefer the compressed
        // capture artifact over the temporary refinement WAV.
        AsrProviderType::Qwen | AsrProviderType::StepAudio => opus_path.or(refinement_path),
        // MiMo only accepts wav/mp3 and caps input at 10 MB, but its client
        // transcodes the Opus capture to compact 16 kHz mono MP3. Prefer that
        // over the much larger 48 kHz refinement WAV.
        AsrProviderType::Mimo => opus_path.or(refinement_path),
        _ => refinement_path.or(opus_path),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PostRecordingRefineProvider {
    Coli,
    ElevenLabs,
    OpenAI,
    Qwen,
}

impl PostRecordingRefineProvider {
    fn from_config(config: &AsrConfig) -> Option<Self> {
        match config.provider_type {
            AsrProviderType::Coli
                if config.coli_realtime
                    && config.coli_final_refinement_mode != ColiRefinementMode::Off =>
            {
                Some(Self::Coli)
            }
            _ if !config.post_recording_batch_refine_enabled() => None,
            AsrProviderType::ElevenLabs => Some(Self::ElevenLabs),
            AsrProviderType::OpenAI => Some(Self::OpenAI),
            AsrProviderType::Qwen => Some(Self::Qwen),
            _ => None,
        }
    }

    fn from_asr_provider(provider: AsrProviderType) -> Option<Self> {
        match provider {
            AsrProviderType::Coli => Some(Self::Coli),
            AsrProviderType::ElevenLabs => Some(Self::ElevenLabs),
            AsrProviderType::OpenAI => Some(Self::OpenAI),
            AsrProviderType::Qwen => Some(Self::Qwen),
            _ => None,
        }
    }

    fn as_asr_provider_type(self) -> AsrProviderType {
        match self {
            Self::Coli => AsrProviderType::Coli,
            Self::ElevenLabs => AsrProviderType::ElevenLabs,
            Self::OpenAI => AsrProviderType::OpenAI,
            Self::Qwen => AsrProviderType::Qwen,
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Coli => "coli",
            Self::ElevenLabs => "ElevenLabs",
            Self::OpenAI => "OpenAI",
            Self::Qwen => "Qwen",
        }
    }

    fn log_tag(self) -> &'static str {
        match self {
            Self::Coli => "COLI_REFINE",
            Self::ElevenLabs => "ELEVENLABS_REFINE",
            Self::OpenAI => "OPENAI_REFINE",
            Self::Qwen => "QWEN_REFINE",
        }
    }

    fn uses_recognizing_hud(self) -> bool {
        !matches!(self, Self::Coli)
    }

    fn audio_path(self, state: &AppState) -> Option<PathBuf> {
        match self {
            Self::Coli => state
                .session_refinement_audio_path
                .clone()
                .or(state.session_audio_path.clone())
                .filter(|path| path.is_file()),
            Self::ElevenLabs | Self::OpenAI | Self::Qwen => {
                match state.session_audio_path.clone() {
                    Some(path) if path.is_file() => Some(path),
                    _ => None,
                }
            }
        }
    }

    fn missing_audio_message(self) -> String {
        match self {
            Self::Coli => "Local ASR refinement audio file is missing".to_string(),
            Self::ElevenLabs => "ElevenLabs batch refine audio file is missing".to_string(),
            Self::OpenAI => "OpenAI batch refine audio file is missing".to_string(),
            Self::Qwen => "Qwen batch refine audio file is missing".to_string(),
        }
    }

    fn empty_result_message(self) -> String {
        match self {
            Self::Coli => "Local ASR refinement returned empty result".to_string(),
            Self::ElevenLabs => "ElevenLabs batch refine returned empty result".to_string(),
            Self::OpenAI => "OpenAI batch refine returned empty result".to_string(),
            Self::Qwen => "Qwen batch refine returned empty result".to_string(),
        }
    }

    fn streaming_model_name(self, config: &AsrConfig) -> Option<String> {
        match self {
            Self::Coli => None,
            Self::ElevenLabs => {
                crate::services::history_service::HistoryService::elevenlabs_realtime_model_name(
                    &config.elevenlabs_realtime_model,
                )
            }
            Self::OpenAI => {
                crate::services::history_service::HistoryService::format_provider_model(
                    "OpenAI Realtime",
                    &config.openai_asr_model,
                )
            }
            Self::Qwen => crate::services::history_service::HistoryService::format_provider_model(
                "Qwen",
                &config.qwen_model,
            ),
        }
    }

    fn streaming_model_name_from_settings(
        self,
        settings: &crate::commands::settings::AppSettings,
    ) -> Option<String> {
        match self {
            Self::Coli => None,
            Self::ElevenLabs => {
                crate::services::history_service::HistoryService::elevenlabs_realtime_model_name(
                    &settings.elevenlabs_realtime_model,
                )
            }
            Self::OpenAI => {
                crate::services::history_service::HistoryService::format_provider_model(
                    "OpenAI Realtime",
                    &settings.openai_asr_model,
                )
            }
            Self::Qwen => crate::services::history_service::HistoryService::format_provider_model(
                "Qwen",
                &settings.qwen_asr_model,
            ),
        }
    }

    fn target_model_name(self, config: &AsrConfig) -> Option<String> {
        match self {
            Self::Coli => config
                .coli_final_refinement_mode
                .display_name()
                .map(|model| model.to_string()),
            Self::ElevenLabs => crate::services::history_service::HistoryService::elevenlabs_realtime_batch_refine_model_name(
                &config.elevenlabs_realtime_model,
                &config.elevenlabs_batch_model,
            ),
            Self::OpenAI => crate::services::history_service::HistoryService::openai_realtime_batch_refine_model_name(
                &config.openai_asr_model, &config.openai_asr_refine_model
            ),
            Self::Qwen => crate::services::history_service::HistoryService::qwen_realtime_batch_refine_model_name(
                &config.qwen_model,
                &config.qwen_batch_model,
            ),
        }
    }

    async fn refine_file(
        self,
        config: AsrConfig,
        audio_path: &PathBuf,
    ) -> Result<(Option<String>, Option<String>), String> {
        match self {
            Self::Coli => {
                let client = ColiAsrClient::new(config);
                match client.refine_file(audio_path).await {
                    Ok(Some((text, model_name))) => Ok((Some(text), Some(model_name))),
                    Ok(None) => Ok((None, None)),
                    Err(err) => Err(err.to_string()),
                }
            }
            Self::ElevenLabs => {
                let client = ElevenLabsTranscriptionClient::new(config);
                let refined = client
                    .transcribe_file(audio_path)
                    .await
                    .map_err(|err| err.to_string())?;
                let refined = refined.trim().to_string();
                if refined.is_empty() {
                    Err(self.empty_result_message())
                } else {
                    Ok((Some(refined), None))
                }
            }
            Self::OpenAI => {
                let client = OpenAITranscriptionClient::new(config.openai_refinement_config());
                let refined = client
                    .transcribe_file(audio_path)
                    .await
                    .map_err(|err| err.to_string())?;
                let refined = refined.trim().to_string();
                if refined.is_empty() {
                    Err(self.empty_result_message())
                } else {
                    Ok((Some(refined), None))
                }
            }
            Self::Qwen => {
                let client = QwenTranscriptionClient::new(config);
                let refined = client
                    .transcribe_file(audio_path)
                    .await
                    .map_err(|err| err.to_string())?;
                let refined = refined.trim().to_string();
                if refined.is_empty() {
                    Err(self.empty_result_message())
                } else {
                    Ok((Some(refined), None))
                }
            }
        }
    }

    fn failure_message(self, state: &AppState) -> &'static str {
        match self {
            Self::Coli => {
                if has_stream_fallback_result(state) {
                    "本地 ASR 精修失败，已保留实时结果"
                } else {
                    "本地 ASR 精修失败，且未保留到实时结果"
                }
            }
            Self::ElevenLabs => {
                if has_stream_fallback_result(state) {
                    "ElevenLabs 精修失败，已保留实时结果"
                } else {
                    "ElevenLabs 精修失败，且未保留到实时结果"
                }
            }
            Self::OpenAI => {
                if has_stream_fallback_result(state) {
                    "OpenAI 精修失败，已保留实时结果"
                } else {
                    "OpenAI 精修失败，且未保留到实时结果"
                }
            }
            Self::Qwen => {
                if has_stream_fallback_result(state) {
                    "Qwen 精修失败，已保留实时结果"
                } else {
                    "Qwen 精修失败，且未保留到实时结果"
                }
            }
        }
    }
}

fn has_stream_fallback_result(state: &AppState) -> bool {
    state.has_final_result && !state.session_final_text.trim().is_empty()
}

fn should_silent_exit_before_post_recording_refinement(
    state: &AppState,
    _provider: PostRecordingRefineProvider,
) -> bool {
    if has_stream_fallback_result(state) || !state.transcript_text.trim().is_empty() {
        return false;
    }

    recording_looks_like_no_speech(
        state,
        &[
            state.session_audio_path.as_deref(),
            state.session_refinement_audio_path.as_deref(),
        ],
    )
}

fn should_silent_exit_after_empty_refine_failure(
    state: &AppState,
    provider: Option<AsrProviderType>,
    reason: &str,
) -> bool {
    let Some(refine_provider) = provider.and_then(PostRecordingRefineProvider::from_asr_provider)
    else {
        return false;
    };
    if reason.trim() != refine_provider.empty_result_message()
        && !is_empty_batch_transcription_reason(reason)
    {
        return false;
    }
    if has_stream_fallback_result(state) || !state.transcript_text.trim().is_empty() {
        return false;
    }

    recording_looks_like_no_speech(
        state,
        &[
            state.session_audio_path.as_deref(),
            state.session_refinement_audio_path.as_deref(),
        ],
    )
}

fn is_empty_batch_transcription_reason(reason: &str) -> bool {
    crate::asr::is_empty_asr_result_message(reason)
}

fn should_silent_exit_after_empty_batch_failure(state: &AppState, reason: &str) -> bool {
    if !is_empty_batch_transcription_reason(reason) {
        return false;
    }

    let Ok(settings) = crate::storage::get_settings() else {
        return false;
    };
    let config = AsrConfig::from(&settings);
    if !config.is_batch() {
        return false;
    }

    recording_looks_like_no_speech(
        state,
        &[
            state.session_refinement_audio_path.as_deref(),
            state.session_audio_path.as_deref(),
        ],
    )
}

fn should_surface_terminal_batch_post_refine_failure(state: &AppState) -> bool {
    !has_stream_fallback_result(state)
}

fn build_batch_asr_hud_message(provider: Option<AsrProviderType>, reason: &str) -> String {
    let Some(provider_type) = provider else {
        return "识别失败，请稍后重试。".to_string();
    };
    AsrFailure::from_raw_reason(provider_type, crate::asr::AsrPhase::Finalizing, reason)
        .display_message
}

fn recording_looks_like_no_speech(state: &AppState, candidate_paths: &[Option<&Path>]) -> bool {
    let duration_ms = state.session_duration_ms.unwrap_or_default();
    if duration_ms > 0 && duration_ms < 800 {
        return true;
    }

    for path in candidate_paths.iter().flatten() {
        match analyze_audio_path(path) {
            Ok(analysis) => {
                log::info!(
                    "No-speech analysis path={} frames={} voiced_frames={} voiced_ms={} max_run={} voiced_ratio={:.3} p95_rms={:.5} peak_rms={:.5} hp_p95_rms={:.5} probable_no_speech={}",
                    path.display(),
                    analysis.frame_count,
                    analysis.voiced_frames,
                    analysis.voiced_duration_ms,
                    analysis.max_voiced_run,
                    analysis.voiced_ratio,
                    analysis.p95_rms,
                    analysis.peak_rms,
                    analysis.hp_p95_rms,
                    analysis.probable_no_speech
                );
                return analysis.probable_no_speech;
            }
            Err(err) => {
                log::debug!(
                    "No-speech analysis unavailable for {}: {}",
                    path.display(),
                    err
                );
            }
        }
    }

    false
}

#[derive(Debug, Clone, Copy)]
struct NoSpeechAnalysis {
    probable_no_speech: bool,
    frame_count: usize,
    voiced_frames: usize,
    voiced_duration_ms: u32,
    max_voiced_run: usize,
    voiced_ratio: f32,
    p95_rms: f32,
    peak_rms: f32,
    hp_p95_rms: f32,
}

fn analyze_audio_path(path: &Path) -> Result<NoSpeechAnalysis, String> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let (samples, sample_rate) = match extension.as_str() {
        "wav" => load_wav_i16_samples(path)?,
        "ogg" | "opus" => (
            bytes_to_i16_samples(&crate::asr::ogg_decoder::decode_ogg_opus_to_pcm16k(path)?),
            16_000,
        ),
        _ => return Err(format!("Unsupported audio format: {}", extension)),
    };

    Ok(analyze_pcm_i16_at_rate(&samples, sample_rate))
}

fn load_wav_i16_samples(path: &Path) -> Result<(Vec<i16>, u32), String> {
    let bytes = std::fs::read(path)
        .map_err(|err| format!("Failed to read wav file {}: {}", path.display(), err))?;
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("Invalid WAV header".to_string());
    }

    let mut cursor = 12usize;
    let mut channels = 1u16;
    let mut sample_rate = 16_000u32;
    let mut bits_per_sample = 16u16;
    let mut pcm_data: Option<&[u8]> = None;

    while cursor + 8 <= bytes.len() {
        let chunk_id = &bytes[cursor..cursor + 4];
        let chunk_size = u32::from_le_bytes([
            bytes[cursor + 4],
            bytes[cursor + 5],
            bytes[cursor + 6],
            bytes[cursor + 7],
        ]) as usize;
        cursor += 8;

        if cursor + chunk_size > bytes.len() {
            return Err("Truncated WAV chunk".to_string());
        }

        match chunk_id {
            b"fmt " => {
                if chunk_size < 16 {
                    return Err("Invalid WAV fmt chunk".to_string());
                }
                let audio_format = u16::from_le_bytes([bytes[cursor], bytes[cursor + 1]]);
                channels = u16::from_le_bytes([bytes[cursor + 2], bytes[cursor + 3]]);
                sample_rate = u32::from_le_bytes([
                    bytes[cursor + 4],
                    bytes[cursor + 5],
                    bytes[cursor + 6],
                    bytes[cursor + 7],
                ]);
                bits_per_sample = u16::from_le_bytes([bytes[cursor + 14], bytes[cursor + 15]]);
                if audio_format != 1 {
                    return Err(format!("Unsupported WAV audio format: {}", audio_format));
                }
            }
            b"data" => {
                pcm_data = Some(&bytes[cursor..cursor + chunk_size]);
            }
            _ => {}
        }

        cursor += chunk_size + (chunk_size % 2);
    }

    if bits_per_sample != 16 {
        return Err(format!(
            "Unsupported WAV bits per sample: {}",
            bits_per_sample
        ));
    }

    let pcm = pcm_data.ok_or_else(|| "WAV data chunk missing".to_string())?;
    let mut samples = bytes_to_i16_samples(pcm);
    if channels > 1 {
        let channel_count = channels as usize;
        samples = samples
            .chunks_exact(channel_count)
            .map(|frame| {
                let sum = frame.iter().map(|sample| *sample as i32).sum::<i32>();
                (sum / channel_count as i32) as i16
            })
            .collect();
    }

    Ok((samples, sample_rate.max(1)))
}

fn bytes_to_i16_samples(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect()
}

#[cfg(test)]
fn analyze_pcm_i16(samples: &[i16]) -> NoSpeechAnalysis {
    analyze_pcm_i16_at_rate(samples, 16_000)
}

fn percentile_sorted(sorted: &[f32], quantile: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len().saturating_sub(1)) as f32 * quantile.clamp(0.0, 1.0)).round() as usize;
    sorted[idx.min(sorted.len().saturating_sub(1))]
}

fn analyze_pcm_i16_at_rate(samples: &[i16], sample_rate: u32) -> NoSpeechAnalysis {
    const FRAME_DURATION_MS: u32 = 20;
    const VOICED_RAW_RMS_FLOOR: f32 = 0.004;
    const VOICED_HP_RMS_FLOOR: f32 = 0.0032;
    const MAX_RAW_NOISE_FLOOR: f32 = 0.015;
    const MAX_HP_NOISE_FLOOR: f32 = 0.0035;
    const RAW_SNR_MULTIPLIER: f32 = 2.5;
    const HP_SNR_MULTIPLIER: f32 = 2.2;
    const EDGE_TRIM_START_FRAMES: usize = 4; // 80 ms hotkey press click
    const EDGE_TRIM_END_FRAMES: usize = 5; // 100 ms hotkey release click
    const MIN_FRAMES_FOR_EDGE_TRIM: usize = 25; // 500 ms
    const MIN_SPEECH_FRAMES: usize = 8; // 160 ms
    const MIN_SPEECH_RUN_FRAMES: usize = 3; // 60 ms contiguous speech

    let safe_rate = sample_rate.max(1) as usize;
    let frame_samples = (safe_rate * (FRAME_DURATION_MS as usize) / 1000).max(1);
    // Keep pre-emphasis time constant (~62.5 us, equivalent to 1 sample at 16 kHz)
    // invariant across 16 kHz, 24 kHz, and 48 kHz capture rates.
    let hp_step = (safe_rate / 16_000).max(1);

    if samples.is_empty() {
        return NoSpeechAnalysis {
            probable_no_speech: true,
            frame_count: 0,
            voiced_frames: 0,
            voiced_duration_ms: 0,
            max_voiced_run: 0,
            voiced_ratio: 0.0,
            p95_rms: 0.0,
            peak_rms: 0.0,
            hp_p95_rms: 0.0,
        };
    }

    let mut frame_rms = Vec::new();
    let mut frame_hp_rms = Vec::new();
    for (frame_idx, frame) in samples.chunks(frame_samples).enumerate() {
        if frame.is_empty() {
            continue;
        }
        let base_offset = frame_idx * frame_samples;
        let mut raw_energy = 0.0f32;
        let mut hp_energy = 0.0f32;

        for (i, sample) in frame.iter().enumerate() {
            let x = *sample as f32 / i16::MAX as f32;
            raw_energy += x * x;

            let global_idx = base_offset + i;
            let prev = if global_idx >= hp_step {
                samples[global_idx - hp_step] as f32 / i16::MAX as f32
            } else {
                x
            };
            let hp = x - 0.97 * prev;
            hp_energy += hp * hp;
        }

        let inv_len = 1.0 / (frame.len() as f32);
        frame_rms.push((raw_energy * inv_len).sqrt());
        frame_hp_rms.push((hp_energy * inv_len).sqrt());
    }

    if frame_rms.is_empty() {
        return NoSpeechAnalysis {
            probable_no_speech: true,
            frame_count: 0,
            voiced_frames: 0,
            voiced_duration_ms: 0,
            max_voiced_run: 0,
            voiced_ratio: 0.0,
            p95_rms: 0.0,
            peak_rms: 0.0,
            hp_p95_rms: 0.0,
        };
    }

    let total_frames = frame_rms.len();
    let (analysis_raw, analysis_hp) = if total_frames > MIN_FRAMES_FOR_EDGE_TRIM {
        let end = total_frames.saturating_sub(EDGE_TRIM_END_FRAMES);
        (
            &frame_rms[EDGE_TRIM_START_FRAMES..end],
            &frame_hp_rms[EDGE_TRIM_START_FRAMES..end],
        )
    } else {
        (frame_rms.as_slice(), frame_hp_rms.as_slice())
    };

    let mut sorted_raw = analysis_raw.to_vec();
    sorted_raw.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let mut sorted_hp = analysis_hp.to_vec();
    sorted_hp.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));

    let raw_p20 = percentile_sorted(&sorted_raw, 0.20).min(MAX_RAW_NOISE_FLOOR);
    let hp_p20 = percentile_sorted(&sorted_hp, 0.20).min(MAX_HP_NOISE_FLOOR);
    let p95_rms = percentile_sorted(&sorted_raw, 0.95);
    let peak_rms = sorted_raw.last().copied().unwrap_or(0.0);
    let hp_p95_rms = percentile_sorted(&sorted_hp, 0.95);

    let raw_voiced_threshold = VOICED_RAW_RMS_FLOOR.max(raw_p20 * RAW_SNR_MULTIPLIER);
    let hp_voiced_threshold = VOICED_HP_RMS_FLOOR.max(hp_p20 * HP_SNR_MULTIPLIER);

    let mut voiced_frames = 0usize;
    let mut current_run = 0usize;
    let mut max_voiced_run = 0usize;

    for (&raw_val, &hp_val) in analysis_raw.iter().zip(analysis_hp.iter()) {
        let is_voiced = hp_val >= hp_voiced_threshold || raw_val >= raw_voiced_threshold;
        if is_voiced {
            voiced_frames += 1;
            current_run += 1;
            if current_run > max_voiced_run {
                max_voiced_run = current_run;
            }
        } else {
            current_run = 0;
        }
    }

    let voiced_duration_ms = (voiced_frames as u32).saturating_mul(FRAME_DURATION_MS);
    let voiced_ratio = voiced_frames as f32 / (analysis_raw.len().max(1) as f32);
    let probable_no_speech =
        voiced_frames < MIN_SPEECH_FRAMES || max_voiced_run < MIN_SPEECH_RUN_FRAMES;

    NoSpeechAnalysis {
        probable_no_speech,
        frame_count: total_frames,
        voiced_frames,
        voiced_duration_ms,
        max_voiced_run,
        voiced_ratio,
        p95_rms,
        peak_rms,
        hp_p95_rms,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        analyze_pcm_i16, analyze_pcm_i16_at_rate, build_batch_asr_hud_message,
        is_empty_batch_transcription_reason, should_silent_exit_after_empty_refine_failure,
        should_silent_exit_before_post_recording_refinement,
        should_surface_terminal_batch_post_refine_failure, PostRecordingRefineProvider,
    };
    use crate::asr::AsrProviderType;
    use crate::state::AppState;

    #[test]
    fn elevenlabs_refine_failure_message_mentions_stream_fallback_when_available() {
        let mut state = AppState::new();
        state.has_final_result = true;
        state.session_final_text = "hello world".to_string();

        assert_eq!(
            PostRecordingRefineProvider::ElevenLabs.failure_message(&state),
            "ElevenLabs 精修失败，已保留实时结果"
        );
    }

    #[test]
    fn elevenlabs_refine_failure_message_mentions_missing_fallback_when_no_final() {
        let state = AppState::new();

        assert_eq!(
            PostRecordingRefineProvider::ElevenLabs.failure_message(&state),
            "ElevenLabs 精修失败，且未保留到实时结果"
        );
    }

    #[test]
    fn elevenlabs_refine_failure_without_stream_result_is_terminal() {
        let state = AppState::new();

        assert!(should_surface_terminal_batch_post_refine_failure(&state));
    }

    #[test]
    fn elevenlabs_refine_failure_with_stream_result_is_not_terminal() {
        let mut state = AppState::new();
        state.has_final_result = true;
        state.session_final_text = "fallback".to_string();

        assert!(!should_surface_terminal_batch_post_refine_failure(&state));
    }

    #[test]
    fn qwen_refine_failure_message_mentions_stream_fallback_when_available() {
        let mut state = AppState::new();
        state.has_final_result = true;
        state.session_final_text = "hello world".to_string();

        assert_eq!(
            PostRecordingRefineProvider::Qwen.failure_message(&state),
            "Qwen 精修失败，已保留实时结果"
        );
    }

    #[test]
    fn qwen_refine_failure_without_stream_result_is_terminal() {
        let state = AppState::new();

        assert!(should_surface_terminal_batch_post_refine_failure(&state));
    }

    #[test]
    fn batch_hud_message_hides_raw_provider_errors() {
        assert_eq!(
            build_batch_asr_hud_message(
                Some(AsrProviderType::StepAudio),
                "StepAudio ASR 失败: Protocol error: StepAudio returned an empty transcription",
            ),
            "StepAudio 未识别到有效语音"
        );
        assert_eq!(
            build_batch_asr_hud_message(
                Some(AsrProviderType::Qwen),
                r#"Qwen ASR 失败: Server error: Qwen HTTP 401: {"code":"InvalidApiKey"}"#,
            ),
            "Qwen ASR 认证失败，请检查 API Key 和相关配置。"
        );
        assert_eq!(
            build_batch_asr_hud_message(
                Some(AsrProviderType::OpenAI),
                "OpenAI ASR 失败: Server error: OpenAI HTTP 429: quota exceeded",
            ),
            "OpenAI 当前请求过多，请稍后再试。"
        );
    }

    #[test]
    fn empty_batch_transcription_reason_matches_provider_variants() {
        assert!(is_empty_batch_transcription_reason(
            "Batch ASR returned empty result"
        ));
        assert!(is_empty_batch_transcription_reason(
            "StepAudio ASR 失败: Protocol error: StepAudio returned an empty transcription"
        ));
        assert!(is_empty_batch_transcription_reason(
            "MiMo ASR 失败: Server error: MiMo returned an empty transcript"
        ));
        assert!(is_empty_batch_transcription_reason(
            "ElevenLabs ASR 失败: Server error: ElevenLabs returned an empty transcription result"
        ));
    }

    #[test]
    fn silence_analysis_marks_flat_signal_as_no_speech() {
        let samples = vec![0i16; 320 * 20];
        let analysis = analyze_pcm_i16(&samples);

        assert!(analysis.probable_no_speech);
        assert_eq!(analysis.frame_count, 20);
        assert_eq!(analysis.peak_rms, 0.0);
    }

    #[test]
    fn silence_analysis_marks_low_freq_rumble_and_edge_clicks_as_no_speech() {
        // Simulate 2 seconds (100 frames) at 16 kHz with steady 100 Hz room rumble
        // (~0.011 RMS, above the old 0.008 threshold) plus a loud hotkey click on the
        // first and last 2 frames. Must be classified as probable_no_speech.
        let frame_samples = 320;
        let total_samples = frame_samples * 100;
        let mut samples = (0..total_samples)
            .map(|idx| {
                let t = idx as f32 / 16_000.0;
                let rumble = (std::f32::consts::TAU * 100.0 * t).sin() * 500.0;
                rumble as i16
            })
            .collect::<Vec<_>>();
        for idx in 0..(frame_samples * 2) {
            samples[idx] = 4000;
        }
        for idx in (total_samples - frame_samples * 2)..total_samples {
            samples[idx] = 4000;
        }

        let analysis = analyze_pcm_i16(&samples);
        assert!(analysis.probable_no_speech);
    }

    #[test]
    fn silence_analysis_keeps_clear_voice_like_signal() {
        let samples = (0..(320 * 20))
            .map(|index| {
                let phase = (index as f32 / 24.0).sin();
                (phase * 4000.0) as i16
            })
            .collect::<Vec<_>>();
        let analysis = analyze_pcm_i16(&samples);

        assert!(!analysis.probable_no_speech);
        assert!(analysis.peak_rms > 0.008);
    }

    #[test]
    fn silence_analysis_preserves_short_quiet_word_in_long_recording() {
        // 10 seconds at 48 kHz (500 frames of 20 ms), with a 240 ms quiet utterance
        // (12 frames at ~200 amplitude, RMS ~0.0043 < 0.008). Must NOT be classified as no-speech.
        let frame_samples = 960; // 20 ms at 48 kHz
        let mut samples = vec![0i16; frame_samples * 500];
        for frame_idx in 100..112 {
            for i in 0..frame_samples {
                let phase = (i as f32 / 24.0).sin();
                samples[frame_idx * frame_samples + i] = (phase * 200.0) as i16;
            }
        }
        let analysis = analyze_pcm_i16_at_rate(&samples, 48_000);

        assert_eq!(analysis.frame_count, 500);
        assert_eq!(analysis.voiced_frames, 12);
        assert_eq!(analysis.voiced_duration_ms, 240);
        assert!(!analysis.probable_no_speech);
    }

    #[test]
    fn post_recording_refine_silent_exit_applies_to_all_refine_providers() {
        let mut state = AppState::new();
        state.session_duration_ms = Some(500);

        for provider in [
            PostRecordingRefineProvider::Coli,
            PostRecordingRefineProvider::ElevenLabs,
            PostRecordingRefineProvider::OpenAI,
            PostRecordingRefineProvider::Qwen,
        ] {
            assert!(should_silent_exit_before_post_recording_refinement(
                &state, provider
            ));
            assert!(should_silent_exit_after_empty_refine_failure(
                &state,
                Some(provider.as_asr_provider_type()),
                &provider.empty_result_message(),
            ));
        }
    }
}
