use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::asr::{
    AsrClient, AsrConfig, AsrEvent, AsrProviderType, CohereTranscriptionClient, ColiAsrClient,
    ColiRefinementMode, ElevenLabsRealtimeClient, ElevenLabsRecognitionMode,
    ElevenLabsTranscriptionClient, FunAsrRealtimeClient, GeminiLiveClient,
    GeminiTranscriptionClient, GoogleSttClient, OpenAIRealtimeClient, OpenAITranscriptionClient,
    QwenRealtimeClient, QwenRecognitionMode, QwenTranscriptionClient, SonioxClient,
    StepAudioTranscriptionClient,
};
use crate::services::history_service::HistoryService;

/// 100ms of 16kHz mono i16 = 16000 * 2 * 0.1 = 3200 bytes
const CHUNK_BYTES: usize = 3200;

#[derive(Debug)]
pub struct AsrTranscriptionOutcome {
    pub text: String,
    pub model_name: Option<String>,
}

pub async fn transcribe_audio_path(
    path: &PathBuf,
    config: &mut AsrConfig,
    cancel: CancellationToken,
) -> Result<String, String> {
    transcribe_audio_path_detailed(path, config, cancel)
        .await
        .map(|outcome| outcome.text)
}

pub async fn transcribe_audio_path_detailed(
    path: &PathBuf,
    config: &mut AsrConfig,
    cancel: CancellationToken,
) -> Result<AsrTranscriptionOutcome, String> {
    match config.provider_type {
        AsrProviderType::Coli => run_coli_asr(path, config).await,
        AsrProviderType::FunAsr => run_streaming_asr(path, config, cancel).await,
        AsrProviderType::Qwen => run_qwen_asr(path, config).await,
        AsrProviderType::Gemini => run_gemini_asr(path, config).await,
        AsrProviderType::GeminiLive => run_streaming_asr(path, config, cancel).await,
        AsrProviderType::Cohere => run_cohere_asr(path, config).await,
        AsrProviderType::OpenAI => run_openai_asr(path, config).await,
        AsrProviderType::ElevenLabs => run_elevenlabs_asr(path, config).await,
        AsrProviderType::StepAudio => run_stepaudio_asr(path, config).await,
        AsrProviderType::Google => run_google_asr(path, config, cancel).await,
        _ => run_streaming_asr(path, config, cancel).await,
    }
}

async fn run_stepaudio_asr(
    path: &PathBuf,
    config: &AsrConfig,
) -> Result<AsrTranscriptionOutcome, String> {
    let client = StepAudioTranscriptionClient::new(config.clone());
    let text = client
        .transcribe_file(path)
        .await
        .map_err(|e| format!("StepAudio ASR 失败: {}", e))?;
    Ok(AsrTranscriptionOutcome {
        text,
        model_name: HistoryService::format_provider_model("StepAudio", &config.stepaudio_model),
    })
}

async fn run_qwen_asr(
    path: &PathBuf,
    config: &AsrConfig,
) -> Result<AsrTranscriptionOutcome, String> {
    if config.qwen_recognition_mode == QwenRecognitionMode::Batch {
        let client = QwenTranscriptionClient::new(config.clone());
        let text = client
            .transcribe_file(path)
            .await
            .map_err(|e| format!("Qwen ASR 失败: {}", e))?;
        return Ok(AsrTranscriptionOutcome {
            text,
            model_name: HistoryService::qwen_batch_model_name(&config.qwen_batch_model),
        });
    }

    let streaming = run_streaming_asr(path, config, CancellationToken::new()).await?;
    let trimmed_streaming = streaming.text.trim().to_string();
    if trimmed_streaming.is_empty() {
        return Err("Qwen Realtime 返回空结果".into());
    }

    if !config.post_recording_batch_refine_enabled() {
        return Ok(AsrTranscriptionOutcome {
            text: trimmed_streaming,
            model_name: HistoryService::format_provider_model("Qwen", &config.qwen_model),
        });
    }

    let client = QwenTranscriptionClient::new(config.clone());
    match client.transcribe_file(path).await {
        Ok(refined) => {
            let refined = refined.trim().to_string();
            if refined.is_empty() {
                log::warn!(
                    "Qwen batch refine returned empty result during transcription; falling back to realtime transcript"
                );
                Ok(AsrTranscriptionOutcome {
                    text: trimmed_streaming,
                    model_name: HistoryService::format_provider_model("Qwen", &config.qwen_model),
                })
            } else {
                Ok(AsrTranscriptionOutcome {
                    text: refined,
                    model_name: HistoryService::qwen_realtime_batch_refine_model_name(
                        &config.qwen_model,
                        &config.qwen_batch_model,
                    ),
                })
            }
        }
        Err(err) => {
            log::warn!(
                "Qwen batch refine failed during transcription; falling back to realtime transcript: {}",
                err
            );
            Ok(AsrTranscriptionOutcome {
                text: trimmed_streaming,
                model_name: HistoryService::format_provider_model("Qwen", &config.qwen_model),
            })
        }
    }
}

