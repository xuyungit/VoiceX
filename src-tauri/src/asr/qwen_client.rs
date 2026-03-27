//! Qwen realtime ASR WebSocket client.

use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc::Receiver;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
};

use super::audio_utils::resample_to_16k;
use super::config::AsrConfig;
use super::protocol::{AsrError, AsrEvent};

pub struct QwenRealtimeClient {
    config: AsrConfig,
}

impl QwenRealtimeClient {
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
                "Invalid Qwen ASR configuration".to_string(),
            ));
        }

        let stream_rate = if sample_rate == 16_000 {
            sample_rate
        } else {
            16_000
        };
        let ws_url = build_ws_url(&self.config.qwen_ws_url, &self.config.qwen_model);
        let diagnostics_enabled = self.config.enable_diagnostics;

        log::info!(
            "Qwen ASR connecting to {} (capture {} Hz -> stream {} Hz, {} ch, model={}, lang={})",
            self.config.qwen_ws_url,
            sample_rate,
            stream_rate,
            channels,
            self.config.qwen_model,
            if self.config.qwen_language.trim().is_empty() {
                "auto"
            } else {
                self.config.qwen_language.as_str()
            },
        );

        let mut req = ws_url
            .into_client_request()
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;
        {
            let headers = req.headers_mut();
            let auth = format!("Bearer {}", self.config.qwen_api_key);
            if let Ok(v) = HeaderValue::from_str(&auth) {
                headers.insert("Authorization", v);
            }
            headers.insert("OpenAI-Beta", HeaderValue::from_static("realtime=v1"));
        }

        let (ws_stream, _) = connect_async(req)
            .await
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;
        let (mut ws_write, mut ws_read) = ws_stream.split();

        // Build corpus text for filtering the phantom echo item that Qwen
        // generates from silence at session start (see corpus_echo filter below).
        let corpus_for_filter = if self.config.hotwords.is_empty() {
            String::new()
        } else {
            self.config.hotwords.join(", ")
        };

        let on_event = Arc::new(on_event);
        let on_event_reader = on_event.clone();
        let cancel_reader = cancel.clone();
        let diagnostics_reader = diagnostics_enabled;
        let reader_handle = tokio::spawn(async move {
            let mut transcript = TranscriptAccumulator::default();
            let mut saw_session_finished = false;
            // Qwen echoes corpus.text as a phantom first transcription item
            // (duration=0, generated from initial silence). Track whether we
            // have already stripped it so we only filter once.
            let mut corpus_echo_stripped = corpus_for_filter.is_empty();

            while let Some(msg) = tokio::select! {
                _ = cancel_reader.cancelled() => None,
                v = ws_read.next() => v,
            } {
                match msg {
                    Ok(Message::Text(text)) => {
                        if diagnostics_reader {
                            log::info!("QWEN_DIAG inbound_raw {}", text);
                        }
                        let payload: Value = serde_json::from_str(&text).map_err(|e| {
                            AsrError::ProtocolError(format!("Invalid Qwen JSON event: {e}"))
                        })?;
                        let event_type = payload
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or_default();

                        match event_type {
                            "session.created" => {
                                if let Some(session_id) = payload
                                    .get("session")
                                    .and_then(|v| v.get("id"))
                                    .and_then(Value::as_str)
                                {
                                    log::info!("Qwen ASR session created: {}", session_id);
                                } else {
                                    log::debug!("Qwen ASR session created");
                                }
                            }
                            "conversation.item.input_audio_transcription.text" => {
                                if let Some(text) = extract_partial_text(&payload) {
                                    if let Some(combined) = transcript.push_partial(&text) {
                                        // Suppress partials that are still building up
                                        // the corpus echo (prefix of corpus text).
                                        if !corpus_echo_stripped
                                            && is_corpus_echo_partial(&combined, &corpus_for_filter)
                                        {
                                            continue;
                                        }
                                        on_event_reader(AsrEvent {
                                            text: combined,
                                            is_final: false,
                                            prefetch: false,
                                            definite: false,
                                            confidence: None,
                                        });
                                    }
                                }
                            }
                            "conversation.item.input_audio_transcription.completed" => {
                                // Detect phantom corpus echo: duration=0 means no real
                                // audio was transcribed — the server hallucinated the
                                // corpus.text content from initial silence.
                                if !corpus_echo_stripped {
                                    let duration = payload
                                        .get("usage")
                                        .and_then(|u| u.get("duration"))
                                        .and_then(Value::as_u64)
                                        .unwrap_or(1);
                                    corpus_echo_stripped = true;
                                    if duration == 0 {
                                        transcript = TranscriptAccumulator::default();
                                        log::info!(
                                            "Qwen ASR: filtered corpus echo (usage.duration=0)"
                                        );
                                        continue;
                                    }
                                }

                                if let Some(text) =
                                    extract_text(&payload, &["transcript", "text", "stash"])
                                {
                                    if let Some(combined) = transcript.push_final(&text) {
                                        on_event_reader(AsrEvent {
                                            text: combined,
                                            is_final: true,
                                            prefetch: false,
                                            definite: true,
                                            confidence: None,
                                        });
                                    }
                                }
                            }
                            "input_audio_buffer.speech_started" => {
                                log::debug!("Qwen ASR VAD speech started");
                            }
                            "input_audio_buffer.speech_stopped" => {
                                log::debug!("Qwen ASR VAD speech stopped");
                            }
                            "session.finished" => {
                                saw_session_finished = true;
                                if let Some(text) =
                                    extract_text(&payload, &["transcript", "text", "stash"])
                                {
                                    if let Some(combined) = transcript.finish_with_text(&text) {
                                        on_event_reader(AsrEvent {
                                            text: combined,
                                            is_final: true,
                                            prefetch: false,
                                            definite: true,
                                            confidence: None,
                                        });
                                    }
                                } else if let Some(combined) = transcript.flush_pending() {
                                    on_event_reader(AsrEvent {
                                        text: combined,
                                        is_final: true,
                                        prefetch: false,
                                        definite: true,
                                        confidence: None,
                                    });
                                }
                                break;
                            }
                            "error" => {
                                return Err(AsrError::ServerError(extract_error_message(&payload)));
                            }
                            other => {
                                if payload.get("error").is_some() {
                                    return Err(AsrError::ServerError(extract_error_message(
                                        &payload,
                                    )));
                                }
                                log::debug!("Qwen ASR event ignored: {}", other);
                            }
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        log::debug!("Qwen ASR WebSocket closed by server: {:?}", frame);
                        break;
                    }
                    Ok(Message::Binary(bin)) => {
                        log::debug!(
                            "Qwen ASR received unexpected binary frame len={}",
                            bin.len()
                        );
                    }
                    Ok(other) => {
                        log::debug!("Qwen ASR received non-text frame: {:?}", other);
                    }
                    Err(e) => {
                        return Err(AsrError::ConnectionFailed(format!(
                            "Qwen ASR WebSocket read failed: {e}"
                        )));
                    }
                }
            }

            if !saw_session_finished {
                if let Some(combined) = transcript.flush_pending() {
                    on_event_reader(AsrEvent {
                        text: combined,
                        is_final: true,
                        prefetch: false,
                        definite: true,
                        confidence: None,
                    });
                }
            }

            Ok::<(), AsrError>(())
        });

        let session_update = build_session_update(
            stream_rate,
            &self.config.qwen_language,
            &self.config.hotwords,
        );
        if diagnostics_enabled {
            log::info!("QWEN_DIAG outbound_session_update {}", session_update);
        }
        ws_write
            .send(Message::Text(session_update.to_string()))
            .await
            .map_err(|e| {
                AsrError::ConnectionFailed(format!("Failed to send Qwen session.update: {e}"))
            })?;

        let resample_needed = sample_rate != stream_rate;
        if resample_needed {
            log::debug!(
                "Qwen ASR resampling input {} Hz -> {} Hz",
                sample_rate,
                stream_rate
            );
        }

        let mut audio_rx = audio_rx;
        let mut event_seq: u64 = 1;
        while let Some(chunk) = tokio::select! {
            _ = cancel.cancelled() => None,
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

            let append_event = json!({
                "event_id": format!("audio_{}", event_seq),
                "type": "input_audio_buffer.append",
                "audio": STANDARD.encode(pcm),
            });
            ws_write
                .send(Message::Text(append_event.to_string()))
                .await
                .map_err(|e| {
                    AsrError::ConnectionFailed(format!("Failed to send Qwen audio chunk: {e}"))
                })?;
            event_seq += 1;
        }

        if cancel.is_cancelled() {
            let _ = ws_write.close().await;
            let _ = tokio::time::timeout(Duration::from_millis(1000), reader_handle).await;
            return Ok(());
        }

        let finish_event = json!({
            "event_id": format!("finish_{}", event_seq),
            "type": "session.finish",
        });
        if diagnostics_enabled {
            log::info!("QWEN_DIAG outbound_session_finish {}", finish_event);
        }
        ws_write
            .send(Message::Text(finish_event.to_string()))
            .await
            .map_err(|e| {
                AsrError::ConnectionFailed(format!("Failed to send Qwen session.finish: {e}"))
            })?;

        match tokio::time::timeout(Duration::from_millis(10_000), reader_handle).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => Err(AsrError::ConnectionFailed(format!(
                "Qwen ASR reader task failed: {e}"
            ))),
            Err(_) => {
                log::warn!("Qwen ASR timed out waiting for session.finished");
                let _ = ws_write.close().await;
                Ok(())
            }
        }
    }
}

