//! DashScope `/inference` realtime ASR client shared by Fun-ASR and Qwen-Audio ASR.

use crate::network::connect_async;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc::Receiver;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message};

use super::audio_utils::{downmix_to_mono, resample_to_16k, resample_to_8k};
use super::config::AsrConfig;
use super::protocol::{AsrError, AsrEvent, AsrPhase};

pub struct FunAsrRealtimeClient {
    config: AsrConfig,
    provider: InferenceProvider,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InferenceProvider {
    FunAsr,
    QwenAudio,
}

impl FunAsrRealtimeClient {
    pub fn new(config: AsrConfig) -> Self {
        Self {
            config,
            provider: InferenceProvider::FunAsr,
        }
    }

    pub fn new_qwen(config: AsrConfig) -> Self {
        Self {
            config,
            provider: InferenceProvider::QwenAudio,
        }
    }

    fn model(&self) -> &str {
        match self.provider {
            InferenceProvider::FunAsr => &self.config.funasr_model,
            InferenceProvider::QwenAudio => &self.config.qwen_model,
        }
    }

    fn ws_url(&self) -> &str {
        match self.provider {
            InferenceProvider::FunAsr => &self.config.funasr_ws_url,
            InferenceProvider::QwenAudio => &self.config.qwen_ws_url,
        }
    }

    fn api_key(&self) -> &str {
        match self.provider {
            InferenceProvider::FunAsr => &self.config.funasr_api_key,
            InferenceProvider::QwenAudio => &self.config.qwen_api_key,
        }
    }

    fn language(&self) -> &str {
        match self.provider {
            InferenceProvider::FunAsr => &self.config.funasr_language,
            InferenceProvider::QwenAudio => &self.config.qwen_language,
        }
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
            return Err(AsrError::ConnectionFailed(
                "Invalid DashScope inference ASR configuration".to_string(),
            ));
        }

        let model = self.model().trim().to_string();
        let stream_rate = target_sample_rate(&model);
        let ws_url = if self.provider == InferenceProvider::QwenAudio {
            qwen_inference_ws_url(self.ws_url(), &self.config.qwen_workspace_id)?
        } else {
            inference_ws_url(self.ws_url())?
        };
        let mut req = ws_url
            .into_client_request()
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()).in_phase(AsrPhase::Connect))?;
        {
            let headers = req.headers_mut();
            let auth = format!("Bearer {}", self.api_key().trim());
            let auth_value = HeaderValue::from_str(&auth).map_err(|e| {
                AsrError::ConnectionFailed(format!("Invalid authorization header: {}", e))
                    .in_phase(AsrPhase::Connect)
            })?;
            headers.insert("Authorization", auth_value);
        }

        let (ws_stream, _) = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            result = connect_async(req) => result,
        }
        .map_err(|e| {
            AsrError::ConnectionFailed(format!(
                "DashScope inference WebSocket connect failed: {}",
                e
            ))
            .in_phase(AsrPhase::Connect)
        })?;
        let (mut ws_write, mut ws_read) = ws_stream.split();

        // 上下文增强：把 VoiceX 用户词典与最近识别历史组装成 input.context。
        // Qwen-Audio 3.0 Streaming 与两个 Fun-ASR 主版本支持该能力。
        // 不支持的模型若开了上下文，显式 warn 而非静默吞（遵守 AGENTS.md 防静默失败规则）。
        let context_payload = build_context_payload(
            &model,
            &self.config.hotwords,
            &history,
            self.config.enable_context,
        );
        match &context_payload {
            Some(ctx) => log::info!(
                "DashScope inference ASR: 上下文增强已启用 (model={}, context_chars={})",
                model,
                context_text_len(ctx),
            ),
            None => {
                if self.config.enable_context || !self.config.hotwords.is_empty() {
                    if !model_supports_context(&model) {
                        log::warn!(
                            "DashScope inference ASR: 上下文增强被忽略——模型 {} 不支持",
                            model
                        );
                    } else if context_text_for(
                        &self.config.hotwords,
                        &history,
                        self.config.enable_context,
                    )
                    .is_empty()
                    {
                        log::debug!(
                            "DashScope inference ASR: 上下文增强已开启但词表与历史均为空，跳过"
                        );
                    }
                }
            }
        }

        let task_id = uuid::Uuid::new_v4().to_string();
        let start_message = build_run_task_message(
            &task_id,
            stream_rate,
            &model,
            self.language(),
            context_payload.as_ref(),
            if self.provider == InferenceProvider::QwenAudio {
                Some(&self.config)
            } else {
                None
            },
        )?;
        ws_write
            .send(Message::Text(start_message.into()))
            .await
            .map_err(|e| {
                AsrError::ConnectionFailed(format!(
                    "DashScope inference run-task send failed: {}",
                    e
                ))
                .in_phase(AsrPhase::Handshake)
            })?;
        wait_for_task_started(&mut ws_read, &task_id).await?;

        let on_event = Arc::new(on_event);
        let reader_cancel = cancel.clone();
        let on_event_reader = on_event.clone();
        let reader_handle =
            tokio::spawn(async move { read_events(ws_read, reader_cancel, on_event_reader).await });

        write_audio_and_finish(
            &model,
            sample_rate,
            channels,
            stream_rate,
            audio_rx,
            cancel.clone(),
            &mut ws_write,
            &task_id,
        )
        .await?;

        reader_handle.await.map_err(|e| {
            AsrError::ConnectionFailed(format!(
                "DashScope inference reader task join failed: {}",
                e
            ))
            .in_phase(AsrPhase::Finalizing)
        })?
    }
}

