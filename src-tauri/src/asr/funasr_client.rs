//! DashScope Fun-ASR realtime WebSocket client.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc::Receiver;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
};

use super::audio_utils::{downmix_to_mono, resample_to_8k, resample_to_16k};
use super::config::AsrConfig;
use super::protocol::{AsrError, AsrEvent, AsrPhase};

pub struct FunAsrRealtimeClient {
    config: AsrConfig,
}

impl FunAsrRealtimeClient {
    pub fn new(config: AsrConfig) -> Self {
        Self { config }
    }

    pub async fn stream_session<F>(
        &self,
        sample_rate: u32,
        channels: u16,
        audio_rx: Receiver<Vec<u8>>,
        cancel: tokio_util::sync::CancellationToken,
        _history: Vec<String>,
        on_event: F,
    ) -> Result<(), AsrError>
    where
        F: Fn(AsrEvent) + Send + Sync + 'static,
    {
        if !self.config.is_valid() {
            return Err(AsrError::ConnectionFailed(
                "Invalid Fun-ASR configuration".to_string(),
            ));
        }

        let stream_rate = target_sample_rate(&self.config.funasr_model);
        let ws_url = self.config.funasr_ws_url.trim().to_string();
        let mut req = ws_url
            .into_client_request()
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()).in_phase(AsrPhase::Connect))?;
        {
            let headers = req.headers_mut();
            let auth = format!("Bearer {}", self.config.funasr_api_key.trim());
            let auth_value = HeaderValue::from_str(&auth).map_err(|e| {
                AsrError::ConnectionFailed(format!("Invalid authorization header: {}", e))
                    .in_phase(AsrPhase::Connect)
            })?;
            headers.insert("Authorization", auth_value);
        }

        let (ws_stream, _) = connect_async(req)
            .await
            .map_err(|e| {
                AsrError::ConnectionFailed(format!("Fun-ASR WebSocket connect failed: {}", e))
                    .in_phase(AsrPhase::Connect)
            })?;
        let (mut ws_write, mut ws_read) = ws_stream.split();

        let task_id = uuid::Uuid::new_v4().to_string();
        let start_message = build_run_task_message(&self.config, &task_id, stream_rate)?;
        ws_write
            .send(Message::Text(start_message.into()))
            .await
            .map_err(|e| {
                AsrError::ConnectionFailed(format!("Fun-ASR run-task send failed: {}", e))
                    .in_phase(AsrPhase::Handshake)
            })?;
        wait_for_task_started(&mut ws_read, &task_id).await?;

        if !self.config.hotwords.is_empty() {
            log::info!(
                "Fun-ASR realtime: VoiceX dictionary has {} entries, but inline hotwords are not supported by the current integration; ignoring them",
                self.config.hotwords.len()
            );
        }
        if self.config.enable_context {
            log::info!("Fun-ASR realtime: ASR history context is not supported; ignoring it");
        }

        let on_event = Arc::new(on_event);
        let reader_cancel = cancel.clone();
        let on_event_reader = on_event.clone();
        let reader_handle = tokio::spawn(async move {
            read_events(ws_read, reader_cancel, on_event_reader).await
        });

        write_audio_and_finish(
            &self.config,
            sample_rate,
            channels,
            stream_rate,
            audio_rx,
            cancel.clone(),
            &mut ws_write,
            &task_id,
        )
        .await?;

        reader_handle
            .await
            .map_err(|e| {
                AsrError::ConnectionFailed(format!("Fun-ASR reader task join failed: {}", e))
                    .in_phase(AsrPhase::Finalizing)
            })?
    }
}

async fn wait_for_task_started(
    ws_read: &mut futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    task_id: &str,
) -> Result<(), AsrError> {
    while let Some(message) = ws_read.next().await {
        match message {
            Ok(Message::Text(text)) => {
                let payload: Value = serde_json::from_str(&text).map_err(|e| {
                    AsrError::ProtocolError(format!(
                        "Invalid Fun-ASR handshake JSON for task {}: {}",
                        task_id, e
                    ))
                    .in_phase(AsrPhase::Handshake)
                })?;
                let header = payload.get("header").and_then(Value::as_object).ok_or_else(|| {
                    AsrError::ProtocolError("Fun-ASR handshake missing header".to_string())
                        .in_phase(AsrPhase::Handshake)
                })?;
                match header.get("event").and_then(Value::as_str).unwrap_or_default() {
                    "task-started" => return Ok(()),
                    "task-failed" => {
                        return Err(task_failed_error(header).in_phase(AsrPhase::Handshake));
                    }
                    other => {
                        return Err(AsrError::ProtocolError(format!(
                            "Unexpected Fun-ASR handshake event: {}",
                            other
                        ))
                        .in_phase(AsrPhase::Handshake));
                    }
                }
            }
            Ok(Message::Close(frame)) => {
                return Err(AsrError::ConnectionFailed(format!(
                    "Fun-ASR closed before task start: {:?}",
                    frame
                ))
                .in_phase(AsrPhase::Handshake));
            }
            Ok(other) => {
                log::debug!("Fun-ASR handshake ignoring frame: {:?}", other);
            }
            Err(e) => {
                return Err(AsrError::ConnectionFailed(format!(
                    "Fun-ASR handshake read failed: {}",
                    e
                ))
                .in_phase(AsrPhase::Handshake));
            }
        }
    }

    Err(AsrError::ConnectionFailed(
        "Fun-ASR connection ended before task-started".to_string(),
    )
    .in_phase(AsrPhase::Handshake))
}

