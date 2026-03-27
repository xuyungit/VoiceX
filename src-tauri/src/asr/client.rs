//! ASR WebSocket client (Volcengine bigmodel_async).

use std::io::{Read, Write};
use std::time::Duration;

use flate2::{write::GzEncoder, Compression};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
};

use super::audio_utils::resample_to_16k;
use super::config::AsrConfig;
use super::protocol::{
    decode_server_error_frame, decode_server_response, encode_audio_packet, encode_full_request,
    parse_header_size, parse_message_type, AsrError, AsrEvent,
};

/// ASR client for streaming speech recognition
pub struct AsrClient {
    config: AsrConfig,
}

impl AsrClient {
    pub fn new(config: AsrConfig) -> Self {
        Self { config }
    }

    /// Run a streaming session: send full request, then stream audio chunks until the channel closes.
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
            return Err(AsrError::ConnectionFailed(
                "Invalid ASR configuration".to_string(),
            ));
        }

        let stream_rate = if sample_rate != 16_000 {
            16_000
        } else {
            sample_rate
        };

        log::info!(
            "ASR connecting to {} (capture {} Hz -> stream {} Hz, {} ch)",
            self.config.ws_url,
            sample_rate,
            stream_rate,
            channels
        );

        let mut req = self
            .config
            .ws_url
            .clone()
            .into_client_request()
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;
        {
            let headers = req.headers_mut();
            if let Ok(v) = HeaderValue::from_str(&self.config.app_key) {
                headers.insert("X-Api-App-Key", v);
            }
            if let Ok(v) = HeaderValue::from_str(&self.config.access_key) {
                headers.insert("X-Api-Access-Key", v);
            }
            if let Ok(v) = HeaderValue::from_str(&self.config.resource_id) {
                headers.insert("X-Api-Resource-Id", v);
            }
            if let Ok(v) = HeaderValue::from_str(&uuid::Uuid::new_v4().to_string()) {
                headers.insert("X-Api-Connect-Id", v);
            }
        }

        let (ws_stream, resp) = connect_async(req)
            .await
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;

        if let Some(logid) = resp
            .headers()
            .get("X-Tt-Logid")
            .and_then(|v| v.to_str().ok())
        {
            log::info!("ASR connected, logid={}", logid);
        } else {
            log::debug!("ASR connected (no logid header)");
        }

        let (mut ws_write, mut ws_read) = ws_stream.split();
        log::debug!("ASR WebSocket connected");

        // Send full client request (gzip)
        log::info!(
            "ASR request: two_pass={}, end_window_size={:?}, force_to_speech_time={:?}, show_utterances={}, enable_ddc={}",
            self.config.enable_nonstream,
            self.config.end_window_size,
            self.config.force_to_speech_time,
            self.config.show_utterances,
            self.config.enable_ddc
        );
        let full_payload = build_full_request_payload(&self.config, stream_rate, channels, history);
        log::debug!(
            "ASR full request payload: {}",
            String::from_utf8_lossy(&full_payload)
        );
        let full_payload_gzip = gzip_compress(&full_payload)?;
        let full_packet = encode_full_request(&full_payload_gzip);
        ws_write
            .send(Message::Binary(full_packet))
            .await
            .map_err(|e| AsrError::ConnectionFailed(format!("Send full request failed: {e}")))?;
        log::debug!("ASR full request sent");

        let on_event = Arc::new(on_event);

        // Reader task
        let on_event_reader = on_event.clone();
        let mut msg_count: u32 = 0;
        let cancel_reader = cancel.clone();
        let reader_handle = tokio::spawn(async move {
            log::debug!("ASR reader task started");
            while let Some(msg) = tokio::select! {
                _ = cancel_reader.cancelled() => None,
                v = ws_read.next() => v,
            } {
                match msg {
                    Ok(Message::Binary(bin)) => {
                        msg_count += 1;
                        log::debug!("ASR recv #{} binary len={}", msg_count, bin.len());
                        if bin.len() < 4 {
                            log::warn!("ASR binary too short: {}", bin.len());
                            continue;
                        }

                        let header_size_words = std::cmp::max(parse_header_size(&bin), 1);
                        let header_bytes = header_size_words * 4;
                        let msg_type = parse_message_type(&bin).unwrap_or(0);
                        let flags = bin[1] & 0x0f;
                        let serialization = bin[2] >> 4;
                        let compression = bin[2] & 0x0f;

                        if msg_type == 0b1111 {
                            if let Some(err) = decode_server_error_frame(&bin) {
                                log::error!(
                                    "ASR server error code={} message={}",
                                    err.code,
                                    err.message
                                );
                            } else {
                                log::error!("ASR server error frame could not be decoded");
                            }
                            continue;
                        }

                        let mut cursor = header_bytes;

                        let mut is_last = false;
                        if flags & 0x01 != 0 {
                            if bin.len() < cursor + 4 {
                                log::warn!(
                                    "ASR binary shorter than seq (len={}, cursor={})",
                                    bin.len(),
                                    cursor
                                );
                                continue;
                            }
                            cursor += 4; // sequence
                        }
                        if flags & 0x02 != 0 {
                            is_last = true;
                        }
                        if flags & 0x04 != 0 {
                            if bin.len() < cursor + 4 {
                                log::warn!(
                                    "ASR binary shorter than event (len={}, cursor={})",
                                    bin.len(),
                                    cursor
                                );
                                continue;
                            }
                            cursor += 4; // event code
                        }

                        if bin.len() < cursor + 4 {
                            log::warn!(
                                "ASR binary shorter than payload size (len={}, cursor={})",
                                bin.len(),
                                cursor
                            );
                            continue;
                        }
                        let payload_size = u32::from_be_bytes([
                            bin[cursor],
                            bin[cursor + 1],
                            bin[cursor + 2],
                            bin[cursor + 3],
                        ]) as usize;
                        cursor += 4;
                        let available = bin.len().saturating_sub(cursor);
                        let effective_payload = if available < payload_size {
                            log::warn!(
                                "ASR payload truncated (len={}, payload_size={}, start={}, available={}); using available bytes",
                                bin.len(),
                                payload_size,
                                cursor,
                                available
                            );
                            available
                        } else {
                            payload_size
                        };
                        let payload_slice = &bin[cursor..cursor + effective_payload];

                        let payload_owned;
                        let payload = if compression == 0x01 {
                            match flate2::read::GzDecoder::new(payload_slice)
                                .bytes()
                                .collect::<Result<Vec<u8>, _>>()
                            {
                                Ok(decompressed) => {
                                    payload_owned = decompressed;
                                    payload_owned.as_slice()
                                }
                                Err(e) => {
                                    log::error!("ASR payload gzip decompress failed: {}", e);
                                    continue;
                                }
                            }
                        } else {
                            payload_slice
                        };

                        let _preview: String =
                            String::from_utf8_lossy(payload).chars().take(240).collect();
                        match msg_type {
                            0b1001 => {
                                if let Some(mut evt) = decode_server_response(payload) {
                                    // Log two-pass diagnostics: compare streaming vs nostream results
                                    if evt.prefetch {
                                        log::info!(
                                            "ASR 1st-pass (prefetch): \"{}\"",
                                            evt.text.chars().take(80).collect::<String>()
                                        );
                                    }
                                    // Treat protocol is_last flag as final, even if payload omitted definite/prefetch.
                                    if evt.prefetch && !is_last {
                                        evt.is_final = false;
                                    }
                                    if is_last {
                                        evt.is_final = true;
                                        evt.definite = true;
                                    }
                                    if evt.definite {
                                        log::info!(
                                            "ASR 2nd-pass (definite): \"{}\"",
                                            evt.text.chars().take(80).collect::<String>()
                                        );
                                    }
                                    on_event_reader(evt);
                                    if is_last {
                                        log::debug!("ASR final result received");
                                    }
                                } else {
                                    log::debug!(
                                        "ASR response could not be decoded (serialization={}): {}",
                                        serialization,
                                        String::from_utf8_lossy(payload)
                                    );
                                }
                            }
                            other => {
                                log::debug!("ASR message type {} ignored", other);
                            }
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        log::debug!("ASR WebSocket closed by server: {:?}", frame);
                        break;
                    }
                    Ok(other) => {
                        log::debug!("ASR received non-binary frame: {:?}", other);
                    }
                    Err(e) => {
                        log::warn!("ASR WebSocket read error: {}", e);
                        break;
                    }
                }
            }
            log::debug!("ASR reader task ended");
        });

        let resample_needed = sample_rate != stream_rate;
        if resample_needed {
            log::debug!(
                "ASR resampling input {} Hz -> {} Hz for streaming",
                sample_rate,
                stream_rate
            );
        }

        // Writer: stream audio (gzip each chunk)
        let mut audio_rx = audio_rx;
        let mut seq: u32 = 2;
        while let Some(chunk) = tokio::select! {
            _ = cancel.cancelled() => None,
            v = audio_rx.recv() => v,
        } {
            let pcm = if resample_needed {
                resample_to_16k(&chunk, sample_rate)
            } else {
                chunk
            };
            let compressed = gzip_compress(&pcm)?;
            let packet = encode_audio_packet(seq, &compressed, false);
            if let Err(e) = ws_write.send(Message::Binary(packet)).await {
                return Err(AsrError::ConnectionFailed(format!(
                    "Send audio failed: {e}"
                )));
            }
            seq += 1;
        }
        // Send last packet with negative seq
        let last_audio = gzip_compress(&[])?;
        let last = encode_audio_packet(seq, &last_audio, true);
        log::debug!(
            "ASR last packet sent (flags last, seq {}, payload_len={})",
            seq,
            last_audio.len()
        );
        ws_write
            .send(Message::Binary(last))
            .await
            .map_err(|e| AsrError::ConnectionFailed(format!("Send last packet failed: {e}")))?;

        // Wait briefly for server final responses / close
        match tokio::time::timeout(Duration::from_millis(5000), reader_handle).await {
            Ok(_) => log::debug!("ASR reader completed after final packet"),
            Err(_) => {
                if cancel.is_cancelled() {
                    log::debug!("ASR reader aborted due to cancel");
                } else {
                    log::warn!("ASR reader timed out waiting for final response (waited 5s)");
                }
            }
        }

        Ok(())
    }
}

