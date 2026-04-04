//! Re-transcribe command — re-runs ASR (and optionally LLM) on an existing audio file.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::asr::{
    AsrClient, AsrConfig, AsrEvent, AsrProviderType, CohereTranscriptionClient, ColiAsrClient,
    ColiRefinementMode, ElevenLabsRealtimeClient, ElevenLabsRecognitionMode,
    ElevenLabsTranscriptionClient, GeminiLiveClient, GeminiTranscriptionClient, GoogleSttClient,
    OpenAIRealtimeClient, OpenAITranscriptionClient, QwenRealtimeClient, QwenRecognitionMode,
    QwenTranscriptionClient,
};
use crate::llm::{LLMClient, LLMConfig, LLMProviderType, PromptBuildOptions};
use crate::services::history_service::HistoryService;
use crate::storage;

static RETRANSCRIBE_CANCEL: std::sync::OnceLock<Mutex<Option<CancellationToken>>> =
    std::sync::OnceLock::new();

fn cancel_store() -> &'static Mutex<Option<CancellationToken>> {
    RETRANSCRIBE_CANCEL.get_or_init(|| Mutex::new(None))
}

const OVERALL_TIMEOUT_SECS: u64 = 300;
const LLM_TIMEOUT_SECS: u64 = 8;
/// 100ms of 16kHz mono i16 = 16000 * 2 * 0.1 = 3200 bytes
const CHUNK_BYTES: usize = 3200;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReTranscribeRequest {
    pub audio_path: String,
    pub asr_provider: String,
    pub enable_llm: bool,
    pub llm_provider: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReTranscribeResult {
    pub asr_text: String,
    pub llm_text: Option<String>,
    pub asr_model_name: String,
    pub llm_model_name: Option<String>,
}

#[derive(Debug)]
struct AsrTranscriptionOutcome {
    text: String,
    model_name: Option<String>,
}

#[tauri::command]
pub async fn re_transcribe(request: ReTranscribeRequest) -> Result<ReTranscribeResult, String> {
    let path = PathBuf::from(&request.audio_path);
    if !path.is_file() {
        return Err(format!("录音文件不存在: {}", path.display()));
    }

    // Load settings and override providers
    let mut settings = storage::get_settings().map_err(|e| format!("加载设置失败: {}", e))?;
    settings.asr_provider_type = request.asr_provider.clone();
    if let Some(ref llm_provider) = request.llm_provider {
        settings.llm_provider_type = llm_provider.clone();
    }

    let mut config = AsrConfig::from(&settings);
    if !config.is_valid() {
        return Err("所选 ASR 服务配置不完整，请先在设置页面填写相关凭证".into());
    }

    // Set up cancellation
    let cancel = CancellationToken::new();
    {
        let mut store = cancel_store().lock().unwrap();
        *store = Some(cancel.clone());
    }

    let result = tokio::time::timeout(
        Duration::from_secs(OVERALL_TIMEOUT_SECS),
        run_retranscribe(&path, &mut config, &settings, &request, cancel.clone()),
    )
    .await;

    // Clear cancellation token
    {
        let mut store = cancel_store().lock().unwrap();
        *store = None;
    }

    match result {
        Ok(inner) => inner,
        Err(_) => Err("转录超时，请稍后重试".into()),
    }
}

async fn run_retranscribe(
    path: &PathBuf,
    config: &mut AsrConfig,
    settings: &crate::commands::settings::AppSettings,
    request: &ReTranscribeRequest,
    cancel: CancellationToken,
) -> Result<ReTranscribeResult, String> {
    // --- ASR ---
    let outcome = transcribe_with_config_detailed(path, config, cancel.clone()).await?;
    let asr_text = outcome.text;

    if asr_text.trim().is_empty() {
        return Err("ASR 未能识别出任何文本，请检查录音质量或尝试其他模型".into());
    }

    // --- LLM ---
    let (llm_text, llm_model_name) = if request.enable_llm {
        run_llm_correction(&asr_text, settings).await
    } else {
        (None, None)
    };

    // --- Model names ---
    let asr_model_name = outcome.model_name.unwrap_or_else(|| {
        HistoryService::resolve_asr_model_name(settings).unwrap_or_else(|| "Unknown".to_string())
    });

    Ok(ReTranscribeResult {
        asr_text,
        llm_text,
        asr_model_name,
        llm_model_name,
    })
}

pub async fn transcribe_with_config(
    path: &PathBuf,
    config: &mut AsrConfig,
    cancel: CancellationToken,
) -> Result<String, String> {
    transcribe_with_config_detailed(path, config, cancel)
        .await
        .map(|outcome| outcome.text)
}

async fn transcribe_with_config_detailed(
    path: &PathBuf,
    config: &mut AsrConfig,
    cancel: CancellationToken,
) -> Result<AsrTranscriptionOutcome, String> {
    match config.provider_type {
        AsrProviderType::Coli => run_coli_asr(path, config).await,
        AsrProviderType::Qwen => run_qwen_asr(path, config).await,
        AsrProviderType::Gemini => run_gemini_asr(path, config).await,
        AsrProviderType::GeminiLive => run_streaming_asr(path, config, cancel).await,
        AsrProviderType::Cohere => run_cohere_asr(path, config).await,
        AsrProviderType::OpenAI => run_openai_asr(path, config).await,
        AsrProviderType::ElevenLabs => run_elevenlabs_asr(path, config).await,
        AsrProviderType::Google => run_google_asr(path, config, cancel).await,
        _ => run_streaming_asr(path, config, cancel).await,
    }
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
                    "Qwen batch refine returned empty result during re-transcribe; falling back to realtime transcript"
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
                "Qwen batch refine failed during re-transcribe; falling back to realtime transcript: {}",
                err
            );
            Ok(AsrTranscriptionOutcome {
                text: trimmed_streaming,
                model_name: HistoryService::format_provider_model("Qwen", &config.qwen_model),
            })
        }
    }
}

