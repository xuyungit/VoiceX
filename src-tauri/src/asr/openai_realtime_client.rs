use crate::network::connect_async;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc::Receiver;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message};

use super::audio_utils::{downmix_to_mono, resample_to_24k};
use super::config::AsrConfig;
use super::openai_client::build_transcription_prompt;
use super::protocol::{AsrError, AsrEvent};

const OPENAI_SESSION_READY_WAIT_MS: u64 = 1500;
const OPENAI_COMPLETION_WAIT_MS: u64 = 10_000;
const OPENAI_IDLE_AFTER_COMMIT_MS: u64 = 1200;
const OPENAI_MIN_COMMIT_AUDIO_BYTES: usize = 4_800;

pub struct OpenAIRealtimeClient {
    config: AsrConfig,
}

impl OpenAIRealtimeClient {
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
                "Invalid OpenAI realtime ASR configuration".to_string(),
            ));
        }

        let stream_rate = 24_000u32;
        let diagnostics_enabled = self.config.enable_diagnostics;
        let ws_url = build_realtime_ws_url(&self.config.openai_asr_base_url);

        log::info!(
            "OpenAI Realtime connecting (capture {} Hz -> stream {} Hz, {} ch, transcription_model={})",
            sample_rate,
            stream_rate,
            channels,
            self.config.openai_asr_model,
        );

        let mut req = ws_url
            .into_client_request()
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;
        {
            let headers = req.headers_mut();
            headers.insert(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.config.openai_asr_api_key))
                    .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?,
            );
        }

        let (ws_stream, _) = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            result = connect_async(req) => result,
        }
        .map_err(|e| AsrError::ConnectionFailed(format!("OpenAI Realtime connect: {e}")))?;
        let (mut ws_write, mut ws_read) = ws_stream.split();

        // The GA API configures the session over the socket rather than at
        // session-creation time, so this must land before any audio is sent.
        let session_update = build_session_update_event(&self.config);
        if diagnostics_enabled {
            log::info!("OpenAI Realtime session.update payload: {}", session_update);
        }
        ws_write
            .send(Message::Text(session_update.to_string()))
            .await
            .map_err(|e| {
                AsrError::ConnectionFailed(format!(
                    "Failed to send OpenAI Realtime session.update: {e}"
                ))
            })?;

        loop {
            let msg = tokio::select! {
                _ = cancel.cancelled() => None,
                v = tokio::time::timeout(Duration::from_millis(OPENAI_SESSION_READY_WAIT_MS), ws_read.next()) => {
                    match v {
                        Ok(msg) => msg,
                        Err(_) => {
                            log::warn!(
                                "OpenAI Realtime session ready event not received within {} ms; proceeding with audio stream",
                                OPENAI_SESSION_READY_WAIT_MS
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
                        log::info!("OpenAI Realtime setup inbound: {}", payload);
                    }

                    if let Some(reason) = extract_server_error(&payload) {
                        return Err(AsrError::ServerError(reason));
                    }

                    let event_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
                    if event_type == "session.created" || event_type == "session.updated" {
                        log::info!("OpenAI Realtime {} received", event_type);
                        break;
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    return Err(AsrError::ConnectionFailed(format!(
                        "OpenAI Realtime closed before session was ready: {:?}",
                        frame
                    )));
                }
                Some(Ok(other)) => {
                    if diagnostics_enabled {
                        log::info!("OpenAI Realtime setup non-text frame: {:?}", other);
                    }
                }
                Some(Err(e)) => {
                    return Err(AsrError::ConnectionFailed(format!(
                        "OpenAI Realtime setup read failed: {e}"
                    )));
                }
                None => {
                    return Err(AsrError::ConnectionFailed(
                        "OpenAI Realtime closed before session was ready".to_string(),
                    ));
                }
            }
        }

        let on_event = Arc::new(on_event);
        let on_event_reader = on_event.clone();
        let cancel_reader = cancel.clone();
        let stream_end_started_at = Arc::new(Mutex::new(None::<Instant>));
        let stream_end_started_at_reader = stream_end_started_at.clone();
        let pending_audio_bytes = Arc::new(Mutex::new(0usize));
        let pending_audio_bytes_reader = pending_audio_bytes.clone();
        let reader_handle = tokio::spawn(async move {
            let mut transcript = OpenAITranscriptAccumulator::default();
            let mut last_relevant_event_at = Instant::now();

            loop {
                let msg = tokio::select! {
                    _ = cancel_reader.cancelled() => None,
                    v = tokio::time::timeout(Duration::from_millis(200), ws_read.next()) => {
                        match v {
                            Ok(msg) => msg,
                            Err(_) => {
                                let stream_end_started_at = stream_end_started_at_reader
                                    .lock()
                                    .ok()
                                    .and_then(|guard| guard.as_ref().cloned());
                                if let Some(stream_end_started_at) = stream_end_started_at {
                                    let idle_since = if last_relevant_event_at > stream_end_started_at {
                                        last_relevant_event_at
                                    } else {
                                        stream_end_started_at
                                    };
                                    if idle_since.elapsed()
                                        >= Duration::from_millis(OPENAI_IDLE_AFTER_COMMIT_MS)
                                    {
                                        break;
                                    }
                                }
                                continue;
                            }
                        }
                    },
                };

                let Some(msg) = msg else {
                    break;
                };

                match msg {
                    Ok(Message::Text(text)) => {
                        let payload = parse_json_message(Message::Text(text), "runtime")?;
                        let stop = handle_runtime_payload(
                            &payload,
                            &mut transcript,
                            &on_event_reader,
                            diagnostics_enabled,
                            &stream_end_started_at_reader,
                            &pending_audio_bytes_reader,
                            &mut last_relevant_event_at,
                        )?;
                        if stop {
                            break;
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        log::debug!("OpenAI Realtime closed by server: {:?}", frame);
                        break;
                    }
                    Ok(other) => {
                        log::debug!("OpenAI Realtime received non-text frame: {:?}", other);
                    }
                    Err(e) => {
                        return Err(AsrError::ConnectionFailed(format!(
                            "OpenAI Realtime read failed: {e}"
                        )));
                    }
                }
            }

            if let Some(text) = transcript.finalize_if_needed() {
                on_event_reader(AsrEvent {
                    text,
                    is_final: true,
                    prefetch: false,
                    definite: true,
                    confidence: None,
                });
            }

            Ok::<(), AsrError>(())
        });

        let resample_needed = sample_rate != stream_rate;
        if resample_needed {
            log::debug!(
                "OpenAI Realtime resampling input {} Hz -> {} Hz",
                sample_rate,
                stream_rate
            );
        }

        let mut audio_rx = audio_rx;
        let mut audio_chunk_count: u64 = 0;
        let mut audio_byte_count: u64 = 0;
        while let Some(chunk) = tokio::select! {
            _ = cancel.cancelled() => None,
            v = audio_rx.recv() => v,
        } {
            let pcm = if resample_needed {
                resample_to_24k(&chunk, sample_rate)
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
            audio_chunk_count = audio_chunk_count.saturating_add(1);
            audio_byte_count = audio_byte_count.saturating_add(mono.len() as u64);
            if let Ok(mut guard) = pending_audio_bytes.lock() {
                *guard = guard.saturating_add(mono.len());
            }

            let audio_message = json!({
                "type": "input_audio_buffer.append",
                "audio": STANDARD.encode(mono),
            });
            ws_write
                .send(Message::Text(audio_message.to_string()))
                .await
                .map_err(|e| {
                    AsrError::ConnectionFailed(format!(
                        "Failed to send OpenAI Realtime audio chunk: {e}"
                    ))
                })?;
        }

        if cancel.is_cancelled() {
            let _ = ws_write.close().await;
            let _ = tokio::time::timeout(Duration::from_millis(1000), reader_handle).await;
            return Ok(());
        }

        if let Ok(mut guard) = stream_end_started_at.lock() {
            *guard = Some(Instant::now());
        }
        let pending_bytes = pending_audio_bytes
            .lock()
            .ok()
            .map(|guard| *guard)
            .unwrap_or_default();

        if pending_bytes >= OPENAI_MIN_COMMIT_AUDIO_BYTES {
            ws_write
                .send(Message::Text(
                    json!({
                        "type": "input_audio_buffer.commit",
                    })
                    .to_string(),
                ))
                .await
                .map_err(|e| {
                    AsrError::ConnectionFailed(format!(
                        "Failed to send OpenAI Realtime input_audio_buffer.commit: {e}"
                    ))
                })?;
            log::info!(
                "OpenAI Realtime input_audio_buffer.commit sent (chunks={}, bytes={}, pending_bytes={})",
                audio_chunk_count,
                audio_byte_count,
                pending_bytes
            );
        } else {
            log::info!(
                "OpenAI Realtime final commit skipped: pending buffer too small (pending_bytes={}, min_bytes={})",
                pending_bytes,
                OPENAI_MIN_COMMIT_AUDIO_BYTES
            );
        }

        match tokio::time::timeout(
            Duration::from_millis(OPENAI_COMPLETION_WAIT_MS),
            reader_handle,
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => Err(AsrError::ConnectionFailed(format!(
                "OpenAI Realtime reader task failed: {e}"
            ))),
            Err(_) => {
                log::warn!(
                    "OpenAI Realtime timed out waiting for stream completion ({} ms)",
                    OPENAI_COMPLETION_WAIT_MS
                );
                let _ = ws_write.close().await;
                Ok(())
            }
        }
    }
}

/// Build the GA realtime WebSocket URL.
///
/// Transcription sessions are selected with `?intent=transcription`; the
/// `?model=` parameter used by speech-to-speech sessions is rejected here
/// because a transcription model is not a valid session model.
fn build_realtime_ws_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');

    let base = if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if trimmed.starts_with("wss://") || trimmed.starts_with("ws://") {
        trimmed.to_string()
    } else {
        format!("wss://{trimmed}")
    };

    format!("{base}/realtime?intent=transcription")
}

fn build_session_update_event(config: &AsrConfig) -> Value {
    let model = config.openai_asr_model.trim();
    let mut transcription = json!({ "model": model });

    if super::openai_client::model_supports_keywords(model) {
        let languages = super::openai_client::parse_language_list(&config.openai_asr_language);
        if !languages.is_empty() {
            transcription["languages"] = json!(languages);
        }
        let keywords = super::openai_client::cleaned_openai_keywords(&config.hotwords);
        if !keywords.is_empty() {
            transcription["keywords"] = json!(keywords);
        }
        let prompt = config.openai_asr_prompt.trim();
        if !prompt.is_empty() {
            transcription["prompt"] = json!(prompt);
        }
        let delay = config.openai_asr_delay.trim();
        if !delay.is_empty() && super::models::supports_delay("openai", model) {
            transcription["delay"] = json!(delay);
        }
    } else {
        // Legacy transcription models have no keyword channel, so the dictionary
        // has to ride along inside the prompt.
        if let Some(language) =
            super::openai_client::parse_language_list(&config.openai_asr_language).first()
        {
            transcription["language"] = json!(language);
        }
        let prompt = build_transcription_prompt(config.openai_asr_prompt.trim(), &config.hotwords);
        if !prompt.is_empty() {
            transcription["prompt"] = json!(prompt);
        }
    }

    // `null` means "no server-side VAD": VoiceX is push-to-talk and commits the
    // buffer itself when the hotkey is released. It is also required rather than
    // merely preferred — gpt-live-transcribe rejects any turn_detection block.
    let mut session = json!({
        "type": "transcription",
        "audio": {
            "input": {
                "format": { "type": "audio/pcm", "rate": 24_000 },
                "transcription": transcription,
                "turn_detection": Value::Null,
            }
        }
    });

    if config.enable_diagnostics {
        session["include"] = json!(["item.input_audio_transcription.logprobs"]);
    }

    json!({ "type": "session.update", "session": session })
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
    transcript: &mut OpenAITranscriptAccumulator,
    on_event: &Arc<impl Fn(AsrEvent) + Send + Sync + 'static>,
    diagnostics_enabled: bool,
    stream_end_started_at: &Arc<Mutex<Option<Instant>>>,
    pending_audio_bytes: &Arc<Mutex<usize>>,
    last_relevant_event_at: &mut Instant,
) -> Result<bool, AsrError> {
    if let Some(reason) = extract_server_error(payload) {
        let finalizing = stream_end_started_at
            .lock()
            .ok()
            .and_then(|guard| *guard)
            .is_some();
        let benign_empty_commit =
            finalizing && reason.contains("Error committing input audio buffer: buffer too small");
        if benign_empty_commit {
            if let Ok(mut guard) = pending_audio_bytes.lock() {
                *guard = 0;
            }
            *last_relevant_event_at = Instant::now();
            log::info!(
                "OpenAI Realtime ignoring benign final commit race: {}",
                reason
            );
            return Ok(false);
        }
        return Err(AsrError::ServerError(reason));
    }

    let event_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
    if !event_type.is_empty() {
        log::debug!("OpenAI Realtime inbound event: {}", event_type);
    }

    match event_type {
        "input_audio_buffer.committed" => {
            *last_relevant_event_at = Instant::now();
            if let Ok(mut guard) = pending_audio_bytes.lock() {
                *guard = 0;
            }
            let item_id = payload
                .get("item_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let previous_item_id = payload
                .get("previous_item_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            if let Some(item_id) = item_id {
                log::info!(
                    "OpenAI Realtime input_audio_buffer.committed item_id={} previous_item_id={}",
                    item_id,
                    previous_item_id.as_deref().unwrap_or("none")
                );
                transcript.register_commit(item_id, previous_item_id);
            }
        }
        "conversation.item.input_audio_transcription.delta" => {
            *last_relevant_event_at = Instant::now();
            let item_id = payload
                .get("item_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AsrError::ProtocolError(
                        "OpenAI Realtime delta event missing item_id".to_string(),
                    )
                })?;
            let delta = payload.get("delta").and_then(Value::as_str).unwrap_or("");
            if diagnostics_enabled {
                log::info!("OpenAI Realtime delta item_id={} text={}", item_id, delta);
            }
            if let Some(text) = transcript.push_delta(item_id.to_string(), delta) {
                on_event(AsrEvent {
                    text,
                    is_final: false,
                    prefetch: false,
                    definite: false,
                    confidence: None,
                });
            }
        }
        "conversation.item.input_audio_transcription.completed" => {
            *last_relevant_event_at = Instant::now();
            let item_id = payload
                .get("item_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AsrError::ProtocolError(
                        "OpenAI Realtime completed event missing item_id".to_string(),
                    )
                })?;
            let completed = payload
                .get("transcript")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            log::info!(
                "OpenAI Realtime completed item_id={} len={}",
                item_id,
                completed.chars().count()
            );
            if let Some(text) = transcript.complete(item_id.to_string(), completed) {
                on_event(AsrEvent {
                    text,
                    is_final: true,
                    prefetch: false,
                    definite: true,
                    confidence: None,
                });
            }
        }
        "error" => {
            if diagnostics_enabled {
                log::info!("OpenAI Realtime error payload: {}", payload);
            }
        }
        _ => {}
    }

    Ok(false)
}