fn build_full_request_payload(
    config: &AsrConfig,
    sample_rate: u32,
    channels: u16,
    history: Vec<String>,
) -> Vec<u8> {
    let mut request_obj = json!({
        "model_name": config.model_name,
        "enable_nonstream": config.enable_nonstream,
        "show_utterances": config.show_utterances,
        "enable_itn": config.enable_itn,
        "enable_punc": config.enable_punc,
        "enable_ddc": config.enable_ddc,
        "enable_accelerate_text": config.enable_accelerate_text,
        "accelerate_score": config.accelerate_score,
        "end_window_size": config.end_window_size,
        "force_to_speech_time": config.force_to_speech_time,
    });

    let context = build_corpus_context(&config.hotwords, history);
    // Build corpus object: prefer inline hotwords over online table to avoid
    // potential conflicts from sending the same words through both channels.
    let has_context = !context.is_empty();
    let has_boosting_id = config.online_hotword_id.is_some();
    if has_context || has_boosting_id {
        let mut corpus = serde_json::Map::new();
        if has_boosting_id && !has_context {
            // Only use online table when no inline hotwords/history are present
            if let Some(ref id) = config.online_hotword_id {
                corpus.insert("boosting_table_id".to_string(), json!(id));
            }
        } else if has_boosting_id {
            log::info!(
                "ASR: skipping boosting_table_id because inline context is present (avoids duplicate hotwords)"
            );
        }
        if has_context {
            corpus.insert("context".to_string(), json!(context));
        }
        if let Some(obj) = request_obj.as_object_mut() {
            obj.insert("corpus".to_string(), serde_json::Value::Object(corpus));
        }
    }

    let req = json!({
        "user": {
            "uid": "voicex-desktop"
        },
        "audio": {
            "format": "pcm",
            "codec": "raw",
            "rate": sample_rate,
            "bits": 16,
            "channel": channels
        },
        "request": request_obj
    });

    serde_json::to_vec(&req).unwrap_or_default()
}