/// Run ASR via Coli's file-based recognition.
async fn run_coli_asr(
    path: &PathBuf,
    config: &mut AsrConfig,
) -> Result<AsrTranscriptionOutcome, String> {
    // Force a refinement mode if currently Off — the user explicitly wants transcription
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
        return run_streaming_asr(path, config, CancellationToken::new()).await;
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
                            "ElevenLabs batch refine returned empty result during re-transcribe; falling back to realtime transcript"
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
                        "ElevenLabs batch refine failed during re-transcribe; falling back to realtime transcript: {}",
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

/// Run ASR via Google: try sync Recognize first (fast, ≤60s), fall back to streaming.
async fn run_google_asr(
    path: &PathBuf,
    config: &AsrConfig,
    cancel: CancellationToken,
) -> Result<AsrTranscriptionOutcome, String> {
    let client = GoogleSttClient::new(config.clone());

    // Try sync Recognize first — much faster for short audio
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

    // Fallback to streaming (handles >60s audio and other edge cases)
    run_streaming_asr(path, config, cancel).await
}

/// Run ASR via streaming interface (Volcengine/Google/Qwen) by feeding decoded PCM.
async fn run_streaming_asr(
    path: &PathBuf,
    config: &AsrConfig,
    cancel: CancellationToken,
) -> Result<AsrTranscriptionOutcome, String> {
    // Decode OGG/Opus to PCM 16kHz mono
    let pcm_data = crate::asr::ogg_decoder::decode_ogg_opus_to_pcm16k(path)?;

    if pcm_data.is_empty() {
        return Err("音频文件解码后为空".into());
    }

    // Create channel to feed PCM chunks
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(32);

    // Spawn chunk feeder with provider-specific pacing.
    // Volcengine client has a 5s reader timeout after the last packet; if bulk audio
    // is sent instantly the server cannot finish processing within that window.
    // Qwen realtime API also requires near-real-time delivery.
    // Google gRPC has no post-stream timeout (reader waits for server close).
    let feeder_cancel = cancel.clone();
    let pacing_ms: u64 = match config.provider_type {
        AsrProviderType::Google => 0,
        AsrProviderType::Gemini => unreachable!("Gemini should use file-based transcription"),
        // Gemini Live and Soniox are more reliable for offline replays when
        // audio is fed at near-realtime speed, otherwise the stream may end
        // before the server finishes processing the tail audio.
        AsrProviderType::GeminiLive => 100,
        // Soniox needs slower pacing than 30ms but can handle ~2x real-time
        AsrProviderType::Soniox => 50,
        AsrProviderType::Cohere => unreachable!("Cohere should use file-based transcription"),
        AsrProviderType::OpenAI if config.openai_asr_mode == "realtime" => 50,
        AsrProviderType::OpenAI => unreachable!("OpenAI should use file-based transcription"),
        AsrProviderType::ElevenLabs => 100,
        _ => 30, // ~3x real-time (each chunk = 100ms of audio)
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
        // tx drops here, signaling end of stream
    });

    // Collect ASR events
    let events: Arc<Mutex<Vec<AsrEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let on_event = move |evt: AsrEvent| {
        let mut lock = events_clone.lock().unwrap();
        lock.push(evt);
    };

    // Load history context
    let history = if config.enable_context {
        HistoryService::new().get_recent_history(3)
    } else {
        Vec::new()
    };

    // Dispatch to provider — same flow as asr_manager.rs
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
            let client = crate::asr::SonioxClient::new(config.clone());
            client
                .stream_session(16000, 1, rx, cancel.clone(), history, on_event)
                .await
        }
        AsrProviderType::Coli => {
            unreachable!("Coli should use refine_file path")
        }
    };

    if cancel.is_cancelled() {
        return Err("转录已取消".into());
    }

    result.map_err(|e| format!("ASR 流式识别失败: {}", e))?;

    // Extract final text from collected events
    let events = events.lock().unwrap();
    let text = extract_final_text(&events)?;
    Ok(AsrTranscriptionOutcome {
        text,
        model_name: streaming_model_name(config),
    })
}