async fn run_coli_asr(
    path: &PathBuf,
    config: &mut AsrConfig,
) -> Result<AsrTranscriptionOutcome, String> {
    if config.coli_final_refinement_mode == ColiRefinementMode::Off {
        config.coli_final_refinement_mode = ColiRefinementMode::SenseVoice;
    }

    let client = ColiAsrClient::new(config.clone());
    match client.refine_file(path).await {
        Ok(Some((text, model_name))) => Ok(AsrTranscriptionOutcome {
            text,
            model_name: Some(format!("Local / coli / {}", model_name)),
        }),
        Ok(None) => Err("Coli ASR 返回空结果".into()),
        Err(e) => Err(format!("Coli ASR 失败: {}", e)),
    }
}

async fn run_gemini_asr(
    path: &PathBuf,
    config: &AsrConfig,
) -> Result<AsrTranscriptionOutcome, String> {
    let client = GeminiTranscriptionClient::new(config.clone());
    let text = client
        .transcribe_file(path)
        .await
        .map_err(|e| format!("Gemini ASR 失败: {}", e))?;
    Ok(AsrTranscriptionOutcome {
        text,
        model_name: HistoryService::format_provider_model("Gemini", &config.gemini_model),
    })
}

async fn run_cohere_asr(
    path: &PathBuf,
    config: &AsrConfig,
) -> Result<AsrTranscriptionOutcome, String> {
    let client = CohereTranscriptionClient::new(config.clone());
    let text = client
        .transcribe_file(path)
        .await
        .map_err(|e| format!("Cohere ASR 失败: {}", e))?;
    Ok(AsrTranscriptionOutcome {
        text,
        model_name: HistoryService::format_provider_model("Cohere", &config.cohere_model),
    })
}

async fn run_openai_asr(
    path: &PathBuf,
    config: &AsrConfig,
) -> Result<AsrTranscriptionOutcome, String> {
    if config.openai_asr_mode == "realtime" {
        let streaming = run_streaming_asr(path, config, CancellationToken::new()).await?;
        let trimmed_streaming = streaming.text.trim().to_string();
        if trimmed_streaming.is_empty() {
            return Err("OpenAI Realtime 返回空结果".into());
        }

        if !config.post_recording_batch_refine_enabled() {
            return Ok(AsrTranscriptionOutcome {
                text: trimmed_streaming,
                model_name: HistoryService::format_provider_model(
                    "OpenAI Realtime",
                    &config.openai_asr_model,
                ),
            });
        }

        let client = OpenAITranscriptionClient::new(config.clone());
        return match client.transcribe_file(path).await {
            Ok(refined) => {
                let refined = refined.trim().to_string();
                if refined.is_empty() {
                    log::warn!(
                        "OpenAI batch refine returned empty result during transcription; falling back to realtime transcript"
                    );
                    Ok(AsrTranscriptionOutcome {
                        text: trimmed_streaming,
                        model_name: HistoryService::format_provider_model(
                            "OpenAI Realtime",
                            &config.openai_asr_model,
                        ),
                    })
                } else {
                    Ok(AsrTranscriptionOutcome {
                        text: refined,
                        model_name: HistoryService::openai_realtime_batch_refine_model_name(
                            &config.openai_asr_model,
                        ),
                    })
                }
            }
            Err(err) => {
                log::warn!(
                    "OpenAI batch refine failed during transcription; falling back to realtime transcript: {}",
                    err
                );
                Ok(AsrTranscriptionOutcome {
                    text: trimmed_streaming,
                    model_name: HistoryService::format_provider_model(
                        "OpenAI Realtime",
                        &config.openai_asr_model,
                    ),
                })
            }
        };
    }

    let client = OpenAITranscriptionClient::new(config.clone());
    let text = client
        .transcribe_file(path)
        .await
        .map_err(|e| format!("OpenAI ASR 失败: {}", e))?;
    Ok(AsrTranscriptionOutcome {
        text,
        model_name: HistoryService::format_provider_model("OpenAI", &config.openai_asr_model),
    })
}

