//! Soniox real-time ASR WebSocket client.
//!
//! Protocol: <https://soniox.com/docs/stt/rt/real-time-transcription>
//! - Connect to `wss://stt-rt.soniox.com/transcribe-websocket`
//! - Send JSON config as first text message
//! - Stream raw PCM audio as binary frames
//! - Receive JSON responses with `tokens` array (each token has `text` and `is_final`)
//! - Send empty string `""` to signal end-of-audio
//! - Session ends when response contains `"finished": true`

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc::Receiver;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::audio_utils::resample_to_16k;
use super::config::AsrConfig;
use super::protocol::{AsrError, AsrEvent, AsrPhase};

const SONIOX_DEFAULT_WS_URL: &str = "wss://stt-rt.soniox.com/transcribe-websocket";

pub struct SonioxClient {
    config: AsrConfig,
}

impl SonioxClient {
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
            return Err(
                AsrError::ConnectionFailed("Invalid Soniox ASR configuration".to_string())
                    .in_phase(AsrPhase::Connect),
            );
        }

        let stream_rate: u32 = 16_000;
        let diagnostics_enabled = self.config.enable_diagnostics;
        let ws_url = resolve_soniox_ws_url();
        let injected_fault = SonioxFaultMode::from_env();

        if let Some(fault) = injected_fault {
            log::warn!("SONIOX_FAULT active: {}", fault.as_str());
        }

        log::info!(
            "Soniox ASR connecting (capture {} Hz -> stream {} Hz, {} ch, model={}, lang={:?}, ws={})",
            sample_rate,
            stream_rate,
            channels,
            self.config.soniox_model,
            self.config.soniox_language,
            ws_url,
        );

        if injected_fault == Some(SonioxFaultMode::ConnectFail) {
            return Err(AsrError::ConnectionFailed(
                "Injected Soniox fault: connect failure".to_string(),
            )
            .in_phase(AsrPhase::Connect));
        }

        let (ws_stream, _) = connect_async(&ws_url).await.map_err(|e| {
            AsrError::ConnectionFailed(format!("Soniox WebSocket connect: {e}"))
                .in_phase(AsrPhase::Connect)
        })?;
        let (mut ws_write, mut ws_read) = ws_stream.split();

        if injected_fault == Some(SonioxFaultMode::HandshakeFail) {
            return Err(AsrError::ConnectionFailed(
                "Injected Soniox fault: handshake failure".to_string(),
            )
            .in_phase(AsrPhase::Handshake));
        }

        // Build initial configuration message
        // Ref: https://soniox.com/docs/stt/api-reference/websocket-api
        let mut config_msg = json!({
            "api_key": self.config.soniox_api_key,
            "model": self.config.soniox_model,
            "enable_endpoint_detection": true,
            "audio_format": "s16le",
            "sample_rate": stream_rate,
            "num_channels": 1,
        });

        // Language hints — comma-separated string → JSON array
        if !self.config.soniox_language.is_empty() {
            let hints: Vec<String> = self
                .config
                .soniox_language
                .split(',')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            if !hints.is_empty() {
                config_msg["language_hints"] = json!(hints);
            }
        }

        // Hotwords as custom terms via context.terms
        // Ref: context.terms is array<string>, max 10,000 chars total
        if !self.config.hotwords.is_empty() {
            config_msg["context"] = json!({
                "terms": self.config.hotwords,
            });
            log::info!(
                "Soniox ASR: sending {} hotwords as context.terms",
                self.config.hotwords.len()
            );
        }

        if diagnostics_enabled {
            // Redact api_key for logging
            let mut log_msg = config_msg.clone();
            log_msg["api_key"] = json!("***");
            log::info!("SONIOX_DIAG config {}", log_msg);
        }

        ws_write
            .send(Message::Text(config_msg.to_string()))
            .await
            .map_err(|e| {
                AsrError::ConnectionFailed(format!("Failed to send Soniox config: {e}"))
                    .in_phase(AsrPhase::Handshake)
            })?;

        if let Some(fault) = injected_fault {
            if let Some(status) = fault.injected_server_status() {
                return Err(AsrError::ServerError(format!(
                    "Injected Soniox fault {}: simulated provider response",
                    status
                ))
                .in_phase(AsrPhase::Handshake));
            }
        }

        // Use a CancellationToken so the reader can tell the writer to stop
        // when the server closes the connection or sends an error.
        let reader_cancel = cancel.child_token();
        let writer_cancel = reader_cancel.clone();

        // Spawn reader task
        let on_event = Arc::new(on_event);
        let on_event_reader = on_event.clone();
        let cancel_reader = cancel.clone();

        let reader_handle = tokio::spawn(async move {
            // Soniox token accumulation:
            // - Final tokens are sent ONCE and never repeated in subsequent responses.
            // - Non-final tokens reset each response (only current provisional state).
            // - We accumulate final tokens across responses into `final_text`.
            // - Each response's non-final tokens are the current interim tail.
            // - Display = final_text + current non-final tokens.
            //
            // We also keep `last_non_final` across responses because the
            // `finished: true` response may arrive with `tokens: []`, and
            // we need to preserve the trailing non-final text from the
            // previous response so it isn't lost.
            let mut final_text = String::new();
            let mut last_non_final = String::new();

            let result: Result<(), AsrError> = async {
                while let Some(msg) = tokio::select! {
                    _ = cancel_reader.cancelled() => None,
                    v = ws_read.next() => v,
                } {
                    match msg {
                        Ok(Message::Text(text)) => {
                            if diagnostics_enabled {
                                log::info!("SONIOX_DIAG inbound {}", text);
                            }

                            let payload: Value = serde_json::from_str(&text).map_err(|e| {
                                AsrError::ProtocolError(format!("Invalid Soniox JSON: {e}"))
                                    .in_phase(AsrPhase::Streaming)
                            })?;

                            // Check for error (error_code is a number per API spec)
                            if let Some(error_code) = payload.get("error_code") {
                                let code = error_code
                                    .as_u64()
                                    .map(|n| n.to_string())
                                    .or_else(|| error_code.as_str().map(String::from))
                                    .unwrap_or_else(|| "unknown".to_string());
                                let error_msg = payload
                                    .get("error_message")
                                    .and_then(Value::as_str)
                                    .unwrap_or("Unknown error");
                                return Err(AsrError::ServerError(format!(
                                    "Soniox error {}: {}",
                                    code, error_msg
                                ))
                                .in_phase(AsrPhase::Streaming));
                            }

                            let finished = payload
                                .get("finished")
                                .and_then(Value::as_bool)
                                .unwrap_or(false);

                            // Process tokens in this response.
                            // Final tokens: new, append to accumulated final_text.
                            // Non-final tokens: current provisional tail, replaced each response.
                            if let Some(tokens) = payload.get("tokens").and_then(Value::as_array) {
                                let mut non_final_part = String::new();

                                for token in tokens {
                                    let token_text =
                                        token.get("text").and_then(Value::as_str).unwrap_or("");

                                    // Skip special control tokens (e.g. <end>)
                                    if token_text.starts_with('<') && token_text.ends_with('>') {
                                        continue;
                                    }

                                    let is_final = token
                                        .get("is_final")
                                        .and_then(Value::as_bool)
                                        .unwrap_or(false);

                                    if is_final {
                                        // Final tokens are new — append to accumulator
                                        final_text.push_str(token_text);
                                    } else {
                                        non_final_part.push_str(token_text);
                                    }
                                }

                                // Update last_non_final only when this response has
                                // tokens; the finished response (tokens=[]) should
                                // NOT clear it.
                                if !tokens.is_empty() {
                                    last_non_final = non_final_part.clone();
                                }

                                // Display = all accumulated finals + current non-finals
                                let display_non_final = if tokens.is_empty() {
                                    &last_non_final
                                } else {
                                    &non_final_part
                                };
                                let combined = format!("{}{}", final_text, display_non_final);
                                let trimmed = combined.trim();

                                if finished {
                                    // Session done — emit one definitive final event
                                    log::info!("Soniox ASR session finished");
                                    if !trimmed.is_empty() {
                                        on_event_reader(AsrEvent {
                                            text: trimmed.to_string(),
                                            is_final: true,
                                            prefetch: false,
                                            definite: true,
                                            confidence: None,
                                        });
                                    }
                                    return Ok(());
                                }

                                // Interim update — always is_final: false during session
                                if !trimmed.is_empty() {
                                    on_event_reader(AsrEvent {
                                        text: trimmed.to_string(),
                                        is_final: false,
                                        prefetch: false,
                                        definite: false,
                                        confidence: None,
                                    });
                                }
                            }
                        }
                        Ok(Message::Close(frame)) => {
                            return Err(AsrError::ConnectionFailed(format!(
                                "Soniox ASR WebSocket closed by server before finish: {:?}",
                                frame
                            ))
                            .in_phase(AsrPhase::Streaming));
                        }
                        Ok(other) => {
                            log::debug!("Soniox ASR received non-text frame: {:?}", other);
                        }
                        Err(e) => {
                            return Err(AsrError::ConnectionFailed(format!(
                                "Soniox ASR WebSocket read failed: {e}"
                            ))
                            .in_phase(AsrPhase::Streaming));
                        }
                    }
                }

                // Connection closed without finished — emit accumulated text as final
                let full = format!("{}{}", final_text, last_non_final);
                let trimmed = full.trim();
                if !trimmed.is_empty() {
                    on_event_reader(AsrEvent {
                        text: trimmed.to_string(),
                        is_final: true,
                        prefetch: false,
                        definite: true,
                        confidence: None,
                    });
                }

                Ok(())
            }
            .await;

            // Signal the writer to stop regardless of success/failure
            reader_cancel.cancel();
            result
        });

        // Stream audio — also watch writer_cancel so we stop if reader hit an error
        let resample_needed = sample_rate != stream_rate;
        if resample_needed {
            log::debug!(
                "Soniox ASR resampling input {} Hz -> {} Hz",
                sample_rate,
                stream_rate
            );
        }

        let mut audio_rx = audio_rx;
        let mut writer_error: Option<AsrError> = None;
        let mut sent_audio_chunks: u32 = 0;
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

            // Downmix to mono if stereo
            let mono = if channels > 1 {
                downmix_to_mono(&pcm, channels)
            } else {
                pcm
            };

            if let Err(e) = ws_write.send(Message::Binary(mono)).await {
                log::debug!("Soniox ASR audio send stopped: {e}");
                writer_error = Some(
                    AsrError::ConnectionFailed(format!("Soniox ASR audio send failed: {e}"))
                        .in_phase(AsrPhase::Streaming),
                );
                break;
            }

            sent_audio_chunks = sent_audio_chunks.saturating_add(1);
            if injected_fault == Some(SonioxFaultMode::DropAfterFirstAudio)
                && sent_audio_chunks >= 1
            {
                writer_error = Some(
                    AsrError::ConnectionFailed(
                        "Injected Soniox fault: connection dropped after first audio chunk"
                            .to_string(),
                    )
                    .in_phase(AsrPhase::Streaming),
                );
                break;
            }
        }

        if let Some(err) = writer_error {
            let _ = ws_write.close().await;
            let _ = tokio::time::timeout(Duration::from_millis(2000), reader_handle).await;
            return Err(err);
        }

        if cancel.is_cancelled() {
            let _ = ws_write.close().await;
            let _ = tokio::time::timeout(Duration::from_millis(2000), reader_handle).await;
            return Ok(());
        }

        if writer_cancel.is_cancelled() {
            let _ = ws_write.close().await;
            return match tokio::time::timeout(Duration::from_millis(2_000), reader_handle).await {
                Ok(Ok(result)) => result,
                Ok(Err(e)) => Err(AsrError::ConnectionFailed(format!(
                    "Soniox ASR reader task failed: {e}"
                ))
                .in_phase(AsrPhase::Streaming)),
                Err(_) => Err(AsrError::ConnectionFailed(
                    "Soniox ASR reader did not finish after stream cancellation".to_string(),
                )
                .in_phase(AsrPhase::Streaming)),
            };
        }

        // Signal end-of-audio
        if diagnostics_enabled {
            log::info!("SONIOX_DIAG sending end-of-audio");
        }
        if let Err(e) = ws_write.send(Message::Text(String::new())).await {
            return Err(AsrError::ConnectionFailed(format!(
                "Soniox ASR failed to send end-of-audio: {e}"
            ))
            .in_phase(AsrPhase::Finalizing));
        }

        if injected_fault == Some(SonioxFaultMode::FinalTimeout) {
            return Err(AsrError::ConnectionFailed(
                "Injected Soniox fault: finalizing timed out".to_string(),
            )
            .in_phase(AsrPhase::Finalizing));
        }

        // Wait for reader to finish
        match tokio::time::timeout(Duration::from_millis(15_000), reader_handle).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => Err(AsrError::ConnectionFailed(format!(
                "Soniox ASR reader task failed: {e}"
            ))
            .in_phase(AsrPhase::Finalizing)),
            Err(_) => {
                let _ = ws_write.close().await;
                Err(AsrError::ConnectionFailed(
                    "Soniox ASR timed out waiting for session finish".to_string(),
                )
                .in_phase(AsrPhase::Finalizing))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SonioxFaultMode {
    ConnectFail,
    HandshakeFail,
    ServerError401,
    ServerError429,
    ServerError502,
    DropAfterFirstAudio,
    FinalTimeout,
}

impl SonioxFaultMode {
    fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" => None,
            "connect_fail" => Some(Self::ConnectFail),
            "handshake_fail" => Some(Self::HandshakeFail),
            "server_error_401" | "auth_fail" => Some(Self::ServerError401),
            "server_error_429" | "rate_limit" => Some(Self::ServerError429),
            "server_error_502" | "bad_gateway" => Some(Self::ServerError502),
            "close_after_first_audio" | "drop_after_first_audio" => Some(Self::DropAfterFirstAudio),
            "final_timeout" | "stall_finalizing" => Some(Self::FinalTimeout),
            _ => None,
        }
    }

    fn from_env() -> Option<Self> {
        crate::services::asr_debug_service::AsrDebugService::soniox_fault_mode()
            .and_then(|value| Self::from_str(&value))
            .or_else(|| {
                std::env::var("VOICEX_SONIOX_FAULT")
                    .ok()
                    .and_then(|value| Self::from_str(&value))
            })
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::ConnectFail => "connect_fail",
            Self::HandshakeFail => "handshake_fail",
            Self::ServerError401 => "server_error_401",
            Self::ServerError429 => "server_error_429",
            Self::ServerError502 => "server_error_502",
            Self::DropAfterFirstAudio => "drop_after_first_audio",
            Self::FinalTimeout => "final_timeout",
        }
    }

    fn injected_server_status(&self) -> Option<u16> {
        match self {
            Self::ServerError401 => Some(401),
            Self::ServerError429 => Some(429),
            Self::ServerError502 => Some(502),
            _ => None,
        }
    }
}