/// Extract the best final text from collected ASR events.
fn extract_final_text(events: &[AsrEvent]) -> Result<String, String> {
    // Prefer the last definite (Volcengine 2nd-pass) result
    if let Some(evt) = events.iter().rev().find(|e| e.definite) {
        return Ok(evt.text.clone());
    }
    // Then the last is_final result
    if let Some(evt) = events.iter().rev().find(|e| e.is_final) {
        return Ok(evt.text.clone());
    }
    // Fallback to the last event's text
    if let Some(evt) = events.last() {
        return Ok(evt.text.clone());
    }

    Err("ASR 未返回任何结果".into())
}

fn streaming_model_name(config: &AsrConfig) -> Option<String> {
    match config.provider_type {
        AsrProviderType::Google => Some("Google / chirp_3".to_string()),
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

/// Run LLM correction on ASR text.
async fn run_llm_correction(
    asr_text: &str,
    settings: &crate::commands::settings::AppSettings,
) -> (Option<String>, Option<String>) {
    let provider_type = LLMProviderType::from_str(&settings.llm_provider_type);

    let (base_url, api_key, model_name, volcengine_reasoning_effort) = match provider_type {
        LLMProviderType::Volcengine => (
            settings.llm_volcengine_base_url.clone(),
            settings.llm_volcengine_api_key.clone(),
            settings.llm_volcengine_model.clone(),
            settings.llm_volcengine_reasoning_effort.clone(),
        ),
        LLMProviderType::Openai => (
            settings.llm_openai_base_url.clone(),
            settings.llm_openai_api_key.clone(),
            settings.llm_openai_model.clone(),
            None,
        ),
        LLMProviderType::Qwen => (
            settings.llm_qwen_base_url.clone(),
            settings.llm_qwen_api_key.clone(),
            settings.llm_qwen_model.clone(),
            None,
        ),
        LLMProviderType::Custom => (
            settings.llm_custom_base_url.clone(),
            settings.llm_custom_api_key.clone(),
            settings.llm_custom_model.clone(),
            None,
        ),
    };

    let client = LLMClient::new(LLMConfig {
        provider_type: provider_type.clone(),
        base_url,
        api_key,
        model_name,
        volcengine_reasoning_effort,
    });

    let history = if settings.enable_llm_history_context {
        Some(HistoryService::new().get_recent_history(5))
    } else {
        None
    };

    let llm_model_name = HistoryService::resolve_llm_model_name(settings);

    match tokio::time::timeout(
        Duration::from_secs(LLM_TIMEOUT_SECS),
        client.correct(
            asr_text.trim(),
            &settings.llm_prompt_template,
            &settings.dictionary_text,
            history.as_deref(),
            PromptBuildOptions::default(),
        ),
    )
    .await
    {
        Ok(Ok(corrected)) => (Some(corrected), llm_model_name),
        Ok(Err(err)) => {
            log::warn!("Re-transcribe LLM correction failed: {}", err);
            (None, llm_model_name)
        }
        Err(_) => {
            log::warn!("Re-transcribe LLM correction timed out");
            (None, llm_model_name)
        }
    }
}

#[tauri::command]
pub fn cancel_retranscribe() {
    let store = cancel_store().lock().unwrap();
    if let Some(token) = store.as_ref() {
        token.cancel();
        log::info!("Re-transcribe cancelled by user");
    }
}