async fn read_events(
    mut ws_read: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    cancel: tokio_util::sync::CancellationToken,
    on_event: Arc<dyn Fn(AsrEvent) + Send + Sync>,
) -> Result<(), AsrError> {
    let mut committed_text = String::new();
    let mut pending_partial = String::new();

    while let Some(message) = tokio::select! {
        _ = cancel.cancelled() => None,
        next = ws_read.next() => next,
    } {
        match message {
            Ok(Message::Text(text)) => {
                let payload: Value = serde_json::from_str(&text).map_err(|e| {
                    AsrError::ProtocolError(format!("Invalid Fun-ASR event JSON: {}", e))
                        .in_phase(AsrPhase::Streaming)
                })?;
                let header = payload.get("header").and_then(Value::as_object).ok_or_else(|| {
                    AsrError::ProtocolError("Fun-ASR event missing header".to_string())
                        .in_phase(AsrPhase::Streaming)
                })?;
                let event_name = header.get("event").and_then(Value::as_str).unwrap_or_default();

                match event_name {
                    "result-generated" => {
                        if let Some(sentence) = payload
                            .get("payload")
                            .and_then(|v| v.get("output"))
                            .and_then(|v| v.get("sentence"))
                            .and_then(Value::as_object)
                        {
                            if sentence
                                .get("heartbeat")
                                .and_then(Value::as_bool)
                                .unwrap_or(false)
                            {
                                continue;
                            }
                            let text = sentence
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            if text.is_empty() {
                                continue;
                            }
                            let sentence_end = sentence
                                .get("sentence_end")
                                .and_then(Value::as_bool)
                                .unwrap_or(false);

                            if sentence_end {
                                committed_text.push_str(text);
                                pending_partial.clear();
                                on_event(AsrEvent {
                                    text: committed_text.clone(),
                                    is_final: true,
                                    prefetch: false,
                                    definite: true,
                                    confidence: None,
                                });
                            } else {
                                pending_partial = text.to_string();
                                on_event(AsrEvent {
                                    text: format!("{}{}", committed_text, pending_partial),
                                    is_final: false,
                                    prefetch: false,
                                    definite: false,
                                    confidence: None,
                                });
                            }
                        }
                    }
                    "task-finished" => {
                        if !pending_partial.is_empty() {
                            committed_text.push_str(&pending_partial);
                            pending_partial.clear();
                            on_event(AsrEvent {
                                text: committed_text.clone(),
                                is_final: true,
                                prefetch: false,
                                definite: true,
                                confidence: None,
                            });
                        }
                        return Ok(());
                    }
                    "task-failed" => {
                        return Err(task_failed_error(header).in_phase(AsrPhase::Streaming));
                    }
                    other => {
                        log::debug!("Fun-ASR ignored event: {}", other);
                    }
                }
            }
            Ok(Message::Binary(_)) => {
                return Err(AsrError::ProtocolError(
                    "Fun-ASR returned unexpected binary frame".to_string(),
                )
                .in_phase(AsrPhase::Streaming));
            }
            Ok(Message::Close(frame)) => {
                return Err(AsrError::ConnectionFailed(format!(
                    "Fun-ASR WebSocket closed unexpectedly: {:?}",
                    frame
                ))
                .in_phase(AsrPhase::Streaming));
            }
            Ok(other) => {
                log::debug!("Fun-ASR ignoring non-text frame: {:?}", other);
            }
            Err(e) => {
                return Err(AsrError::ConnectionFailed(format!(
                    "Fun-ASR WebSocket read failed: {}",
                    e
                ))
                .in_phase(AsrPhase::Streaming));
            }
        }
    }

    if cancel.is_cancelled() {
        return Ok(());
    }

    Err(AsrError::ConnectionFailed(
        "Fun-ASR stream ended before task-finished".to_string(),
    )
    .in_phase(AsrPhase::Finalizing))
}