#[derive(Debug, Default)]
struct TranscriptAccumulator {
    accumulated_final: String,
    pending_partial: String,
    last_emitted_final: String,
}

impl TranscriptAccumulator {
    fn push_partial(&mut self, text: &str) -> Option<String> {
        let incoming = text.trim();
        if incoming.is_empty() {
            return None;
        }

        let combined = merge_transcript(&self.accumulated_final, incoming);
        self.pending_partial = combined
            .strip_prefix(&self.accumulated_final)
            .unwrap_or(incoming)
            .to_string();

        Some(combined)
    }

    fn push_final(&mut self, text: &str) -> Option<String> {
        self.finish_with_text(text)
    }

    fn finish_with_text(&mut self, text: &str) -> Option<String> {
        let incoming = text.trim();
        if incoming.is_empty() {
            return self.flush_pending();
        }

        let combined = merge_transcript(&self.accumulated_final, incoming);
        self.pending_partial.clear();

        if combined == self.last_emitted_final {
            self.accumulated_final = combined;
            return None;
        }

        self.accumulated_final = combined.clone();
        self.last_emitted_final = combined.clone();
        Some(combined)
    }

    fn flush_pending(&mut self) -> Option<String> {
        if self.pending_partial.is_empty() {
            return None;
        }

        let combined = format!("{}{}", self.accumulated_final, self.pending_partial);
        self.pending_partial.clear();

        if combined == self.last_emitted_final {
            self.accumulated_final = combined;
            return None;
        }

        self.accumulated_final = combined.clone();
        self.last_emitted_final = combined.clone();
        Some(combined)
    }
}

