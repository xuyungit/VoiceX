use tokio::sync::mpsc::Receiver;

use crate::asr::{
    AsrClient, AsrConfig, AsrEvent, AsrFailure, AsrProviderType, ColiAsrClient, GeminiLiveClient,
    GoogleSttClient, OpenAIRealtimeClient, QwenRealtimeClient, SonioxClient,
};
use crate::storage;

#[derive(Clone, Default)]
pub struct AsrManager;

impl AsrManager {
    pub fn new() -> Self {
        Self
    }

    pub async fn stream(
        &self,
        sample_rate: u32,
        channels: u16,
        rx: Receiver<Vec<u8>>,
        cancel: tokio_util::sync::CancellationToken,
        on_event: impl Fn(AsrEvent) + Send + Sync + 'static,
        on_finished: impl Fn(Option<AsrFailure>) + Send + Sync + 'static,
    ) {
        let settings = match storage::get_settings() {
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

        // Hotword diagnostics
        log::info!(
            "ASR corpus config: {} inline hotwords, online_hotword_id={}, enable_context={}",
            config.hotwords.len(),
            config.online_hotword_id.as_deref().unwrap_or("none"),
            config.enable_context,
        );
        if !config.hotwords.is_empty() {
            let preview: Vec<_> = config
                .hotwords
                .iter()
                .take(10)
                .map(|s| s.as_str())
                .collect();
            log::info!(
                "ASR hotwords preview (first {}): {:?}{}",
                preview.len(),
                preview,
                if config.hotwords.len() > 10 {
                    " ..."
                } else {
                    ""
                }
            );
        }
        if config.online_hotword_id.is_some() && !config.hotwords.is_empty() {
            log::info!(
                "ASR: both online_hotword_id and inline hotwords are set; \
                 inline hotwords have higher priority per API docs"
            );
        }

        let history = if config.enable_context {
            crate::services::history_service::HistoryService::new().get_recent_history(3)
        } else {
            Vec::new()
        };
        if !history.is_empty() {
            log::info!(
                "ASR starting session with {} history records as context",
                history.len()
            );
        }

        let on_event = std::sync::Arc::new(on_event);
        let on_finished = std::sync::Arc::new(on_finished);

        let provider = config.provider_type;
        let result = match provider {
            AsrProviderType::Volcengine => {
                let ws_url = config.ws_url.clone();
                log::info!(
                    "Starting ASR stream [Volcengine] (ws: {}, {} Hz, {} ch, two_pass={}, end_window_size={:?}, force_to_speech_time={:?})",
                    ws_url,
                    sample_rate,
                    channels,
                    config.enable_nonstream,
                    config.end_window_size,
                    config.force_to_speech_time,
                );
                let client = AsrClient::new(config);
                let on_event = on_event.clone();
                client
                    .stream_session(
                        sample_rate,
                        channels,
                        rx,
                        cancel.clone(),
                        history,
                        move |evt| {
                            (on_event)(evt);
                        },
                    )
                    .await
            }
            AsrProviderType::Google => {
                log::info!(
                    "Starting ASR stream [Google] ({} Hz, {} ch, lang={})",
                    sample_rate,
                    channels,
                    config.google_language_code,
                );
                let client = GoogleSttClient::new(config);
                let on_event = on_event.clone();
                client
                    .stream_session(
                        sample_rate,
                        channels,
                        rx,
                        cancel.clone(),
                        history,
                        move |evt| {
                            (on_event)(evt);
                        },
                    )
                    .await
            }
            AsrProviderType::Qwen => {
                log::info!(
                    "Starting ASR stream [Qwen] (ws: {}, {} Hz, {} ch, model={}, lang={})",
                    config.qwen_ws_url,
                    sample_rate,
                    channels,
                    config.qwen_model,
                    if config.qwen_language.trim().is_empty() {
                        "auto"
                    } else {
                        config.qwen_language.as_str()
                    },
                );
                if !config.hotwords.is_empty() {
                    log::info!(
                        "Qwen ASR: sending {} hotwords via corpus.text context biasing",
                        config.hotwords.len()
                    );
                }
                let client = QwenRealtimeClient::new(config);
                let on_event = on_event.clone();
                client
                    .stream_session(
                        sample_rate,
                        channels,
                        rx,
                        cancel.clone(),
                        history,
                        move |evt| {
                            (on_event)(evt);
                        },
                    )
                    .await
            }
            AsrProviderType::Gemini => {
                log::warn!("Gemini ASR is batch-only and should not enter streaming mode");
                Ok(())
            }
            AsrProviderType::GeminiLive => {
                log::info!(
                    "Starting ASR stream [Gemini Live] ({} Hz, {} ch, model={})",
                    sample_rate,
                    channels,
                    config.gemini_live_model,
                );
                let client = GeminiLiveClient::new(config);
                let on_event = on_event.clone();
                client
                    .stream_session(
                        sample_rate,
                        channels,
                        rx,
                        cancel.clone(),
                        history,
                        move |evt| {
                            (on_event)(evt);
                        },
                    )
                    .await
            }
            AsrProviderType::Cohere => {
                log::warn!("Cohere ASR is batch-only and should not enter streaming mode");
                Ok(())
            }
            AsrProviderType::OpenAI => {
                if config.openai_asr_mode != "realtime" {
                    log::warn!("OpenAI ASR is in batch mode and should not enter streaming mode");
                    Ok(())
                } else {
                    log::info!(
                        "Starting ASR stream [OpenAI Realtime] ({} Hz, {} ch, model={}, lang={})",
                        sample_rate,
                        channels,
                        config.openai_asr_model,
                        if config.openai_asr_language.trim().is_empty() {
                            "auto"
                        } else {
                            config.openai_asr_language.as_str()
                        },
                    );
                    let client = OpenAIRealtimeClient::new(config);
                    let on_event = on_event.clone();
                    client
                        .stream_session(
                            sample_rate,
                            channels,
                            rx,
                            cancel.clone(),
                            history,
                            move |evt| {
                                (on_event)(evt);
                            },
                        )
                        .await
                }
            }
            AsrProviderType::Soniox => {
                log::info!(
                    "Starting ASR stream [Soniox] ({} Hz, {} ch, model={}, lang={})",
                    sample_rate,
                    channels,
                    config.soniox_model,
                    config.soniox_language,
                );
                let client = SonioxClient::new(config);
                let on_event = on_event.clone();
                client
                    .stream_session(
                        sample_rate,
                        channels,
                        rx,
                        cancel.clone(),
                        history,
                        move |evt| {
                            (on_event)(evt);
                        },
                    )
                    .await
            }
            AsrProviderType::Coli => {
                let command_path = crate::asr::resolve_coli_command(&config.coli_command_path)
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "not found".to_string());
                log::info!(
                    "Starting ASR stream [coli] (cmd: {}, {} Hz, {} ch, vad={}, interval_ms={})",
                    command_path,
                    sample_rate,
                    channels,
                    config.coli_use_vad,
                    config.coli_asr_interval_ms,
                );
                let client = ColiAsrClient::new(config);
                let on_event = on_event.clone();
                client
                    .stream_session(
                        sample_rate,
                        channels,
                        rx,
                        cancel.clone(),
                        history,
                        move |evt| {
                            (on_event)(evt);
                        },
                    )
                    .await
            }
        };

        let finish_error = match result {
            Err(err) => {
                let failure = AsrFailure::from_error(provider, &err);
                log::error!(
                    "ASR streaming failed: provider={} phase={} kind={} retryable={} message={}",
                    provider.display_name(),
                    failure.phase.as_str(),
                    failure.kind.as_str(),
                    failure.retryable,
                    failure.technical_message,
                );
                cancel.cancel();
                Some(failure)
            }
            Ok(()) => {
                log::info!("ASR stream completed");
                None
            }
        };

        if !cancel.is_cancelled() || finish_error.is_some() {
            (on_finished)(finish_error);
        } else {
            log::debug!("ASR stream cancelled; skipping on_finished callback");
        }
    }
}