async fn run_elevenlabs_asr(
    path: &PathBuf,
    config: &AsrConfig,
) -> Result<AsrTranscriptionOutcome, String> {
    match config.elevenlabs_recognition_mode {
        ElevenLabsRecognitionMode::Batch => {
            let client = ElevenLabsTranscriptionClient::new(config.clone());
            let text = client
                .transcribe_file(path)
                .await
                .map_err(|e| format!("ElevenLabs ASR 失败: {}", e))?;
            Ok(AsrTranscriptionOutcome {
                text,
                model_name: HistoryService::elevenlabs_batch_model_name(
                    &config.elevenlabs_batch_model,
                ),
            })
        }
        ElevenLabsRecognitionMode::Realtime => {
            let streaming = run_streaming_asr(path, config, CancellationToken::new()).await?;
            let trimmed_streaming = streaming.text.trim().to_string();
            if trimmed_streaming.is_empty() {
                return Err("ElevenLabs Realtime 返回空结果".into());
            }

            if !config.post_recording_batch_refine_enabled() {
                return Ok(AsrTranscriptionOutcome {
                    text: trimmed_streaming,
                    model_name: HistoryService::elevenlabs_realtime_model_name(
                        &config.elevenlabs_realtime_model,
                    ),
                });
            }

            let client = ElevenLabsTranscriptionClient::new(config.clone());
            match client.transcribe_file(path).await {
                Ok(refined) => {
                    let refined = refined.trim().to_string();
                    if refined.is_empty() {
                        log::warn!(
                            "ElevenLabs batch refine returned empty result during transcription; falling back to realtime transcript"
                        );
                        Ok(AsrTranscriptionOutcome {
                            text: trimmed_streaming,
                            model_name: HistoryService::elevenlabs_realtime_model_name(
                                &config.elevenlabs_realtime_model,
                            ),
                        })
                    } else {
                        Ok(AsrTranscriptionOutcome {
                            text: refined,
                            model_name: HistoryService::elevenlabs_realtime_batch_refine_model_name(
                                &config.elevenlabs_realtime_model,
                                &config.elevenlabs_batch_model,
                            ),
                        })
                    }
                }
                Err(err) => {
                    log::warn!(
                        "ElevenLabs batch refine failed during transcription; falling back to realtime transcript: {}",
                        err
                    );
                    Ok(AsrTranscriptionOutcome {
                        text: trimmed_streaming,
                        model_name: HistoryService::elevenlabs_realtime_model_name(
                            &config.elevenlabs_realtime_model,
                        ),
                    })
                }
            }
        }
    }
}

async fn run_google_asr(
    path: &PathBuf,
    config: &AsrConfig,
    cancel: CancellationToken,
) -> Result<AsrTranscriptionOutcome, String> {
    let client = GoogleSttClient::new(config.clone());

    match client.recognize_file(path).await {
        Ok(text) => {
            log::info!("Google STT Recognize (sync) succeeded");
            return Ok(AsrTranscriptionOutcome {
                text,
                model_name: Some("Google / chirp_3".to_string()),
            });
        }
        Err(e) => {
            log::warn!(
                "Google STT Recognize (sync) failed, falling back to streaming: {}",
                e
            );
        }
    }

    run_streaming_asr(path, config, cancel).await
}