pub fn qwen_uses_inference_protocol(model: &str) -> bool {
    model
        .trim()
        .starts_with("qwen-audio-3.0-asr-flash-streaming")
}

async fn wait_for_task_started(
    ws_read: &mut futures_util::stream::SplitStream<crate::network::WsStream>,
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
                let header = payload
                    .get("header")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        AsrError::ProtocolError("Fun-ASR handshake missing header".to_string())
                            .in_phase(AsrPhase::Handshake)
                    })?;
                match header
                    .get("event")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
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

    Err(
        AsrError::ConnectionFailed("Fun-ASR connection ended before task-started".to_string())
            .in_phase(AsrPhase::Handshake),
    )
}

async fn read_events(
    mut ws_read: futures_util::stream::SplitStream<crate::network::WsStream>,
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
                let header = payload
                    .get("header")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        AsrError::ProtocolError("Fun-ASR event missing header".to_string())
                            .in_phase(AsrPhase::Streaming)
                    })?;
                let event_name = header
                    .get("event")
                    .and_then(Value::as_str)
                    .unwrap_or_default();

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

    Err(
        AsrError::ConnectionFailed("Fun-ASR stream ended before task-finished".to_string())
            .in_phase(AsrPhase::Finalizing),
    )
}

async fn write_audio_and_finish(
    model: &str,
    sample_rate: u32,
    channels: u16,
    stream_rate: u32,
    mut audio_rx: Receiver<Vec<u8>>,
    cancel: tokio_util::sync::CancellationToken,
    ws_write: &mut futures_util::stream::SplitSink<crate::network::WsStream, Message>,
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
        ws_write
            .send(Message::Binary(pcm.into()))
            .await
            .map_err(|e| {
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
        "DashScope inference finish-task sent (model={}, stream_rate={})",
        model,
        stream_rate
    );
    Ok(())
}

fn build_run_task_message(
    task_id: &str,
    sample_rate: u32,
    model: &str,
    language: &str,
    context: Option<&Value>,
    qwen_config: Option<&AsrConfig>,
) -> Result<String, AsrError> {
    let mut parameters = json!({
        "format": "pcm",
        "sample_rate": sample_rate
    });

    let language_hints = language_hints(language, qwen_config.is_some());
    if !language_hints.is_empty() {
        parameters["language_hints"] = json!(language_hints);
    }

    if let Some(config) = qwen_config {
        parameters["semantic_punctuation_enabled"] =
            json!(config.qwen_semantic_punctuation_enabled);
        parameters["max_sentence_silence"] =
            json!(config.qwen_max_sentence_silence_ms.clamp(200, 6000));
        parameters["heartbeat"] = json!(config.qwen_heartbeat);

        let vocabulary = build_instant_vocabulary(&config.hotwords, config.qwen_hotword_weight);
        if !vocabulary.is_empty() {
            parameters["vocabulary"] = Value::Object(vocabulary);
            if !config.qwen_vocabulary_id.trim().is_empty() {
                log::warn!(
                    "Qwen-Audio ASR: inline dictionary hotwords override vocabulary_id={} for this request",
                    config.qwen_vocabulary_id.trim()
                );
            }
        } else if !config.qwen_vocabulary_id.trim().is_empty() {
            parameters["vocabulary_id"] = json!(config.qwen_vocabulary_id.trim());
        }
    }

    let mut input = serde_json::Map::new();
    if let Some(ctx) = context {
        input.insert("context".to_string(), ctx.clone());
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
            "model": model.trim(),
            "parameters": parameters,
            "input": Value::Object(input)
        }
    }))
    .map_err(|e| {
        AsrError::ProtocolError(format!(
            "Failed to serialize DashScope inference run-task: {}",
            e
        ))
    })
}