async fn write_audio_and_finish(
    config: &AsrConfig,
    sample_rate: u32,
    channels: u16,
    stream_rate: u32,
    mut audio_rx: Receiver<Vec<u8>>,
    cancel: tokio_util::sync::CancellationToken,
    ws_write: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    task_id: &str,
) -> Result<(), AsrError> {
    while let Some(chunk) = tokio::select! {
        _ = cancel.cancelled() => None,
        next = audio_rx.recv() => next,
    } {
        let pcm = prepare_pcm_chunk(chunk, sample_rate, channels, stream_rate);
        if pcm.is_empty() {
            continue;
        }
        ws_write.send(Message::Binary(pcm.into())).await.map_err(|e| {
            AsrError::ConnectionFailed(format!("Fun-ASR audio send failed: {}", e))
                .in_phase(AsrPhase::Streaming)
        })?;
    }

    if cancel.is_cancelled() {
        return Ok(());
    }

    let finish_message = json!({
        "header": {
            "action": "finish-task",
            "task_id": task_id
        },
        "payload": {
            "input": {}
        }
    });
    ws_write
        .send(Message::Text(finish_message.to_string().into()))
        .await
        .map_err(|e| {
            AsrError::ConnectionFailed(format!("Fun-ASR finish-task send failed: {}", e))
                .in_phase(AsrPhase::Finalizing)
        })?;

    log::debug!(
        "Fun-ASR finish-task sent (model={}, stream_rate={})",
        config.funasr_model,
        stream_rate
    );
    Ok(())
}

fn build_run_task_message(config: &AsrConfig, task_id: &str, sample_rate: u32) -> Result<String, AsrError> {
    let mut parameters = json!({
        "format": "pcm",
        "sample_rate": sample_rate
    });

    if let Some(language_hint) = primary_language_hint(&config.funasr_language) {
        parameters["language_hints"] = json!([language_hint]);
    }

    serde_json::to_string(&json!({
        "header": {
            "action": "run-task",
            "task_id": task_id,
            "streaming": "duplex"
        },
        "payload": {
            "task_group": "audio",
            "task": "asr",
            "function": "recognition",
            "model": config.funasr_model.trim(),
            "parameters": parameters,
            "input": {}
        }
    }))
    .map_err(|e| AsrError::ProtocolError(format!("Failed to serialize Fun-ASR run-task: {}", e)))
}

fn task_failed_error(header: &serde_json::Map<String, Value>) -> AsrError {
    let error_code = header
        .get("error_code")
        .and_then(Value::as_str)
        .unwrap_or("unknown_error");
    let error_message = header
        .get("error_message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    AsrError::ServerError(format!("{}: {}", error_code, error_message))
}

fn primary_language_hint(raw: &str) -> Option<String> {
    raw.split([',', ' ', '\n', '\t'])
        .map(str::trim)
        .find(|part| !part.is_empty())
        .map(str::to_string)
}

fn prepare_pcm_chunk(chunk: Vec<u8>, sample_rate: u32, channels: u16, stream_rate: u32) -> Vec<u8> {
    let mono = downmix_to_mono(&chunk, channels);
    match stream_rate {
        8_000 => resample_to_8k(&mono, sample_rate),
        _ => resample_to_16k(&mono, sample_rate),
    }
}

fn target_sample_rate(model: &str) -> u32 {
    if model.trim().contains("8k") {
        8_000
    } else {
        16_000
    }
}

#[cfg(test)]
mod tests {
    use super::{build_run_task_message, primary_language_hint, target_sample_rate};
    use crate::asr::AsrConfig;

    #[test]
    fn funasr_8k_model_maps_to_8k() {
        assert_eq!(target_sample_rate("fun-asr-flash-8k-realtime"), 8_000);
        assert_eq!(target_sample_rate("fun-asr-realtime"), 16_000);
    }

    #[test]
    fn funasr_language_hint_uses_first_non_empty_value() {
        assert_eq!(primary_language_hint("zh, en").as_deref(), Some("zh"));
        assert_eq!(primary_language_hint("  ").as_deref(), None);
    }

    #[test]
    fn run_task_message_contains_expected_fields() {
        let mut config = AsrConfig::default();
        config.funasr_model = "fun-asr-realtime".to_string();
        config.funasr_language = "zh, en".to_string();
        let msg = build_run_task_message(&config, "task-1", 16_000).unwrap();
        assert!(msg.contains("\"action\":\"run-task\""));
        assert!(msg.contains("\"model\":\"fun-asr-realtime\""));
        assert!(msg.contains("\"sample_rate\":16000"));
        assert!(msg.contains("\"language_hints\":[\"zh\"]"));
    }
}