fn merge_transcript(accumulated: &str, incoming: &str) -> String {
    if accumulated.is_empty() {
        return incoming.to_string();
    }
    if incoming.starts_with(accumulated) {
        return incoming.to_string();
    }
    if accumulated.ends_with(incoming) {
        return accumulated.to_string();
    }
    format!("{}{}", accumulated, incoming)
}

fn build_ws_url(base_url: &str, model: &str) -> String {
    let separator = if base_url.contains('?') { '&' } else { '?' };
    format!(
        "{}{}model={}",
        base_url,
        separator,
        urlencoding::encode(model)
    )
}

fn build_session_update(sample_rate: u32, language: &str, hotwords: &[String]) -> Value {
    let mut transcription = serde_json::Map::new();
    if !language.trim().is_empty() {
        transcription.insert(
            "language".to_string(),
            Value::String(language.trim().to_string()),
        );
    }

    // Qwen3-ASR supports context biasing via corpus.text (max 10,000 tokens).
    // See: https://www.alibabacloud.com/help/en/model-studio/qwen-asr-realtime-client-events
    if !hotwords.is_empty() {
        let corpus_text = hotwords.join(", ");
        let mut corpus = serde_json::Map::new();
        corpus.insert("text".to_string(), Value::String(corpus_text));
        transcription.insert("corpus".to_string(), Value::Object(corpus));
    }

    json!({
        "event_id": "session_update",
        "type": "session.update",
        "session": {
            "modalities": ["text"],
            "input_audio_format": "pcm",
            "sample_rate": sample_rate,
            "input_audio_transcription": Value::Object(transcription),
            "turn_detection": {
                "type": "server_vad",
                "threshold": 0.0,
                "silence_duration_ms": 400,
            },
        }
    })
}

