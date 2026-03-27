//! Gemini Live API WebSocket client.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc::Receiver;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::audio_utils::resample_to_16k;
use super::config::AsrConfig;
use super::protocol::{AsrError, AsrEvent};

const GEMINI_LIVE_WS_URL: &str = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";
const SETUP_COMPLETE_WAIT_MS: u64 = 1500;
const STREAM_COMPLETION_WAIT_MS: u64 = 10_000;
const SILENT_STREAM_COMPLETION_WAIT_MS: u64 = 1_500;

pub struct GeminiLiveClient {
    config: AsrConfig,
}

impl GeminiLiveClient {
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
                "Invalid Gemini Live configuration".to_string(),
            ));
        }

        let stream_rate = if sample_rate == 16_000 {
            sample_rate
        } else {
            16_000
        };
        let diagnostics_enabled = self.config.enable_diagnostics;
        let ws_url = format!("{}?key={}", GEMINI_LIVE_WS_URL, self.config.gemini_api_key);

        log::info!(
            "Gemini Live connecting (capture {} Hz -> stream {} Hz, {} ch, model={})",
            sample_rate,
            stream_rate,
            channels,
            self.config.gemini_live_model,
        );

        let (ws_stream, _) = connect_async(&ws_url)
            .await
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;
        let (mut ws_write, mut ws_read) = ws_stream.split();

        let setup_message = build_setup_message(&self.config);
        if diagnostics_enabled {
            log::info!("Gemini Live setup payload: {}", setup_message);
        }
        ws_write
            .send(Message::Text(setup_message.to_string()))
            .await
            .map_err(|e| {
                AsrError::ConnectionFailed(format!("Failed to send Gemini Live setup: {e}"))
            })?;

        loop {
            let msg = tokio::select! {
                _ = cancel.cancelled() => None,
                v = tokio::time::timeout(Duration::from_millis(SETUP_COMPLETE_WAIT_MS), ws_read.next()) => {
                    match v {
                        Ok(msg) => msg,
                        Err(_) => {
                            log::warn!(
                                "Gemini Live setupComplete not received within {} ms; proceeding with audio stream",
                                SETUP_COMPLETE_WAIT_MS
                            );
                            break;
                        }
                    }
                },
            };

            match msg {
                Some(Ok(Message::Text(text))) => {
                    let payload = parse_json_message(Message::Text(text), "setup")?;
                    if diagnostics_enabled {
                        log::info!("Gemini Live setup inbound: {}", payload);
                    }

                    if let Some(reason) = extract_server_error(&payload) {
                        return Err(AsrError::ServerError(reason));
                    }

                    if payload.get("setupComplete").is_some() {
                        log::info!("Gemini Live setupComplete received");
                        break;
                    }
                }
                Some(Ok(Message::Binary(bin))) => {
                    let payload = parse_json_message(Message::Binary(bin), "setup")?;
                    if diagnostics_enabled {
                        log::info!("Gemini Live setup inbound: {}", payload);
                    }

                    if let Some(reason) = extract_server_error(&payload) {
                        return Err(AsrError::ServerError(reason));
                    }

                    if payload.get("setupComplete").is_some() {
                        log::info!("Gemini Live setupComplete received");
                        break;
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    return Err(AsrError::ConnectionFailed(format!(
                        "Gemini Live closed before setup completed: {:?}",
                        frame
                    )));
                }
                Some(Ok(other)) => {
                    if diagnostics_enabled {
                        log::info!("Gemini Live setup non-text frame: {:?}", other);
                    }
                }
                Some(Err(e)) => {
                    return Err(AsrError::ConnectionFailed(format!(
                        "Gemini Live setup read failed: {e}"
                    )));
                }
                None => {
                    return Err(AsrError::ConnectionFailed(
                        "Gemini Live closed before setup completed".to_string(),
                    ));
                }
            }
        }

        let on_event = Arc::new(on_event);
        let on_event_reader = on_event.clone();
        let cancel_reader = cancel.clone();
        let audio_stream_ended = Arc::new(AtomicBool::new(false));
        let audio_stream_ended_reader = audio_stream_ended.clone();
        let saw_input_transcription = Arc::new(AtomicBool::new(false));
        let saw_input_transcription_reader = saw_input_transcription.clone();
        let reader_handle = tokio::spawn(async move {
            let mut transcript = GeminiTranscriptAccumulator::default();

            while let Some(msg) = tokio::select! {
                _ = cancel_reader.cancelled() => None,
                v = ws_read.next() => v,
            } {
                match msg {
                    Ok(Message::Text(text)) => {
                        let payload = parse_json_message(Message::Text(text), "runtime")?;
                        let should_stop = handle_runtime_payload(
                            &payload,
                            &mut transcript,
                            &on_event_reader,
                            audio_stream_ended_reader.load(Ordering::SeqCst),
                            diagnostics_enabled,
                            &saw_input_transcription_reader,
                        )?;
                        if should_stop {
                            break;
                        }
                    }
                    Ok(Message::Binary(bin)) => {
                        let payload = parse_json_message(Message::Binary(bin), "runtime")?;
                        let should_stop = handle_runtime_payload(
                            &payload,
                            &mut transcript,
                            &on_event_reader,
                            audio_stream_ended_reader.load(Ordering::SeqCst),
                            diagnostics_enabled,
                            &saw_input_transcription_reader,
                        )?;
                        if should_stop {
                            break;
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        log::debug!("Gemini Live WebSocket closed by server: {:?}", frame);
                        break;
                    }
                    Ok(other) => {
                        log::debug!("Gemini Live received non-text frame: {:?}", other);
                    }
                    Err(e) => {
                        return Err(AsrError::ConnectionFailed(format!(
                            "Gemini Live read failed: {e}"
                        )));
                    }
                }
            }

            Ok::<(), AsrError>(())
        });

        let resample_needed = sample_rate != stream_rate;
        if resample_needed {
            log::debug!(
                "Gemini Live resampling input {} Hz -> {} Hz",
                sample_rate,
                stream_rate
            );
        }

        let mut audio_rx = audio_rx;
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

            let audio_message = json!({
                "realtimeInput": {
                    "audio": {
                        "data": STANDARD.encode(pcm),
                        "mimeType": format!("audio/pcm;rate={}", stream_rate),
                    }
                }
            });
            ws_write
                .send(Message::Text(audio_message.to_string()))
                .await
                .map_err(|e| {
                    AsrError::ConnectionFailed(format!(
                        "Failed to send Gemini Live audio chunk: {e}"
                    ))
                })?;
        }

        if cancel.is_cancelled() {
            let _ = ws_write.close().await;
            let _ = tokio::time::timeout(Duration::from_millis(1000), reader_handle).await;
            return Ok(());
        }

        ws_write
            .send(Message::Text(
                json!({
                    "realtimeInput": {
                        "audioStreamEnd": true,
                    }
                })
                .to_string(),
            ))
            .await
            .map_err(|e| {
                AsrError::ConnectionFailed(format!(
                    "Failed to send Gemini Live audioStreamEnd: {e}"
                ))
            })?;
        audio_stream_ended.store(true, Ordering::SeqCst);
        log::info!("Gemini Live audioStreamEnd sent");

        let completion_wait_ms = if saw_input_transcription.load(Ordering::SeqCst) {
            STREAM_COMPLETION_WAIT_MS
        } else {
            SILENT_STREAM_COMPLETION_WAIT_MS
        };

        match tokio::time::timeout(Duration::from_millis(completion_wait_ms), reader_handle).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => Err(AsrError::ConnectionFailed(format!(
                "Gemini Live reader task failed: {e}"
            ))),
            Err(_) => {
                log::warn!(
                    "Gemini Live timed out waiting for stream completion ({} ms)",
                    completion_wait_ms
                );
                let _ = ws_write.close().await;
                Ok(())
            }
        }
    }
}