fn build_corpus_context(hotwords: &[String], history: Vec<String>) -> String {
    if hotwords.is_empty() && history.is_empty() {
        return "".to_string();
    }

    let mut context_obj = json!({});

    // 1. Hotwords (Limit to 100 words to stay within 100 token limit for bigmodel_async)
    if !hotwords.is_empty() {
        let limit = 100;
        if hotwords.len() > limit {
            log::warn!(
                "Hotwords truncated: {} -> {} (bigmodel_async 100-token limit)",
                hotwords.len(),
                limit
            );
        }
        let words: Vec<_> = hotwords
            .iter()
            .take(limit)
            .map(|w| json!({"word": w}))
            .collect();
        context_obj["hotwords"] = json!(words);
    }

    // 2. Dialog Context (Recent 3 rounds, max 800 tokens by API)
    if !history.is_empty() {
        let context_data: Vec<_> = history.iter().map(|text| json!({ "text": text })).collect();
        context_obj["context_type"] = json!("dialog_ctx");
        context_obj["context_data"] = json!(context_data);
    }

    serde_json::to_string(&context_obj).unwrap_or_default()
}

fn gzip_compress(data: &[u8]) -> Result<Vec<u8>, AsrError> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .map_err(|e| AsrError::CompressionFailed(e.to_string()))?;
    encoder
        .finish()
        .map_err(|e| AsrError::CompressionFailed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::config::AsrConfig;

    #[test]
    fn test_build_corpus_context_merged() {
        let hotwords = vec!["VoiceX".to_string(), "Antigravity".to_string()];
        let history = vec!["Hello world".to_string()];

        let context_str = build_corpus_context(&hotwords, history);
        let context: serde_json::Value = serde_json::from_str(&context_str).unwrap();

        assert_eq!(context["hotwords"][0]["word"], "VoiceX");
        assert_eq!(context["hotwords"][1]["word"], "Antigravity");
        assert_eq!(context["context_type"], "dialog_ctx");
        assert_eq!(context["context_data"][0]["text"], "Hello world");
    }

    #[test]
    fn test_build_full_request_payload_with_context() {
        let mut config = AsrConfig::default();
        config.hotwords = vec!["VoiceX".to_string()];
        let history = vec!["Previous text".to_string()];

        let payload_bytes = build_full_request_payload(&config, 16000, 1, history);
        let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).unwrap();

        let context_str = payload["request"]["corpus"]["context"].as_str().unwrap();
        let context: serde_json::Value = serde_json::from_str(context_str).unwrap();

        assert_eq!(context["hotwords"][0]["word"], "VoiceX");
        assert_eq!(context["context_data"][0]["text"], "Previous text");
    }

    #[test]
    fn test_build_full_request_payload_with_boosting_table_id() {
        let mut config = AsrConfig::default();
        config.online_hotword_id = Some("test-table-id-12345".to_string());

        let payload_bytes = build_full_request_payload(&config, 16000, 1, vec![]);
        let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).unwrap();

        let boosting_id = payload["request"]["corpus"]["boosting_table_id"]
            .as_str()
            .unwrap();
        assert_eq!(boosting_id, "test-table-id-12345");
        // context should not exist when no hotwords/history
        assert!(payload["request"]["corpus"]["context"].is_null());
    }

    #[test]
    fn test_boosting_table_id_skipped_when_inline_hotwords_present() {
        let mut config = AsrConfig::default();
        config.online_hotword_id = Some("test-table-id-12345".to_string());
        config.hotwords = vec!["VoiceX".to_string()];

        let payload_bytes = build_full_request_payload(&config, 16000, 1, vec![]);
        let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).unwrap();

        // boosting_table_id should be absent when inline hotwords are present
        assert!(payload["request"]["corpus"]["boosting_table_id"].is_null());
        // inline hotwords should still be present
        let context_str = payload["request"]["corpus"]["context"].as_str().unwrap();
        let context: serde_json::Value = serde_json::from_str(context_str).unwrap();
        assert_eq!(context["hotwords"][0]["word"], "VoiceX");
    }

    #[test]
    fn test_build_full_request_payload_no_corpus_when_empty() {
        let config = AsrConfig::default();
        let payload_bytes = build_full_request_payload(&config, 16000, 1, vec![]);
        let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).unwrap();

        // corpus should not exist when no hotwords, no history, no boosting_table_id
        assert!(payload["request"]["corpus"].is_null());
    }
}