fn parse_json_message(msg: Message, stage: &str) -> Result<Value, AsrError> {
    match msg {
        Message::Text(text) => serde_json::from_str(&text).map_err(|e| {
            AsrError::ProtocolError(format!(
                "Invalid OpenAI Realtime {stage} JSON text event: {e}"
            ))
        }),
        Message::Binary(bin) => {
            let text = String::from_utf8(bin).map_err(|e| {
                AsrError::ProtocolError(format!(
                    "Invalid OpenAI Realtime {stage} UTF-8 binary event: {e}"
                ))
            })?;
            serde_json::from_str(&text).map_err(|e| {
                AsrError::ProtocolError(format!(
                    "Invalid OpenAI Realtime {stage} JSON binary event: {e}"
                ))
            })
        }
        other => Err(AsrError::ProtocolError(format!(
            "Unsupported OpenAI Realtime {stage} message: {:?}",
            other
        ))),
    }
}

#[derive(Debug, Default)]
struct OpenAITranscriptAccumulator {
    order: Vec<String>,
    finals: HashMap<String, String>,
    partials: HashMap<String, String>,
    last_emitted_partial: String,
    last_emitted_final: String,
}

impl OpenAITranscriptAccumulator {
    fn register_commit(&mut self, item_id: String, previous_item_id: Option<String>) {
        self.ensure_item_known(&item_id);

        if let Some(prev) = previous_item_id {
            if prev == item_id {
                return;
            }
            self.order.retain(|id| id != &item_id);
            if let Some(idx) = self.order.iter().position(|id| id == &prev) {
                self.order.insert(idx + 1, item_id);
                return;
            }
        }

        if !self.order.iter().any(|id| id == &item_id) {
            self.order.push(item_id);
        }
    }