fn build_setup_message(config: &AsrConfig) -> Value {
    json!({
        "setup": {
            "model": format!("models/{}", config.gemini_live_model.trim()),
            "generationConfig": {
                "responseModalities": ["AUDIO"],
            },
            "systemInstruction": {
                "parts": [{
                    "text": build_system_instruction(&config.gemini_language),
                }]
            },
            "realtimeInputConfig": {
                "automaticActivityDetection": {
                    "disabled": false,
                }
            },
            "inputAudioTranscription": {},
        }
    })
}

fn build_system_instruction(language: &str) -> String {
    let language_hint = match language.trim() {
        "zh" => {
            "The speaker primarily uses Mandarin Chinese, with occasional English words or short phrases. Prefer Simplified Chinese transcription and preserve spoken English terms when they appear. When the audio is ambiguous, avoid misclassifying it as another language."
        }
        "en" => "The expected spoken language is primarily English.",
        "zh-en" => {
            "The speaker primarily uses Mandarin Chinese, but may naturally mix in English words, names, or short phrases. Prefer Simplified Chinese transcription for the main sentence and preserve spoken English terms when they appear. When the audio is ambiguous, avoid misclassifying it as another language."
        }
        _ => {
            "Auto-detect the spoken language, but strongly bias toward Mandarin Chinese with occasional English words or short phrases. Prefer Simplified Chinese transcription when the audio is ambiguous, and avoid misclassifying it as another language."
        }
    };

    format!(
        "You are the internal realtime transcription engine for a desktop dictation app.\n{}\nKeep any spoken reply extremely short, ideally a single brief acknowledgement. Do not answer the user's request, do not translate, and do not add extra commentary.",
        language_hint
    )
}