/// Strip all punctuation and whitespace for fuzzy corpus echo comparison.
/// The server may reformat corpus text (remove separators, switch to
/// full-width punctuation, insert periods), so we compare content only.
fn strip_punct(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '.' | '。' | ',' | '，' | '、' | ' ' | '\t' | ';' | '；'))
        .collect()
}

/// Check if `text` is still building up toward the corpus text (prefix match).
fn is_corpus_echo_partial(text: &str, corpus: &str) -> bool {
    if corpus.is_empty() {
        return false;
    }
    let t = strip_punct(text);
    let c = strip_punct(corpus);
    c.starts_with(&t)
}

#[cfg(test)]
fn is_corpus_echo(text: &str, corpus: &str) -> bool {
    if corpus.is_empty() {
        return false;
    }
    strip_punct(text) == strip_punct(corpus)
}

fn extract_text(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        payload
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn extract_partial_text(payload: &Value) -> Option<String> {
    let text = payload
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let stash = payload
        .get("stash")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();

    match (text.is_empty(), stash.is_empty()) {
        (true, true) => None,
        (false, true) => Some(text.to_string()),
        (true, false) => Some(stash.to_string()),
        (false, false) => Some(format!("{}{}", text, stash)),
    }
}

fn extract_error_message(payload: &Value) -> String {
    payload
        .get("error")
        .and_then(|v| {
            v.get("message")
                .and_then(Value::as_str)
                .or_else(|| v.as_str())
        })
        .or_else(|| payload.get("message").and_then(Value::as_str))
        .unwrap_or("Unknown Qwen ASR error")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        build_session_update, build_ws_url, extract_partial_text, is_corpus_echo,
        is_corpus_echo_partial, TranscriptAccumulator,
    };
    use serde_json::json;

    #[test]
    fn test_build_ws_url_appends_model() {
        assert_eq!(
            build_ws_url(
                "wss://dashscope.aliyuncs.com/api-ws/v1/realtime",
                "qwen3-asr-flash-realtime"
            ),
            "wss://dashscope.aliyuncs.com/api-ws/v1/realtime?model=qwen3-asr-flash-realtime"
        );
    }

    #[test]
    fn test_build_ws_url_preserves_existing_query() {
        assert_eq!(
            build_ws_url(
                "wss://dashscope.aliyuncs.com/api-ws/v1/realtime?foo=bar",
                "qwen3-asr-flash-realtime"
            ),
            "wss://dashscope.aliyuncs.com/api-ws/v1/realtime?foo=bar&model=qwen3-asr-flash-realtime"
        );
    }

    #[test]
    fn test_session_update_omits_empty_language() {
        let payload = build_session_update(16000, "", &[]);
        let language = payload["session"]["input_audio_transcription"]["language"].as_str();
        assert!(language.is_none());
        // No corpus when hotwords empty
        assert!(payload["session"]["input_audio_transcription"]["corpus"].is_null());
    }

    #[test]
    fn test_session_update_includes_corpus_text() {
        let hotwords = vec![
            "VoiceX".to_string(),
            "阿里巴巴".to_string(),
            "语音识别".to_string(),
        ];
        let payload = build_session_update(16000, "zh", &hotwords);
        let corpus_text = payload["session"]["input_audio_transcription"]["corpus"]["text"]
            .as_str()
            .unwrap();
        assert_eq!(corpus_text, "VoiceX, 阿里巴巴, 语音识别");
    }

    #[test]
    fn test_transcript_accumulator_merges_partial_and_final() {
        let mut accumulator = TranscriptAccumulator::default();

        assert_eq!(accumulator.push_partial("你好").as_deref(), Some("你好"));
        assert_eq!(
            accumulator.push_final("你好世界").as_deref(),
            Some("你好世界")
        );
        assert_eq!(
            accumulator.push_partial("，再次问好").as_deref(),
            Some("你好世界，再次问好")
        );
        assert_eq!(
            accumulator.flush_pending().as_deref(),
            Some("你好世界，再次问好")
        );
    }

    #[test]
    fn test_transcript_accumulator_deduplicates_session_finish() {
        let mut accumulator = TranscriptAccumulator::default();

        assert_eq!(accumulator.push_final("第一句").as_deref(), Some("第一句"));
        assert_eq!(accumulator.finish_with_text("第一句").as_deref(), None);
    }

    #[test]
    fn test_corpus_echo_partial_detection() {
        let corpus = "连续刚构桥, 剪力, 有限元";

        // Prefixes of corpus text should be detected
        assert!(is_corpus_echo_partial("连续", corpus));
        assert!(is_corpus_echo_partial("连续刚构桥", corpus));
        assert!(is_corpus_echo_partial("连续刚构桥, 剪力", corpus));
        // With trailing punctuation
        assert!(is_corpus_echo_partial("连续刚构桥, 剪力,", corpus));
        // Server strips separators — still a prefix after normalization
        assert!(is_corpus_echo_partial("连续刚构桥剪力", corpus));
        assert!(is_corpus_echo_partial("连续刚构桥剪力有限", corpus));

        // Real speech should NOT be detected
        assert!(!is_corpus_echo_partial("出现一个", corpus));
        assert!(!is_corpus_echo_partial("连续刚构桥剪力有限元出现", corpus));

        // Empty corpus never matches
        assert!(!is_corpus_echo_partial("任何文字", ""));
    }

    #[test]
    fn test_corpus_echo_full_match() {
        let corpus = "连续刚构桥, 剪力, 有限元";

        // Exact match
        assert!(is_corpus_echo(corpus, corpus));
        // With trailing punctuation added by model
        assert!(is_corpus_echo("连续刚构桥, 剪力, 有限元.", corpus));
        assert!(is_corpus_echo("连续刚构桥, 剪力, 有限元。", corpus));
        // Server reformats: strips commas, uses full-width punctuation
        assert!(is_corpus_echo("连续刚构桥剪力有限元。", corpus));
        assert!(is_corpus_echo("连续刚构桥，剪力，有限元。", corpus));

        // Partial should NOT match as full echo
        assert!(!is_corpus_echo("连续刚构桥, 剪力", corpus));
        // Real speech appended should NOT match
        assert!(!is_corpus_echo("连续刚构桥剪力有限元。出现", corpus));
    }

    #[test]
    fn test_extract_partial_text_combines_text_and_stash() {
        let payload = json!({
            "text": "好的，我现在开始测试。嗯，",
            "stash": "我发现这个模型的速度非常",
        });

        assert_eq!(
            extract_partial_text(&payload).as_deref(),
            Some("好的，我现在开始测试。嗯，我发现这个模型的速度非常")
        );
    }
}