    fn push_delta(&mut self, item_id: String, delta: &str) -> Option<String> {
        if delta.is_empty() {
            return None;
        }

        self.ensure_item_known(&item_id);
        let entry = self.partials.entry(item_id).or_default();
        merge_delta_text(entry, delta);
        let combined = self.combine(false);
        if combined.is_empty()
            || combined == self.last_emitted_partial
            || combined == self.last_emitted_final
        {
            return None;
        }
        self.last_emitted_partial = combined.clone();
        Some(combined)
    }

    fn complete(&mut self, item_id: String, transcript: &str) -> Option<String> {
        self.ensure_item_known(&item_id);
        self.partials.remove(&item_id);
        self.finals.insert(item_id, transcript.to_string());
        let combined = self.combine(true);
        if combined.is_empty() || combined == self.last_emitted_final {
            return None;
        }
        self.last_emitted_final = combined.clone();
        self.last_emitted_partial.clear();
        Some(combined)
    }

    fn finalize_if_needed(&mut self) -> Option<String> {
        let combined = self.combine(false);
        if combined.is_empty() || combined == self.last_emitted_final {
            return None;
        }
        self.last_emitted_final = combined.clone();
        Some(combined)
    }

    fn ensure_item_known(&mut self, item_id: &str) {
        if !self.order.iter().any(|id| id == item_id) {
            self.order.push(item_id.to_string());
        }
    }