fn extract_server_error(payload: &Value) -> Option<String> {
    if let Some(message) = payload
        .get("error")
        .and_then(|v| v.get("message"))
        .and_then(Value::as_str)
    {
        return Some(message.to_string());
    }

    None
}

fn handle_runtime_payload(
    payload: &Value,
    transcript: &mut GeminiTranscriptAccumulator,
    on_event: &Arc<impl Fn(AsrEvent) + Send + Sync + 'static>,
    audio_stream_ended: bool,
    diagnostics_enabled: bool,
    saw_input_transcription: &AtomicBool,
) -> Result<bool, AsrError> {
    if let Some(reason) = extract_server_error(payload) {
        return Err(AsrError::ServerError(reason));
    }

    let summary = summarize_runtime_payload(payload);
    if diagnostics_enabled && !summary.is_empty() {
        log::info!("Gemini Live inbound summary: {}", summary);
    }

    if let Some(input_text) = payload
        .get("serverContent")
        .and_then(|v| v.get("inputTranscription"))
        .and_then(|v| v.get("text"))
        .and_then(Value::as_str)
    {
        saw_input_transcription.store(true, Ordering::SeqCst);
        if diagnostics_enabled {
            log::info!("Gemini Live inputTranscription: {}", input_text);
        }
        if let Some(combined) = transcript.push_partial(input_text) {
            on_event(AsrEvent {
                text: combined,
                is_final: false,
                prefetch: false,
                definite: false,
                confidence: None,
            });
        }
    }

    if let Some(output_text) = payload
        .get("serverContent")
        .and_then(|v| v.get("outputTranscription"))
        .and_then(|v| v.get("text"))
        .and_then(Value::as_str)
    {
        if diagnostics_enabled {
            log::info!("Gemini Live outputTranscription: {}", output_text);
        }
    }

    if payload.get("goAway").is_some() {
        let time_left = payload
            .get("goAway")
            .and_then(|v| v.get("timeLeft"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        log::info!("Gemini Live goAway received, timeLeft={}", time_left);
    }

    let turn_complete = payload
        .get("serverContent")
        .and_then(|v| v.get("turnComplete"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let generation_complete = payload
        .get("serverContent")
        .and_then(|v| v.get("generationComplete"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let interrupted = payload
        .get("serverContent")
        .and_then(|v| v.get("interrupted"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if diagnostics_enabled && (generation_complete || turn_complete || interrupted) {
        log::info!(
            "Gemini Live serverContent flags: generationComplete={}, turnComplete={}, interrupted={}",
            generation_complete,
            turn_complete,
            interrupted
        );
    }

    if turn_complete {
        if let Some(combined) = transcript.commit_turn() {
            on_event(AsrEvent {
                text: combined,
                is_final: true,
                prefetch: false,
                definite: true,
                confidence: None,
            });
        }
        log::debug!("Gemini Live turnComplete received");
    }

    if audio_stream_ended && turn_complete {
        log::info!(
            "Gemini Live stopping after audio end on turnComplete (generationComplete={}, turnComplete={})",
            generation_complete,
            turn_complete
        );
        Ok(true)
    } else {
        Ok(false)
    }
}

fn parse_json_message(msg: Message, stage: &str) -> Result<Value, AsrError> {
    match msg {
        Message::Text(text) => serde_json::from_str(&text).map_err(|e| {
            AsrError::ProtocolError(format!(
                "Invalid Gemini Live {stage} JSON text event: {e}"
            ))
        }),
        Message::Binary(bin) => {
            let text = String::from_utf8(bin).map_err(|e| {
                AsrError::ProtocolError(format!(
                    "Invalid Gemini Live {stage} UTF-8 binary event: {e}"
                ))
            })?;
            serde_json::from_str(&text).map_err(|e| {
                AsrError::ProtocolError(format!(
                    "Invalid Gemini Live {stage} JSON binary event: {e}"
                ))
            })
        }
        other => Err(AsrError::ProtocolError(format!(
            "Unsupported Gemini Live {stage} message: {:?}",
            other
        ))),
    }
}

fn summarize_runtime_payload(payload: &Value) -> String {
    let server_content = payload.get("serverContent");

    let has_input = server_content
        .and_then(|v| v.get("inputTranscription"))
        .is_some();
    let has_output = server_content
        .and_then(|v| v.get("outputTranscription"))
        .is_some();
    let generation_complete = server_content
        .and_then(|v| v.get("generationComplete"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let turn_complete = server_content
        .and_then(|v| v.get("turnComplete"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let interrupted = server_content
        .and_then(|v| v.get("interrupted"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let model_turn = server_content.and_then(|v| v.get("modelTurn"));
    let model_parts = model_turn
        .and_then(|v| v.get("parts"))
        .and_then(Value::as_array)
        .map(|parts| parts.len())
        .unwrap_or(0);
    let model_text_chars: usize = model_turn
        .and_then(|v| v.get("parts"))
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .map(str::chars)
                .map(Iterator::count)
                .sum()
        })
        .unwrap_or(0);
    let model_audio_parts = model_turn
        .and_then(|v| v.get("parts"))
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|part| part.get("inlineData").is_some() || part.get("inline_data").is_some())
                .count()
        })
        .unwrap_or(0);

    if !has_input
        && !has_output
        && !generation_complete
        && !turn_complete
        && !interrupted
        && model_parts == 0
        && model_text_chars == 0
        && model_audio_parts == 0
        && payload.get("goAway").is_none()
    {
        return String::new();
    }

    format!(
        "hasInputTx={}, hasOutputTx={}, generationComplete={}, turnComplete={}, interrupted={}, modelParts={}, modelTextChars={}, modelAudioParts={}",
        has_input,
        has_output,
        generation_complete,
        turn_complete,
        interrupted,
        model_parts,
        model_text_chars,
        model_audio_parts
    )
}

#[derive(Debug, Default)]
struct GeminiTranscriptAccumulator {
    accumulated_final: String,
    current_partial: String,
    last_emitted_partial: String,
    last_emitted_final: String,
}

impl GeminiTranscriptAccumulator {
    fn push_partial(&mut self, text: &str) -> Option<String> {
        let normalized = normalize_transcript_text(text);
        if normalized.is_empty() {
            return None;
        }

        self.current_partial = merge_transcript(&self.current_partial, &normalized);
        let combined = combine_segments(&self.accumulated_final, &self.current_partial);
        if combined == self.last_emitted_partial || combined == self.last_emitted_final {
            return None;
        }

        self.last_emitted_partial = combined.clone();
        Some(combined)
    }

    fn commit_turn(&mut self) -> Option<String> {
        if self.current_partial.is_empty() {
            return None;
        }

        let combined = combine_segments(&self.accumulated_final, &self.current_partial);
        self.accumulated_final = combined.clone();
        self.current_partial.clear();
        self.last_emitted_partial.clear();

        if combined == self.last_emitted_final {
            return None;
        }

        self.last_emitted_final = combined.clone();
        Some(combined)
    }
}

fn merge_transcript(existing: &str, incoming: &str) -> String {
    if existing.is_empty() {
        return incoming.to_string();
    }
    if incoming.is_empty() {
        return existing.to_string();
    }
    if incoming == existing {
        return existing.to_string();
    }
    if incoming.starts_with(existing) {
        return incoming.to_string();
    }
    if existing.starts_with(incoming) {
        return existing.to_string();
    }
    if incoming.ends_with(existing) {
        return incoming.to_string();
    }
    if existing.ends_with(incoming) {
        return existing.to_string();
    }
    combine_segments(existing, incoming)
}

fn combine_segments(accumulated: &str, current: &str) -> String {
    if accumulated.is_empty() {
        current.to_string()
    } else if current.is_empty() {
        accumulated.to_string()
    } else {
        format!("{} {}", accumulated.trim_end(), current.trim_start())
    }
}

fn normalize_transcript_text(text: &str) -> String {
    let mut normalized = text.trim().to_string();

    // Gemini Live sometimes inserts spaces between adjacent CJK characters.
    while let Some((start, end)) = find_cjk_space_span(&normalized) {
        normalized.replace_range(start..end, "");
    }

    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn find_cjk_space_span(text: &str) -> Option<(usize, usize)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for window in chars.windows(3) {
        let (_, prev) = window[0];
        let (space_idx, middle) = window[1];
        let (next_idx, next) = window[2];
        if middle == ' ' && is_cjk(prev) && is_cjk(next) {
            return Some((space_idx, next_idx));
        }
    }
    None
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2A6DF
            | 0x2A700..=0x2B73F
            | 0x2B740..=0x2B81F
            | 0x2B820..=0x2CEAF
            | 0x2CEB0..=0x2EBEF
    )
}

#[cfg(test)]
mod tests {
    use super::GeminiTranscriptAccumulator;

    #[test]
    fn accumulates_multiple_partials_within_same_turn() {
        let mut accumulator = GeminiTranscriptAccumulator::default();

        assert_eq!(accumulator.push_partial("前面一句").as_deref(), Some("前面一句"));
        assert_eq!(
            accumulator.push_partial("后面一句").as_deref(),
            Some("前面一句 后面一句")
        );
        assert_eq!(
            accumulator.commit_turn().as_deref(),
            Some("前面一句 后面一句")
        );
    }

    #[test]
    fn preserves_growing_partial_without_duplication() {
        let mut accumulator = GeminiTranscriptAccumulator::default();

        assert_eq!(accumulator.push_partial("前面一句").as_deref(), Some("前面一句"));
        assert_eq!(
            accumulator.push_partial("前面一句 后面一句").as_deref(),
            Some("前面一句后面一句")
        );
        assert_eq!(
            accumulator.commit_turn().as_deref(),
            Some("前面一句后面一句")
        );
    }
}