/// 支持 `input.context` 的 DashScope `/inference` 实时 ASR 模型。
/// 注意 fun-asr-realtime-2026-02-28 不支持，开了会被服务端静默忽略。
fn model_supports_context(model: &str) -> bool {
    let trimmed = model.trim();
    matches!(
        trimmed,
        "fun-asr-realtime" | "fun-asr-realtime-2025-11-07" | "qwen-audio-3.0-asr-flash-streaming"
    )
}

/// 组装上下文增强的 input.context 载荷。
///
/// 返回 None 的两种情况：
/// 1. 模型不支持上下文增强（调用方应据此 warn 用户）
/// 2. 词表与历史均为空（无需发 context）
///
/// 约束（官方）：
/// - 引擎最多保留最近 5 轮上下文；VoiceX 词表增强只用 1 轮 user 消息
/// - 每轮文本总长度不超过 400 字符，超出从末尾截断
/// - text 必须包含音频里待识别的原词，仅语义描述效果有限
fn build_context_payload(
    model: &str,
    hotwords: &[String],
    history: &[String],
    enable_context: bool,
) -> Option<Value> {
    if !model_supports_context(model) {
        return None;
    }

    let text = context_text_for(hotwords, history, enable_context);
    if text.is_empty() {
        return None;
    }

    // 官方约束：每轮 400 字符，超出从末尾截断
    const MAX_CHARS: usize = 400;
    let char_count = text.chars().count();
    let truncated: String = if char_count > MAX_CHARS {
        log::debug!(
            "Fun-ASR context text truncated from {} to {} chars",
            char_count,
            MAX_CHARS
        );
        text.chars().take(MAX_CHARS).collect()
    } else {
        text
    };

    Some(json!([
        {
            "role": "user",
            "content": [
                {
                    "type": "input_text",
                    "text": truncated
                }
            ]
        }
    ]))
}

/// 构造上下文文本。
///
/// 规则：
/// - 当 enable_context=true：拼接最近 history（最多 3 条）+ 用户词典词表
/// - 当 enable_context=false：仅用用户词典词表（词表增强场景，不需历史）
///
/// 词表部分：用 ", " 分隔，去重，最多取前 64 个（与 qwen_client corpus 思路一致）
fn context_text_for(hotwords: &[String], history: &[String], enable_context: bool) -> String {
    let mut parts: Vec<String> = Vec::new();

    // 仅在显式开启 enable_context 时才拼接历史；否则只用词典做词表增强
    if enable_context {
        for h in history.iter().rev().take(3) {
            let h = h.trim();
            if !h.is_empty() {
                parts.push(h.to_string());
            }
        }
    }

    // 词表去重并截断
    let mut seen = std::collections::HashSet::new();
    let mut hotword_lines: Vec<String> = hotwords
        .iter()
        .map(|w| w.trim().to_string())
        .filter(|w| !w.is_empty())
        .filter(|w| seen.insert(w.clone()))
        .take(64)
        .collect();
    if !hotword_lines.is_empty() {
        if parts.is_empty() {
            hotword_lines.insert(0, "参考词表：".to_string());
        }
        parts.push(hotword_lines.join(", "));
    }

    parts.join("\n")
}