    fn combine(&self, finals_only: bool) -> String {
        self.order
            .iter()
            .filter_map(|item_id| {
                if let Some(final_text) = self.finals.get(item_id) {
                    let trimmed = final_text.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
                if !finals_only {
                    if let Some(partial_text) = self.partials.get(item_id) {
                        let trimmed = partial_text.trim();
                        if !trimmed.is_empty() {
                            return Some(trimmed.to_string());
                        }
                    }
                }
                None
            })
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string()
    }
}

fn merge_delta_text(existing: &mut String, incoming: &str) {
    if existing.is_empty() {
        *existing = incoming.to_string();
        return;
    }
    if incoming.starts_with(existing.as_str()) {
        *existing = incoming.to_string();
        return;
    }
    if existing.ends_with(incoming) {
        return;
    }
    existing.push_str(incoming);
}

#[cfg(test)]
mod tests {
    use super::{build_realtime_ws_url, build_session_update_event};
    use crate::asr::config::AsrConfig;

    fn config(model: &str) -> AsrConfig {
        AsrConfig {
            openai_asr_model: model.to_string(),
            openai_asr_language: "zh, en".to_string(),
            openai_asr_prompt: "A developer dictating a note.".to_string(),
            openai_asr_delay: "low".to_string(),
            hotwords: vec!["Kysely".to_string(), "Drizzle".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn ws_url_selects_the_transcription_intent() {
        assert_eq!(
            build_realtime_ws_url("https://api.openai.com/v1"),
            "wss://api.openai.com/v1/realtime?intent=transcription"
        );
        assert_eq!(
            build_realtime_ws_url("http://localhost:8080/v1/"),
            "ws://localhost:8080/v1/realtime?intent=transcription"
        );
    }

    #[test]
    fn session_update_uses_the_ga_nesting() {
        let event = build_session_update_event(&config("gpt-live-transcribe"));
        assert_eq!(event["type"], "session.update");
        assert_eq!(event["session"]["type"], "transcription");

        let input = &event["session"]["audio"]["input"];
        assert_eq!(input["format"]["type"], "audio/pcm");
        assert_eq!(input["format"]["rate"], 24_000);

        let tr = &input["transcription"];
        assert_eq!(tr["model"], "gpt-live-transcribe");
        assert_eq!(tr["languages"], serde_json::json!(["zh", "en"]));
        assert_eq!(tr["keywords"], serde_json::json!(["Kysely", "Drizzle"]));
        assert_eq!(tr["delay"], "low");
        assert_eq!(tr["prompt"], "A developer dictating a note.");
        // The dictionary must not leak back into the prompt on modern models.
        assert!(!tr["prompt"].as_str().unwrap().contains("Kysely"));
    }

    #[test]
    fn legacy_models_fall_back_to_prompt_stuffing() {
        let event = build_session_update_event(&config("gpt-4o-transcribe"));
        let tr = &event["session"]["audio"]["input"]["transcription"];
        assert!(tr.get("keywords").is_none());
        assert!(tr.get("languages").is_none());
        assert!(tr.get("delay").is_none());
        assert_eq!(tr["language"], "zh");
        assert!(tr["prompt"].as_str().unwrap().contains("Kysely"));
    }

    #[test]
    fn dump_live_payload_for_manual_replay() {
        // Printed with --nocapture so the exact wire payload can be replayed
        // against the real API during protocol changes.
        println!(
            "{}",
            serde_json::to_string(&build_session_update_event(&config("gpt-live-transcribe")))
                .unwrap()
        );
    }
    #[test]
    fn switching_to_committed_model_does_not_send_saved_live_delay() {
        let mut settings = config("gpt-transcribe");
        settings.openai_asr_delay = "low".into();
        let event = build_session_update_event(&settings);
        let encoded = serde_json::to_string(&event).unwrap();
        assert!(!encoded.contains("\"delay\""));
    }
}
