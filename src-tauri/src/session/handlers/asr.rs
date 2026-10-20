use serde_json::json;
use std::path::PathBuf;

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
                    // The manager owns validation and always reports it through
                    // on_finished. A HUD is an observer, not a startup precondition.
                    let asr = crate::services::asr_manager::AsrManager::new();
                    let result = asr
                        .stream(
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

                                if let Some(hud) = &maybe_hud {
                                    hud.emit_transcript(&evt.text, evt.is_final);
                                    let event_name = if evt.is_final {
                                        "recognition:final"
                                    } else {
                                        "recognition:partial"
                                    };
                                    hud.emit_recognition(event_name, json!({ "text": evt.text }));
                                }
                            },
                            move |error| {
                                controller_for_finish
                                    .send_message(SessionMessage::AsrStreamFinished { error });
                            },
                        )
                        .await;
                    if let Err(reason) = result {
                        controller.send_message(SessionMessage::AsrStartupFailed { reason });
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

        state.terminal_asr_failure = Some(failure.clone());
        self.finish_asr_failure_state(state, failure.display_message);
    }

    pub fn on_asr_startup_failed_state(&self, state: &mut AppState, reason: String) {
        log::warn!("ASR startup failed before provider selection: {}", reason);
        state.terminal_asr_failure = None;
        self.finish_asr_failure_state(state, "无法加载识别设置，请稍后重试。".into());
    }

    fn finish_asr_failure_state(&self, state: &mut AppState, message: String) {
        state.terminal_error_message = Some(message.clone());
        state.has_final_result = false;
        state.asr_stream_finished = true;
        state.asr_reconnect_in_progress = false;
        self.cancel_hold_timer();
        self.cancel_asr_final_timeout();
        self.cancel_recording_timeout();
        self.cancel_auto_hide();

        if state.is_recording {
            self.stop_audio_capture("asr_stream_failed");
        }
        state.transition_to_finalizing();
        self.stop_asr_audio_bridge();
        self.cancel_audio_level_task();
        self.emit_state_from(state);
        self.emit_asr_error(&message);
        self.schedule_error_cleanup();
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
            if self.configured_post_recording_refine_provider().is_some() {
                if !state.is_recording && state.asr_stream_finished {
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
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .wrapping_add(1);
        let injection_version = state.final_version;
        let remove_punctuation = state.remove_trailing_punctuation;
        let threshold = state.short_sentence_threshold;
        let rules = state.replacement_rules.clone();
        // Reset cancel flag for this injection.
        self.injection_cancel_flag
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let cancel_flag = self.injection_cancel_flag.clone();
        let injection_guard = {
            let cancelled = cancel_flag.clone();
            let epoch = self.injection_epoch.clone();
            crate::injector::InjectionGuard::new(move || {
                cancelled.load(std::sync::atomic::Ordering::SeqCst)
                    || epoch.load(std::sync::atomic::Ordering::SeqCst) != injection_epoch
            })
            .with_target(state.session_target_app.as_ref().map(|app| app.process_id))
        };
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
                        injection_guard,
                        skip_clipboard_restore_for_override,
                        focus_roundtrip_pid,
                    );
            // Wait for the blocking injection to complete before signalling InjectDone.
            // This prevents a new injection from starting while the previous one is
            // still writing to the clipboard or typing characters.
            if let Some(handle) = inject_handle {
                let _ = handle.await;
            }
            if cancel_flag.load(std::sync::atomic::Ordering::SeqCst)
                || controller
                    .injection_epoch
                    .load(std::sync::atomic::Ordering::SeqCst)
                    != injection_epoch
            {
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
        if text.as_ref().is_none_or(|text| text.trim().is_empty()) {
            if let Some(provider) = state
                .active_asr_refinement_provider
                .and_then(PostRecordingRefineProvider::from_asr_provider)
            {
                self.on_asr_refinement_failed_state(state, provider.empty_result_message());
                return;
            }
        }
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
            _ => log::warn!("ASR refinement returned no text without an active provider"),
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
        let provider = refinement_provider.and_then(PostRecordingRefineProvider::from_asr_provider);
        // Always retain the original diagnostic, including empty-result reasons.
        log::warn!(
            "ASR refinement failed: provider={:?} fallback_to_streaming={} original_error={}",
            refinement_provider,
            has_stream_fallback_result(state),
            reason
        );
        let message = provider.map(|provider| provider.failure_message_for_reason(state, &reason));
        if has_stream_fallback_result(state) {
            if let Some(provider) = provider.filter(|provider| provider.uses_recognizing_hud()) {
                state.session_asr_model_name = crate::storage::get_settings()
                    .ok()
                    .and_then(|settings| provider.streaming_model_name_from_settings(&settings));
            }
        }
        state.asr_refinement_in_progress = false;
        state.asr_refinement_done = true;
        if let Some(message) = message
            .as_ref()
            .filter(|_| should_surface_terminal_batch_post_refine_failure(state))
        {
            state.terminal_error_message = Some(message.clone());
            state.terminal_asr_failure = None;
            self.emit_state_from(state);
            // Error must be the last HUD event; ordinary state events reset its mode.
            self.emit_asr_error(message);
            self.cancel_audio_level_task();
            self.schedule_error_cleanup();
            return;
        }
        if provider.is_some_and(|provider| provider.uses_recognizing_hud()) {
            self.emit_state_from(state);
        } else {
            self.send_message(SessionMessage::CorrectingStop);
        }
        if let Some(message) = message {
            self.emit_asr_error(&message);
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
        if let Err(failure) = config.validate_for_recognition(crate::asr::AsrPhase::Finalizing) {
            failure.log("Batch ASR configuration validation failed");
            self.fail_batch_asr_state(state, failure.display_message);
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

        let provider = crate::storage::get_settings()
            .ok()
            .map(|settings| AsrConfig::from(&settings).provider_type);
        self.fail_batch_asr_state(
            state,
            build_batch_asr_hud_message(provider, reason.as_str()),
        );
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

    fn failure_message_for_reason(self, state: &AppState, reason: &str) -> String {
        if crate::asr::is_empty_asr_result_message(reason) {
            return if has_stream_fallback_result(state) {
                format!(
                    "{} 精修未识别到有效语音，已保留实时结果。",
                    self.display_name()
                )
            } else {
                AsrFailure::from_raw_reason(
                    self.as_asr_provider_type(),
                    crate::asr::AsrPhase::Finalizing,
                    reason,
                )
                .display_message
            };
        }
        let failure = AsrFailure::from_raw_reason(
            self.as_asr_provider_type(),
            crate::asr::AsrPhase::Finalizing,
            reason,
        );
        if failure.kind == crate::asr::AsrFailureKind::AudioFile {
            return if has_stream_fallback_result(state) {
                format!("{} 已保留实时结果。", failure.display_message)
            } else {
                failure.display_message
            };
        }
        self.failure_message(state).to_string()
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

#[cfg(test)]
mod tests {
    #[test]
    fn empty_refinement_reports_no_speech_and_keeps_available_stream_text() {
        for provider in [
            PostRecordingRefineProvider::Qwen,
            PostRecordingRefineProvider::ElevenLabs,
            PostRecordingRefineProvider::OpenAI,
        ] {
            for has_stream_text in [false, true] {
                for empty_completion in [false, true] {
                    let controller = crate::session::SessionController::default();
                    let mut state = AppState::new();
                    state.transition_to_finalizing();
                    state.active_asr_refinement_provider = Some(provider.as_asr_provider_type());
                    state.asr_refinement_in_progress = true;
                    state.asr_stream_finished = true;
                    state.has_final_result = has_stream_text;
                    if has_stream_text {
                        state.session_final_text = "已有实时文本".into();
                        // Leave OS text injection outside this handler regression.
                        state.final_injected = true;
                    }
                    if empty_completion {
                        controller.on_asr_refinement_done_state(&mut state, Some(" ".into()), None);
                    } else {
                        controller.on_asr_refinement_failed_state(&mut state, provider.empty_result_message());
                    }
                    assert!(!state.asr_refinement_in_progress && state.asr_refinement_done);
                    assert!(state.active_asr_refinement_provider.is_none());
                    if has_stream_text {
                        assert!(state.has_final_result && state.terminal_error_message.is_none());
                        assert_eq!(state.session_final_text, "已有实时文本");
                        assert!(provider.failure_message_for_reason(&state, &provider.empty_result_message())
                            .contains("已保留实时结果"));
                    } else {
                        let message = state.terminal_error_message.as_deref().unwrap();
                        assert!(message.contains("未识别到有效语音"));
                        assert!(!message.contains("失败"));
                    }
                }
            }
        }
    }

    use super::{
        build_batch_asr_hud_message, should_surface_terminal_batch_post_refine_failure,
        PostRecordingRefineProvider,
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
            "StepAudio 未识别到有效语音，请检查录音或尝试其他模型。"
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
}