/// 计算 context 载荷中所有 text 字段的总字符数（用于日志）
fn context_text_len(context: &Value) -> usize {
    context
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|msg| msg.get("content"))
                .filter_map(|content| content.as_array())
                .flatten()
                .filter_map(|item| item.get("text"))
                .filter_map(Value::as_str)
                .map(|s| s.chars().count())
                .sum()
        })
        .unwrap_or(0)
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

fn inference_ws_url(raw_url: &str) -> Result<String, AsrError> {
    let trimmed = raw_url.trim().trim_end_matches('/');
    let (scheme, rest) = trimmed.split_once("://").ok_or_else(|| {
        AsrError::ConnectionFailed(format!("Invalid DashScope WebSocket endpoint: {}", raw_url))
    })?;
    if !matches!(scheme, "ws" | "wss") {
        return Err(AsrError::ConnectionFailed(format!(
            "DashScope realtime endpoint must use ws:// or wss://: {}",
            raw_url
        )));
    }
    let host = rest.split(['/', '?']).next().unwrap_or_default().trim();
    if host.is_empty() {
        return Err(AsrError::ConnectionFailed(format!(
            "Invalid DashScope WebSocket endpoint: {}",
            raw_url
        )));
    }
    Ok(format!("{}://{}/api-ws/v1/inference", scheme, host))
}

fn qwen_inference_ws_url(raw_url: &str, workspace_id: &str) -> Result<String, AsrError> {
    let trimmed = raw_url.trim();
    let scheme = trimmed
        .split_once("://")
        .map(|(scheme, _)| scheme)
        .unwrap_or_default();
    if !matches!(scheme, "ws" | "wss") {
        return Err(AsrError::ConnectionFailed(format!(
            "Qwen-Audio realtime endpoint must use ws:// or wss://: {}",
            raw_url
        )));
    }
    let host = qwen_workspace_host(raw_url, workspace_id)?;
    Ok(format!("{}://{}/api-ws/v1/inference", scheme, host))
}

pub(crate) fn qwen_workspace_host(raw_url: &str, workspace_id: &str) -> Result<String, AsrError> {
    let without_scheme = raw_url
        .trim()
        .split_once("://")
        .map(|(_, rest)| rest)
        .ok_or_else(|| {
            AsrError::ConnectionFailed(format!("Invalid Qwen-Audio endpoint: {}", raw_url))
        })?;
    let host = without_scheme
        .split(['/', '?'])
        .next()
        .unwrap_or_default()
        .trim();
    if host.is_empty() {
        return Err(AsrError::ConnectionFailed(format!(
            "Invalid Qwen-Audio endpoint: {}",
            raw_url
        )));
    }
    if host.ends_with(".maas.aliyuncs.com") {
        return Ok(host.to_string());
    }

    let workspace_id = workspace_id.trim();
    if workspace_id.is_empty() {
        return Err(AsrError::ConnectionFailed(
            "Qwen-Audio 3.0 ASR requires an Alibaba Cloud Workspace ID or a full workspace-scoped endpoint".to_string(),
        ));
    }
    if !workspace_id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        return Err(AsrError::ConnectionFailed(
            "Alibaba Cloud Workspace ID contains invalid characters".to_string(),
        ));
    }

    let region_host = if host == "dashscope-intl.aliyuncs.com" || host.contains("ap-southeast-1") {
        "ap-southeast-1.maas.aliyuncs.com"
    } else {
        "cn-beijing.maas.aliyuncs.com"
    };
    Ok(format!("{}.{}", workspace_id, region_host))
}

