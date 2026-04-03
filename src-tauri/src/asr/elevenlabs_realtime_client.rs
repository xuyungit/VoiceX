use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc::Receiver;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
};

use super::audio_utils::{downmix_to_mono, resample_to_16k};
use super::config::AsrConfig;
use super::protocol::{AsrError, AsrEvent, AsrPhase};

const ELEVENLABS_REALTIME_WS_URL: &str = "wss://api.elevenlabs.io/v1/speech-to-text/realtime";
const ELEVENLABS_SESSION_READY_WAIT_MS: u64 = 1_500;
const ELEVENLABS_READER_POLL_MS: u64 = 200;
const ELEVENLABS_FINAL_IDLE_AFTER_COMMIT_MS: u64 = 1_500;
const ELEVENLABS_FINAL_WAIT_MS: u64 = 10_000;
const ELEVENLABS_STREAM_RATE: u32 = 16_000;

pub struct ElevenLabsRealtimeClient {
    config: AsrConfig,
}

impl ElevenLabsRealtimeClient {
    pub fn new(config: AsrConfig) -> Self {
        Self { config }
    }

    pub async fn stream_session<F>(
        &self,
        sample_rate: u32,
        channels: u16,
        audio_rx: Receiver<Vec<u8>>,
        cancel: tokio_util::sync::CancellationToken,
        history: Vec<String>,
        on_event: F,
    ) -> Result<(), AsrError>
    where
        F: Fn(AsrEvent) + Send + Sync + 'static,
    {
        if !self.config.is_valid() {
            return Err(
                AsrError::ConnectionFailed("Invalid ElevenLabs realtime ASR configuration".into())
                    .in_phase(AsrPhase::Connect),
            );
        }

        let ws_url = build_realtime_ws_url(&self.config);
        let diagnostics_enabled = self.config.enable_diagnostics;

        log::info!(
            "ElevenLabs Realtime connecting (capture {} Hz -> stream {} Hz, {} ch, model={}, lang={})",
            sample_rate,
            ELEVENLABS_STREAM_RATE,
            channels,
            self.config.elevenlabs_realtime_model,
            if self.config.elevenlabs_language.trim().is_empty() {
                "auto"
            } else {
                self.config.elevenlabs_language.as_str()
            },
        );

        let mut req = ws_url
            .into_client_request()
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()).in_phase(AsrPhase::Connect))?;
        req.headers_mut().insert(
            "xi-api-key",
            HeaderValue::from_str(&self.config.elevenlabs_api_key).map_err(|e| {
                AsrError::ConnectionFailed(e.to_string()).in_phase(AsrPhase::Connect)
            })?,
        );

        let (ws_stream, _) = connect_async(req).await.map_err(|e| {
            AsrError::ConnectionFailed(format!("ElevenLabs Realtime connect: {e}"))
                .in_phase(AsrPhase::Connect)
        })?;
        let (mut ws_write, mut ws_read) = ws_stream.split();

        loop {
            let msg = tokio::select! {
                _ = cancel.cancelled() => None,
                v = tokio::time::timeout(Duration::from_millis(ELEVENLABS_SESSION_READY_WAIT_MS), ws_read.next()) => {
                    match v {
                        Ok(msg) => msg,
                        Err(_) => {
                            log::warn!(
                                "ElevenLabs session_started not received within {} ms; proceeding with audio stream",
                                ELEVENLABS_SESSION_READY_WAIT_MS
                            );
                            break;
                        }
                    }
                },
            };

            match msg {
                Some(Ok(message)) => {
                    let Some(payload) = parse_json_message(message, "setup")? else {
                        continue;
                    };
                    if diagnostics_enabled {
                        log::info!("ELEVENLABS_DIAG setup inbound {}", payload);
                    }

                    if let Some(reason) = extract_server_error(&payload) {
                        return Err(AsrError::ServerError(reason).in_phase(AsrPhase::Handshake));
                    }

                    if message_type(&payload) == "session_started" {
                        break;
                    }
                }
                Some(Err(e)) => {
                    return Err(AsrError::ConnectionFailed(format!(
                        "ElevenLabs Realtime setup read failed: {e}"
                    ))
                    .in_phase(AsrPhase::Handshake));
                }
                None => {
                    return Err(AsrError::ConnectionFailed(
                        "ElevenLabs Realtime closed before session_started".to_string(),
                    )
                    .in_phase(AsrPhase::Handshake));
                }
            }
        }

        let on_event = Arc::new(on_event);
        let cancel_reader = cancel.clone();
        let reader_cancel = cancel.child_token();
        let writer_cancel = reader_cancel.clone();
        let stream_end_started_at = Arc::new(Mutex::new(None::<Instant>));
        let stream_end_started_at_reader = stream_end_started_at.clone();
        let on_event_reader = on_event.clone();

        let reader_handle = tokio::spawn(async move {
            let mut transcript = ElevenLabsTranscriptAccumulator::default();
            let mut last_relevant_event_at = Instant::now();

            let result: Result<(), AsrError> = async {
                loop {
                    let msg = tokio::select! {
                        _ = cancel_reader.cancelled() => None,
                        v = tokio::time::timeout(Duration::from_millis(ELEVENLABS_READER_POLL_MS), ws_read.next()) => {
                            match v {
                                Ok(msg) => msg,
                                Err(_) => {
                                    let stream_end_started_at = stream_end_started_at_reader
                                        .lock()
                                        .ok()
                                        .and_then(|guard| *guard);
                                    if let Some(stream_end_started_at) = stream_end_started_at {
                                        if last_relevant_event_at > stream_end_started_at
                                            && last_relevant_event_at.elapsed()
                                                >= Duration::from_millis(
                                                    ELEVENLABS_FINAL_IDLE_AFTER_COMMIT_MS,
                                                )
                                        {
                                            break;
                                        }
                                        if stream_end_started_at.elapsed()
                                            >= Duration::from_millis(ELEVENLABS_FINAL_WAIT_MS)
                                        {
                                            return Err(AsrError::ConnectionFailed(
                                                "ElevenLabs Realtime timed out while waiting for final committed transcript"
                                                    .to_string(),
                                            )
                                            .in_phase(AsrPhase::Finalizing));
                                        }
                                    }
                                    continue;
                                }
                            }
                        }
                    };

                    let Some(msg) = msg else {
                        break;
                    };

                    match msg {
                        Ok(message) => {
                            let Some(payload) = parse_json_message(message, "runtime")? else {
                                continue;
                            };
                            if diagnostics_enabled {
                                log::info!("ELEVENLABS_DIAG runtime inbound {}", payload);
                            }

                            if let Some(reason) = extract_server_error(&payload) {
                                return Err(
                                    AsrError::ServerError(reason).in_phase(AsrPhase::Streaming)
                                );
                            }

                            match message_type(&payload) {
                                "partial_transcript" => {
                                    if let Some(text) = payload.get("text").and_then(Value::as_str) {
                                        if let Some(combined) = transcript.push_partial(text) {
                                            on_event_reader(AsrEvent {
                                                text: combined,
                                                is_final: false,
                                                prefetch: false,
                                                definite: false,
                                                confidence: None,
                                            });
                                            last_relevant_event_at = Instant::now();
                                        }
                                    }
                                }
                                "committed_transcript" | "committed_transcript_with_timestamps" => {
                                    if let Some(text) = payload.get("text").and_then(Value::as_str) {
                                        if let Some(combined) = transcript.commit_segment(text) {
                                            on_event_reader(AsrEvent {
                                                text: combined,
                                                is_final: true,
                                                prefetch: false,
                                                definite: true,
                                                confidence: None,
                                            });
                                            last_relevant_event_at = Instant::now();
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        Err(e) => {
                            return Err(AsrError::ConnectionFailed(format!(
                                "ElevenLabs Realtime read failed: {e}"
                            ))
                            .in_phase(AsrPhase::Streaming));
                        }
                    }
                }

                Ok(())
            }
            .await;

            reader_cancel.cancel();
            result
        });

        let resample_needed = sample_rate != ELEVENLABS_STREAM_RATE;
        if resample_needed {
            log::debug!(
                "ElevenLabs Realtime resampling input {} Hz -> {} Hz",
                sample_rate,
                ELEVENLABS_STREAM_RATE
            );
        }

        let mut audio_rx = audio_rx;
        let mut pending_chunk: Option<Vec<u8>> = None;
        let mut audio_chunk_count: u64 = 0;
        let mut audio_byte_count: u64 = 0;
        let mut previous_text = build_previous_text_context(&history);

        while let Some(chunk) = tokio::select! {
            _ = cancel.cancelled() => None,
            _ = writer_cancel.cancelled() => None,
            v = audio_rx.recv() => v,
        } {
            let pcm = if resample_needed {
                resample_to_16k(&chunk, sample_rate)
            } else {
                chunk
            };
            if pcm.is_empty() {
                continue;
            }

            let mono = if channels > 1 {
                downmix_to_mono(&pcm, channels)
            } else {
                pcm
            };
            if mono.is_empty() {
                continue;
            }

            if let Some(previous_chunk) = pending_chunk.replace(mono) {
                send_audio_chunk(
                    &mut ws_write,
                    &previous_chunk,
                    false,
                    previous_text.take(),
                )
                .await?;
                audio_chunk_count = audio_chunk_count.saturating_add(1);
                audio_byte_count = audio_byte_count.saturating_add(previous_chunk.len() as u64);
            }
        }

        if cancel.is_cancelled() {
            let _ = ws_write.close().await;
            let _ = tokio::time::timeout(Duration::from_millis(2_000), reader_handle).await;
            return Ok(());
        }

        if writer_cancel.is_cancelled() {
            let _ = ws_write.close().await;
            return match tokio::time::timeout(Duration::from_millis(2_000), reader_handle).await {
                Ok(Ok(result)) => result,
                Ok(Err(e)) => Err(AsrError::ConnectionFailed(format!(
                    "ElevenLabs Realtime reader task failed: {e}"
                ))
                .in_phase(AsrPhase::Streaming)),
                Err(_) => Err(AsrError::ConnectionFailed(
                    "ElevenLabs Realtime reader did not finish after stream cancellation"
                        .to_string(),
                )
                .in_phase(AsrPhase::Streaming)),
            };
        }

        if let Some(last_chunk) = pending_chunk.take() {
            send_audio_chunk(&mut ws_write, &last_chunk, true, previous_text.take()).await?;
            audio_chunk_count = audio_chunk_count.saturating_add(1);
            audio_byte_count = audio_byte_count.saturating_add(last_chunk.len() as u64);
            if let Ok(mut guard) = stream_end_started_at.lock() {
                *guard = Some(Instant::now());
            }
        }

        log::info!(
            "ElevenLabs Realtime audio stream finished (chunks={}, bytes={})",
            audio_chunk_count,
            audio_byte_count
        );

        let read_result = tokio::time::timeout(
            Duration::from_millis(ELEVENLABS_FINAL_WAIT_MS + 2_000),
            reader_handle,
        )
        .await;

        let _ = ws_write.close().await;

        match read_result {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => Err(AsrError::ConnectionFailed(format!(
                "ElevenLabs Realtime reader task failed: {e}"
            ))
            .in_phase(AsrPhase::Streaming)),
            Err(_) => Err(AsrError::ConnectionFailed(
                "ElevenLabs Realtime reader did not finish before timeout".to_string(),
            )
            .in_phase(AsrPhase::Finalizing)),
        }
    }
}

async fn send_audio_chunk(
    ws_write: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    audio: &[u8],
    commit: bool,
    previous_text: Option<String>,
) -> Result<(), AsrError> {
    let mut payload = json!({
        "message_type": "input_audio_chunk",
        "audio_base_64": STANDARD.encode(audio),
        "sample_rate": ELEVENLABS_STREAM_RATE,
    });

    if commit {
        payload["commit"] = Value::Bool(true);
    }

    if let Some(previous_text) = previous_text.filter(|text| !text.trim().is_empty()) {
        payload["previous_text"] = Value::String(previous_text);
    }

    ws_write
        .send(Message::Text(payload.to_string()))
        .await
        .map_err(|e| {
            AsrError::ConnectionFailed(format!("ElevenLabs Realtime audio send failed: {e}"))
                .in_phase(AsrPhase::Streaming)
        })
}

fn build_realtime_ws_url(config: &AsrConfig) -> String {
    let mut params = vec![
        format!(
            "model_id={}",
            urlencoding::encode(config.elevenlabs_realtime_model.trim())
        ),
        "include_timestamps=false".to_string(),
        "include_language_detection=false".to_string(),
        "audio_format=pcm_16000".to_string(),
        "commit_strategy=vad".to_string(),
        "vad_silence_threshold_secs=1.5".to_string(),
        "vad_threshold=0.4".to_string(),
        "min_speech_duration_ms=100".to_string(),
        "min_silence_duration_ms=100".to_string(),
    ];

    if !config.elevenlabs_language.trim().is_empty() {
        params.push(format!(
            "language_code={}",
            urlencoding::encode(config.elevenlabs_language.trim())
        ));
    }

    format!("{}?{}", ELEVENLABS_REALTIME_WS_URL, params.join("&"))
}

fn parse_json_message(message: Message, stage: &str) -> Result<Option<Value>, AsrError> {
    match message {
        Message::Text(text) => serde_json::from_str(&text).map(Some).map_err(|e| {
            AsrError::ProtocolError(format!(
                "Invalid ElevenLabs Realtime {stage} JSON text event: {e}"
            ))
        }),
        Message::Binary(bin) => {
            let text = String::from_utf8(bin).map_err(|e| {
                AsrError::ProtocolError(format!(
                    "Invalid ElevenLabs Realtime {stage} UTF-8 binary event: {e}"
                ))
            })?;
            serde_json::from_str(&text).map(Some).map_err(|e| {
                AsrError::ProtocolError(format!(
                    "Invalid ElevenLabs Realtime {stage} JSON binary event: {e}"
                ))
            })
        }
        Message::Ping(_) | Message::Pong(_) => Ok(None),
        Message::Close(_) => Ok(None),
        other => Err(AsrError::ProtocolError(format!(
            "Unsupported ElevenLabs Realtime {stage} message: {:?}",
            other
        ))),
    }
}

fn message_type(payload: &Value) -> &str {
    payload
        .get("message_type")
        .and_then(Value::as_str)
        .or_else(|| payload.get("type").and_then(Value::as_str))
        .unwrap_or("")
}

fn extract_server_error(payload: &Value) -> Option<String> {
    let message_type = message_type(payload);
    let is_error = matches!(
        message_type,
        "auth_error"
            | "quota_exceeded"
            | "rate_limited"
            | "queue_overflow"
            | "resource_exhausted"
            | "session_time_limit_exceeded"
            | "chunk_size_exceeded"
            | "insufficient_audio_activity"
            | "commit_throttled"
            | "input_error"
            | "transcriber_error"
            | "error"
    );

    if !is_error {
        return None;
    }

    let reason = payload
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| payload.get("error").and_then(Value::as_str))
        .or_else(|| payload.get("detail").and_then(Value::as_str))
        .unwrap_or("Unknown realtime error");

    Some(format!("ElevenLabs Realtime {}: {}", message_type, reason))
}

fn build_previous_text_context(history: &[String]) -> Option<String> {
    history
        .iter()
        .rev()
        .map(|entry| entry.trim())
        .find(|entry| !entry.is_empty())
        .map(|entry| entry.chars().take(50).collect::<String>())
        .filter(|entry| !entry.is_empty())
}

#[derive(Debug, Default)]
struct ElevenLabsTranscriptAccumulator {
    committed_segments: Vec<String>,
    current_partial: String,
    last_emitted_partial: String,
    last_emitted_final: String,
}

impl ElevenLabsTranscriptAccumulator {
    fn push_partial(&mut self, partial: &str) -> Option<String> {
        let normalized = partial.trim();
        if normalized.is_empty() {
            return None;
        }
        self.current_partial = normalized.to_string();
        let combined = self.combine(Some(normalized));
        if combined.is_empty()
            || combined == self.last_emitted_partial
            || combined == self.last_emitted_final
        {
            return None;
        }
        self.last_emitted_partial = combined.clone();
        Some(combined)
    }

    fn commit_segment(&mut self, segment: &str) -> Option<String> {
        let normalized = segment.trim();
        if normalized.is_empty() {
            return None;
        }

        self.current_partial.clear();
        self.committed_segments.push(normalized.to_string());
        let combined = self.combine(None);
        if combined.is_empty() || combined == self.last_emitted_final {
            return None;
        }
        self.last_emitted_final = combined.clone();
        self.last_emitted_partial.clear();
        Some(combined)
    }

    fn combine(&self, partial: Option<&str>) -> String {
        let mut parts = self.committed_segments.clone();
        if let Some(partial) = partial {
            if !partial.trim().is_empty() {
                parts.push(partial.trim().to_string());
            }
        }
        parts.join(" ").trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_previous_text_context, build_realtime_ws_url, extract_server_error,
        parse_json_message,
    };
    use crate::asr::{AsrConfig, ElevenLabsRecognitionMode, ElevenLabsPostRecordingRefine};
    use serde_json::json;
    use tokio_tungstenite::tungstenite::Message;

    #[test]
    fn realtime_url_contains_expected_defaults() {
        let mut config = AsrConfig::default();
        config.elevenlabs_realtime_model = "scribe_v2_realtime".to_string();
        config.elevenlabs_language = "en".to_string();
        config.elevenlabs_recognition_mode = ElevenLabsRecognitionMode::Realtime;
        config.elevenlabs_post_recording_refine = ElevenLabsPostRecordingRefine::Off;

        let url = build_realtime_ws_url(&config);

        assert!(url.contains("model_id=scribe_v2_realtime"));
        assert!(url.contains("audio_format=pcm_16000"));
        assert!(url.contains("commit_strategy=vad"));
        assert!(url.contains("language_code=en"));
    }

    #[test]
    fn previous_text_context_uses_latest_non_empty_entry() {
        let context = build_previous_text_context(&[
            "".to_string(),
            "older".to_string(),
            " latest context ".to_string(),
        ]);

        assert_eq!(context.as_deref(), Some("latest context"));
    }

    #[test]
    fn realtime_error_extraction_includes_message_type() {
        let payload = json!({
            "message_type": "queue_overflow",
            "message": "Too many buffered chunks"
        });

        let reason = extract_server_error(&payload);

        assert_eq!(
            reason.as_deref(),
            Some("ElevenLabs Realtime queue_overflow: Too many buffered chunks")
        );
    }

    #[test]
    fn parse_json_message_ignores_control_frames() {
        assert!(parse_json_message(Message::Ping(vec![1, 2, 3].into()), "runtime")
            .unwrap()
            .is_none());
        assert!(parse_json_message(Message::Pong(vec![1, 2, 3].into()), "runtime")
            .unwrap()
            .is_none());
        assert!(parse_json_message(Message::Close(None), "runtime")
            .unwrap()
            .is_none());
    }
}