fn resolve_soniox_ws_url() -> String {
    crate::services::asr_debug_service::AsrDebugService::soniox_ws_override()
        .or_else(|| std::env::var("VOICEX_SONIOX_WS_URL").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| SONIOX_DEFAULT_WS_URL.to_string())
}

#[cfg(test)]
mod tests {
    use super::SonioxFaultMode;

    #[test]
    fn parses_fault_aliases() {
        assert_eq!(
            SonioxFaultMode::from_str("close_after_first_audio"),
            Some(SonioxFaultMode::DropAfterFirstAudio)
        );
        assert_eq!(
            SonioxFaultMode::from_str("stall_finalizing"),
            Some(SonioxFaultMode::FinalTimeout)
        );
        assert_eq!(
            SonioxFaultMode::from_str("bad_gateway"),
            Some(SonioxFaultMode::ServerError502)
        );
    }
}

/// Downmix interleaved 16-bit PCM from N channels to mono by averaging.
fn downmix_to_mono(pcm: &[u8], channels: u16) -> Vec<u8> {
    let ch = channels as usize;
    if ch <= 1 {
        return pcm.to_vec();
    }
    let samples: Vec<i16> = pcm
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let frames = samples.len() / ch;
    let mut out = Vec::with_capacity(frames * 2);
    for f in 0..frames {
        let mut sum: i32 = 0;
        for c in 0..ch {
            sum += samples[f * ch + c] as i32;
        }
        let avg = (sum / ch as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        out.extend_from_slice(&avg.to_le_bytes());
    }
    out
}