pub(crate) fn language_hints(raw: &str, supports_multiple: bool) -> Vec<String> {
    let limit = if supports_multiple { 4 } else { 1 };
    raw.split([',', ' ', '\n', '\t'])
        .map(str::trim)
        .filter(|part| !part.is_empty() && *part != "auto")
        .take(limit)
        .map(str::to_string)
        .collect()
}

pub(crate) fn build_instant_vocabulary(
    hotwords: &[String],
    configured_weight: u32,
) -> serde_json::Map<String, Value> {
    let weight = match configured_weight {
        1..=5 | 50 => configured_weight,
        _ => 4,
    };
    let max_words = if weight == 50 { 50 } else { 2000 };
    let mut vocabulary = serde_json::Map::new();
    let mut skipped = 0usize;

    for word in hotwords {
        let word = word.trim();
        if word.is_empty() || vocabulary.contains_key(word) {
            continue;
        }
        if vocabulary.len() >= max_words {
            skipped += 1;
            continue;
        }
        let valid = if word.is_ascii() {
            word.split_whitespace().count() <= 7
        } else {
            word.chars().count() <= 15
        };
        if !valid {
            skipped += 1;
            continue;
        }
        vocabulary.insert(word.to_string(), json!(weight));
    }

    if skipped > 0 {
        log::warn!(
            "Qwen-Audio ASR: skipped {} dictionary entries that exceed hotword limits",
            skipped
        );
    }
    vocabulary
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
    use super::{
        build_context_payload, build_instant_vocabulary, build_run_task_message, context_text_for,
        context_text_len, inference_ws_url, language_hints, model_supports_context,
        qwen_inference_ws_url, qwen_uses_inference_protocol, qwen_workspace_host,
        target_sample_rate,
    };
    use crate::asr::AsrConfig;

    #[test]
    fn funasr_8k_model_maps_to_8k() {
        assert_eq!(target_sample_rate("fun-asr-flash-8k-realtime"), 8_000);
        assert_eq!(target_sample_rate("fun-asr-realtime"), 16_000);
    }

    #[test]
    fn funasr_language_hint_uses_first_non_empty_value() {
        assert_eq!(language_hints("zh, en", false), vec!["zh"]);
        assert!(language_hints("  ", false).is_empty());
    }

    #[test]
    fn run_task_message_contains_expected_fields() {
        let mut config = AsrConfig::default();
        config.funasr_model = "fun-asr-realtime".to_string();
        config.funasr_language = "zh, en".to_string();
        let msg = build_run_task_message(
            "task-1",
            16_000,
            &config.funasr_model,
            &config.funasr_language,
            None,
            None,
        )
        .unwrap();
        assert!(msg.contains("\"action\":\"run-task\""));
        assert!(msg.contains("\"model\":\"fun-asr-realtime\""));
        assert!(msg.contains("\"sample_rate\":16000"));
        assert!(msg.contains("\"language_hints\":[\"zh\"]"));
        // 无 context 时 input 应为空对象
        assert!(msg.contains("\"input\":{}"));
    }

    #[test]
    fn run_task_message_includes_context_when_provided() {
        let mut config = AsrConfig::default();
        config.funasr_model = "fun-asr-realtime".to_string();
        let context = super::build_context_payload(
            "fun-asr-realtime",
            &["VoiceX".to_string(), "热词".to_string()],
            &[],
            false,
        )
        .expect("context payload should be built for supported model");
        let msg = build_run_task_message(
            "task-2",
            16_000,
            &config.funasr_model,
            &config.funasr_language,
            Some(&context),
            None,
        )
        .unwrap();
        assert!(msg.contains("\"context\""));
        assert!(msg.contains("\"input_text\""));
        assert!(msg.contains("VoiceX"));
        assert!(msg.contains("热词"));
    }

    #[test]
    fn model_supports_context_matches_official_models() {
        assert!(model_supports_context("fun-asr-realtime"));
        assert!(model_supports_context("fun-asr-realtime-2025-11-07"));
        assert!(model_supports_context("  fun-asr-realtime  ")); // 容忍空白
        assert!(model_supports_context("qwen-audio-3.0-asr-flash-streaming"));

        // 最新快照不支持——这是最容易踩的坑
        assert!(!model_supports_context("fun-asr-realtime-2026-02-28"));
        // 8k 系列不支持
        assert!(!model_supports_context("fun-asr-flash-8k-realtime"));
        assert!(!model_supports_context(
            "fun-asr-flash-8k-realtime-2026-01-28"
        ));
        // 非实时模式不支持（同步/异步都不支持上下文增强之外的模型）
        assert!(!model_supports_context("fun-asr"));
        assert!(!model_supports_context("fun-asr-flash-2026-06-15"));
    }

    #[test]
    fn qwen_audio_run_task_includes_protocol_features() {
        let mut config = AsrConfig::default();
        config.qwen_model = "qwen-audio-3.0-asr-flash-streaming".to_string();
        config.qwen_language = "zh, en, ja, ko, fr".to_string();
        config.hotwords = vec!["VoiceX".to_string(), "连续刚构桥".to_string()];
        config.qwen_hotword_weight = 50;
        config.qwen_semantic_punctuation_enabled = true;
        config.qwen_max_sentence_silence_ms = 900;
        config.qwen_heartbeat = true;

        let msg = build_run_task_message(
            "task-qwen",
            16_000,
            &config.qwen_model,
            &config.qwen_language,
            None,
            Some(&config),
        )
        .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&msg).unwrap();
        let parameters = &payload["payload"]["parameters"];
        assert_eq!(
            parameters["language_hints"],
            serde_json::json!(["zh", "en", "ja", "ko"])
        );
        assert_eq!(parameters["vocabulary"]["VoiceX"], 50);
        assert_eq!(parameters["vocabulary"]["连续刚构桥"], 50);
        assert_eq!(parameters["semantic_punctuation_enabled"], true);
        assert_eq!(parameters["max_sentence_silence"], 900);
        assert_eq!(parameters["heartbeat"], true);
    }

    #[test]
    fn qwen_audio_helpers_validate_endpoint_and_hotwords() {
        assert!(qwen_uses_inference_protocol(
            "qwen-audio-3.0-asr-flash-streaming"
        ));
        assert_eq!(
            inference_ws_url("wss://dashscope.aliyuncs.com/api-ws/v1/realtime").unwrap(),
            "wss://dashscope.aliyuncs.com/api-ws/v1/inference"
        );
        assert_eq!(language_hints("zh, en, ja, ko, fr", true).len(), 4);

        let vocabulary = build_instant_vocabulary(
            &[
                "VoiceX".to_string(),
                "VoiceX".to_string(),
                "one two three four five six seven eight".to_string(),
            ],
            4,
        );
        assert_eq!(vocabulary.len(), 1);
        assert_eq!(vocabulary["VoiceX"], 4);

        let super_hotwords: Vec<String> = (0..60).map(|index| format!("word{}", index)).collect();
        assert_eq!(build_instant_vocabulary(&super_hotwords, 50).len(), 50);
    }

    #[test]
    fn qwen_audio_workspace_endpoint_is_explicit() {
        assert_eq!(
            qwen_inference_ws_url(
                "wss://dashscope.aliyuncs.com/api-ws/v1/realtime",
                "workspace-123"
            )
            .unwrap(),
            "wss://workspace-123.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference"
        );
        assert_eq!(
            qwen_workspace_host(
                "wss://existing.ap-southeast-1.maas.aliyuncs.com/api-ws/v1/inference",
                ""
            )
            .unwrap(),
            "existing.ap-southeast-1.maas.aliyuncs.com"
        );
        assert!(
            qwen_inference_ws_url("wss://dashscope.aliyuncs.com/api-ws/v1/inference", "").is_err()
        );
    }

    #[test]
    fn build_context_payload_returns_none_for_unsupported_model() {
        // 最新快照不支持上下文增强——必须返回 None，调用方据此 warn 用户
        let payload = build_context_payload(
            "fun-asr-realtime-2026-02-28",
            &["VoiceX".to_string()],
            &[],
            false,
        );
        assert!(payload.is_none());
    }

    #[test]
    fn build_context_payload_returns_none_when_no_hotwords_and_no_history() {
        let payload = build_context_payload("fun-asr-realtime", &[], &[], false);
        assert!(payload.is_none());
    }

    #[test]
    fn build_context_payload_includes_hotwords_for_supported_model() {
        let payload = build_context_payload(
            "fun-asr-realtime",
            &["VoiceX".to_string(), "Kubernetes".to_string()],
            &[],
            false,
        )
        .expect("supported model with hotwords should produce payload");
        let text = payload[0]["content"][0]["text"]
            .as_str()
            .expect("text field should exist");
        assert!(text.contains("VoiceX"));
        assert!(text.contains("Kubernetes"));
        assert!(text.contains("参考词表")); // 词表增强前缀
    }

    #[test]
    fn build_context_payload_includes_history_when_enable_context() {
        let payload = build_context_payload(
            "fun-asr-realtime",
            &["VoiceX".to_string()],
            &["上一轮用户说了什么".to_string()],
            true,
        )
        .expect("supported model should produce payload");
        let text = payload[0]["content"][0]["text"]
            .as_str()
            .expect("text field should exist");
        // enable_context=true 时历史应在词表之前
        assert!(text.contains("上一轮用户说了什么"));
        assert!(text.contains("VoiceX"));
    }

    #[test]
    fn build_context_payload_omits_history_when_enable_context_false() {
        // enable_context=false 时只用词表，不掺历史——词表增强场景
        let payload = build_context_payload(
            "fun-asr-realtime",
            &["VoiceX".to_string()],
            &["不应该出现的上一轮历史".to_string()],
            false,
        )
        .expect("supported model with hotwords should produce payload");
        let text = payload[0]["content"][0]["text"]
            .as_str()
            .expect("text field should exist");
        assert!(!text.contains("不应该出现的上一轮历史"));
        assert!(text.contains("VoiceX"));
    }

    #[test]
    fn build_context_payload_truncates_to_400_chars() {
        // 构造超长词表，验证截断到 400 字符
        let long_word = "a".repeat(500);
        let hotwords = vec![long_word];
        let payload = build_context_payload("fun-asr-realtime", &hotwords, &[], false)
            .expect("supported model should produce payload");
        let text = payload[0]["content"][0]["text"]
            .as_str()
            .expect("text field should exist");
        // 官方约束：每轮 400 字符
        assert!(text.chars().count() <= 400);
    }

    #[test]
    fn context_text_for_deduplicates_hotwords() {
        let hotwords = vec![
            "VoiceX".to_string(),
            "VoiceX".to_string(),
            "Qwen".to_string(),
        ];
        let text = context_text_for(&hotwords, &[], false);
        assert!(text.contains("VoiceX"));
        assert!(text.contains("Qwen"));
        // 去重后 "VoiceX" 只出现一次
        assert_eq!(text.matches("VoiceX").count(), 1);
    }

    #[test]
    fn context_text_for_limits_to_64_hotwords() {
        let hotwords: Vec<String> = (0..100).map(|i| format!("word{}", i)).collect();
        let text = context_text_for(&hotwords, &[], false);
        // 词表前缀 + 前 64 个词
        for i in 0..64 {
            assert!(text.contains(&format!("word{}", i)));
        }
        assert!(!text.contains("word64"));
    }

    #[test]
    fn context_text_len_counts_all_text_fields() {
        let payload = build_context_payload(
            "fun-asr-realtime",
            &["VoiceX".to_string(), "测试".to_string()],
            &[],
            false,
        )
        .unwrap();
        let len = context_text_len(&payload);
        // "参考词表：VoiceX, 测试" = 4 + 6 + 2 + 2 = 14 字符（参考词表：6 + VoiceX 6 + ", " 2 + 测试 2）
        assert!(len > 0);
        assert!(len < 400);
    }
}