async fn run_streaming_asr(
    path: &PathBuf,
    config: &AsrConfig,
    cancel: CancellationToken,
) -> Result<AsrTranscriptionOutcome, String> {
    let pcm_data = crate::asr::ogg_decoder::decode_ogg_opus_to_pcm16k(path)?;
    if pcm_data.is_empty() {
        return Err("音频文件解码后为空".into());
    }

    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(32);

    let feeder_cancel = cancel.clone();
    let pacing_ms: u64 = match config.provider_type {
        AsrProviderType::Google => 0,
        AsrProviderType::FunAsr => 30,
        AsrProviderType::Gemini => unreachable!("Gemini should use file-based transcription"),
        AsrProviderType::GeminiLive => 100,
        AsrProviderType::Soniox => 50,
        AsrProviderType::Cohere => unreachable!("Cohere should use file-based transcription"),
        AsrProviderType::OpenAI if config.openai_asr_mode == "realtime" => 50,
        AsrProviderType::OpenAI => unreachable!("OpenAI should use file-based transcription"),
        AsrProviderType::ElevenLabs => 100,
        AsrProviderType::StepAudio => unreachable!("StepAudio should use file-based transcription"),
        _ => 30,
    };
    tokio::spawn(async move {
        for chunk in pcm_data.chunks(CHUNK_BYTES) {
            if feeder_cancel.is_cancelled() {
                break;
            }
            if tx.send(chunk.to_vec()).await.is_err() {
                break;
            }
            if pacing_ms > 0 {
                tokio::time::sleep(Duration::from_millis(pacing_ms)).await;
            }
        }
    });

    let events: Arc<Mutex<Vec<AsrEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let on_event = move |evt: AsrEvent| {
        let mut lock = events_clone.lock().unwrap();
        lock.push(evt);
    };

    let history = if config.enable_context {
        HistoryService::new().get_recent_history(3)
    } else {
        Vec::new()
    };

    let result = match config.provider_type {
        AsrProviderType::Volcengine => {
            let client = AsrClient::new(config.clone());
            client
                .stream_session(16000, 1, rx, cancel.clone(), history, on_event)
                .await
        }
        AsrProviderType::Google => {
            let client = GoogleSttClient::new(config.clone());
            client
                .stream_session(16000, 1, rx, cancel.clone(), history, on_event)
                .await
        }
        AsrProviderType::FunAsr => {
            let client = FunAsrRealtimeClient::new(config.clone());
            client
                .stream_session(16000, 1, rx, cancel.clone(), history, on_event)
                .await
        }
        AsrProviderType::Qwen => {
            let client = QwenRealtimeClient::new(config.clone());
            client
                .stream_session(16000, 1, rx, cancel.clone(), history, on_event)
                .await
        }
        AsrProviderType::Gemini => {
            unreachable!("Gemini should use file-based transcription")
        }
        AsrProviderType::GeminiLive => {
            let client = GeminiLiveClient::new(config.clone());
            client
                .stream_session(16000, 1, rx, cancel.clone(), history, on_event)
                .await
        }
        AsrProviderType::Cohere => {
            unreachable!("Cohere should use file-based transcription")
        }
        AsrProviderType::OpenAI => {
            if config.openai_asr_mode != "realtime" {
                unreachable!("OpenAI batch should use file-based transcription")
            }
            let client = OpenAIRealtimeClient::new(config.clone());
            client
                .stream_session(16000, 1, rx, cancel.clone(), history, on_event)
                .await
        }
        AsrProviderType::ElevenLabs => {
            if config.elevenlabs_recognition_mode != ElevenLabsRecognitionMode::Realtime {
                unreachable!("ElevenLabs batch should use file-based transcription")
            }
            let client = ElevenLabsRealtimeClient::new(config.clone());
            client
                .stream_session(16000, 1, rx, cancel.clone(), history, on_event)
                .await
        }
        AsrProviderType::Soniox => {
            let client = SonioxClient::new(config.clone());
            client
                .stream_session(16000, 1, rx, cancel.clone(), history, on_event)
                .await
        }
        AsrProviderType::StepAudio => {
            unreachable!("StepAudio should use file-based transcription")
        }
        AsrProviderType::Coli => {
            unreachable!("Coli should use refine_file path")
        }
    };

    if cancel.is_cancelled() {
        return Err("转录已取消".into());
    }

    result.map_err(|e| format!("ASR 流式识别失败: {}", e))?;

    let events = events.lock().unwrap();
    let text = extract_final_text(&events)?;
    Ok(AsrTranscriptionOutcome {
        text,
        model_name: streaming_model_name(config),
    })
}

fn extract_final_text(events: &[AsrEvent]) -> Result<String, String> {
    if let Some(evt) = events.iter().rev().find(|e| e.definite) {
        return Ok(evt.text.clone());
    }
    if let Some(evt) = events.iter().rev().find(|e| e.is_final) {
        return Ok(evt.text.clone());
    }
    if let Some(evt) = events.last() {
        return Ok(evt.text.clone());
    }

    Err("ASR 未返回任何结果".into())
}

fn streaming_model_name(config: &AsrConfig) -> Option<String> {
    match config.provider_type {
        AsrProviderType::Google => Some("Google / chirp_3".to_string()),
        AsrProviderType::FunAsr => {
            HistoryService::format_provider_model("Fun-ASR", &config.funasr_model)
        }
        AsrProviderType::Qwen => HistoryService::format_provider_model("Qwen", &config.qwen_model),
        AsrProviderType::GeminiLive => {
            HistoryService::format_provider_model("Gemini Live", &config.gemini_live_model)
        }
        AsrProviderType::OpenAI if config.openai_asr_mode == "realtime" => {
            HistoryService::format_provider_model("OpenAI Realtime", &config.openai_asr_model)
        }
        AsrProviderType::ElevenLabs => {
            HistoryService::elevenlabs_realtime_model_name(&config.elevenlabs_realtime_model)
        }
        AsrProviderType::Soniox => {
            HistoryService::format_provider_model("Soniox", &config.soniox_model)
        }
        AsrProviderType::Volcengine if config.ws_url.contains("bigmodel_nostream") => {
            Some("Volcengine / bigmodel_nostream".to_string())
        }
        AsrProviderType::Volcengine if config.enable_nonstream => {
            Some("Volcengine / bigmodel_async + native two-pass".to_string())
        }
        AsrProviderType::Volcengine => Some("Volcengine / bigmodel_async".to_string()),
        _ => None,
    }
}
