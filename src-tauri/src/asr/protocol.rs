//! ASR protocol helpers for the Volcengine streaming API.

use std::io::Read;

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};

use super::config::AsrProviderType;

/// ASR recognition event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsrEvent {
    pub text: String,
    pub is_final: bool,
    /// True when server marks this as a first-pass/pre-fetch result in two-pass mode.
    pub prefetch: bool,
    /// Raw definite flag (two-pass final marker).
    pub definite: bool,
    pub confidence: Option<f32>,
}

/// ASR error types
#[derive(Debug, thiserror::Error)]
pub enum AsrError {
    #[error("Not connected to ASR service")]
    NotConnected,

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Server error: {0}")]
    ServerError(String),

    #[error("Protocol error: {0}")]
    ProtocolError(String),

    #[error("Compression failed: {0}")]
    CompressionFailed(String),

    #[error("{source}")]
    Contextual {
        phase: AsrPhase,
        #[source]
        source: Box<AsrError>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrPhase {
    Connect,
    Handshake,
    Streaming,
    Finalizing,
}

impl AsrPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Handshake => "handshake",
            Self::Streaming => "streaming",
            Self::Finalizing => "finalizing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrFailureKind {
    Auth,
    Config,
    RateLimited,
    ProviderUnavailable,
    NetworkTransient,
    Protocol,
    Unsupported,
    Unknown,
}

impl AsrFailureKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Config => "config",
            Self::RateLimited => "rate_limited",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::NetworkTransient => "network_transient",
            Self::Protocol => "protocol",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AsrFailure {
    pub provider: AsrProviderType,
    pub phase: AsrPhase,
    pub kind: AsrFailureKind,
    pub technical_message: String,
    pub display_message: String,
    pub retryable: bool,
    pub retry_after_ms: Option<u64>,
}

impl AsrError {
    pub fn in_phase(self, phase: AsrPhase) -> Self {
        Self::Contextual {
            phase,
            source: Box::new(self),
        }
    }

    pub fn phase(&self) -> Option<AsrPhase> {
        match self {
            Self::Contextual { phase, .. } => Some(*phase),
            _ => None,
        }
    }

    pub fn root_cause(&self) -> &AsrError {
        match self {
            Self::Contextual { source, .. } => source.root_cause(),
            other => other,
        }
    }

    pub fn root_message(&self) -> String {
        match self.root_cause() {
            Self::NotConnected => "Not connected to ASR service".to_string(),
            Self::ConnectionFailed(message)
            | Self::ServerError(message)
            | Self::ProtocolError(message)
            | Self::CompressionFailed(message) => message.clone(),
            Self::Contextual { .. } => unreachable!("root_cause strips contextual wrappers"),
        }
    }
}

impl AsrFailure {
    pub fn from_error(provider: AsrProviderType, error: &AsrError) -> Self {
        let phase = error.phase().unwrap_or(AsrPhase::Streaming);
        let technical_message = error.root_message();
        let lower = technical_message.to_lowercase();

        let (kind, retryable, retry_after_ms) = classify_failure(error.root_cause(), &lower);
        let display_message = build_display_message(provider, phase, kind);

        Self {
            provider,
            phase,
            kind,
            technical_message,
            display_message,
            retryable,
            retry_after_ms,
        }
    }
}

fn classify_failure(error: &AsrError, lower: &str) -> (AsrFailureKind, bool, Option<u64>) {
    match error {
        AsrError::NotConnected => (AsrFailureKind::NetworkTransient, true, None),
        AsrError::CompressionFailed(_) | AsrError::ProtocolError(_) => {
            (AsrFailureKind::Protocol, false, None)
        }
        AsrError::ConnectionFailed(_) => classify_message(lower, true),
        AsrError::ServerError(_) => classify_message(lower, false),
        AsrError::Contextual { .. } => unreachable!("root cause should not be contextual"),
    }
}

fn classify_message(
    lower: &str,
    assume_network_for_connection_errors: bool,
) -> (AsrFailureKind, bool, Option<u64>) {
    if contains_any(
        lower,
        &[
            "401",
            "403",
            "unauthorized",
            "forbidden",
            "invalid api key",
            "authentication",
            "auth failed",
        ],
    ) {
        return (AsrFailureKind::Auth, false, None);
    }

    if contains_any(
        lower,
        &[
            "invalid configuration",
            "config invalid",
            "configuration is required",
            "missing ",
            "project id and service account key are required",
            "api key is required",
            "invalid service account json",
        ],
    ) {
        return (AsrFailureKind::Config, false, None);
    }

    if contains_any(lower, &["429", "rate limit", "too many requests", "quota"]) {
        return (AsrFailureKind::RateLimited, true, None);
    }

    if contains_any(
        lower,
        &[
            "502",
            "503",
            "504",
            "service unavailable",
            "bad gateway",
            "gateway timeout",
        ],
    ) {
        return (AsrFailureKind::ProviderUnavailable, true, None);
    }

    if contains_any(
        lower,
        &[
            "timeout",
            "timed out",
            "connection reset",
            "broken pipe",
            "unexpected eof",
            "temporarily unavailable",
            "dns",
            "tls",
            "websocket connect",
            "read failed",
            "send failed",
            "closed by server",
            "connect:",
            "connect ",
        ],
    ) || assume_network_for_connection_errors
    {
        return (AsrFailureKind::NetworkTransient, true, None);
    }

    if contains_any(lower, &["not supported", "unsupported"]) {
        return (AsrFailureKind::Unsupported, false, None);
    }

    (AsrFailureKind::Unknown, false, None)
}

fn build_display_message(
    provider: AsrProviderType,
    phase: AsrPhase,
    kind: AsrFailureKind,
) -> String {
    let provider_name = provider.display_name();

    match kind {
        AsrFailureKind::Auth => format!("{provider_name} 认证失败，请检查 API Key 和相关配置。"),
        AsrFailureKind::Config => format!("{provider_name} 配置无效，请检查设置后重试。"),
        AsrFailureKind::RateLimited => format!("{provider_name} 当前请求过多，请稍后再试。"),
        AsrFailureKind::ProviderUnavailable => match phase {
            AsrPhase::Connect | AsrPhase::Handshake => {
                format!("{provider_name} 当前服务不可用，无法启动识别。")
            }
            AsrPhase::Streaming | AsrPhase::Finalizing => {
                format!("{provider_name} 当前服务异常，识别已中断。")
            }
        },
        AsrFailureKind::NetworkTransient => match phase {
            AsrPhase::Connect | AsrPhase::Handshake => {
                format!("{provider_name} 连接失败，无法启动识别。")
            }
            AsrPhase::Streaming | AsrPhase::Finalizing => {
                format!("{provider_name} 连接中断，识别已停止。")
            }
        },
        AsrFailureKind::Protocol => {
            format!("{provider_name} 返回了无法处理的响应，请稍后重试。")
        }
        AsrFailureKind::Unsupported => {
            format!("{provider_name} 当前不支持这项识别能力。")
        }
        AsrFailureKind::Unknown => format!("{provider_name} 识别失败，请稍后重试。"),
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// Parsed server error frame payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsrServerErrorFrame {
    pub code: u32,
    pub message: String,
}

#[repr(u8)]
#[allow(dead_code)]
pub enum MessageType {
    ClientFullRequest = 0b0001,
    ClientAudioOnlyRequest = 0b0010,
    ServerFullResponse = 0b1001,
    ServerError = 0b1111,
}

/// Build header bytes (4 bytes) given message type, flags, serialization, compression, header words.
fn header_bytes(
    message_type: MessageType,
    flags: u8,
    serialization: u8,
    compression: u8,
    header_words: u8,
) -> [u8; 4] {
    let version = 0b0001;
    let byte0 = (version << 4) | (header_words & 0x0f);
    let byte1 = ((message_type as u8) << 4) | (flags & 0x0f);
    let byte2 = ((serialization & 0x0f) << 4) | (compression & 0x0f);
    [byte0, byte1, byte2, 0]
}

/// Encode a full client request with no sequence field, per the current Volcengine spec.
pub fn encode_full_request(payload: &[u8]) -> Vec<u8> {
    let header = header_bytes(
        MessageType::ClientFullRequest,
        0b0000,
        0b0001,
        0b0001,
        0b0001,
    );

    let mut data = Vec::with_capacity(8 + payload.len());
    data.extend_from_slice(&header);
    data.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    data.extend_from_slice(payload);
    data
}

/// Encode an audio-only packet (raw PCM, no compression).
pub fn encode_audio_packet(seq: u32, audio: &[u8], is_last: bool) -> Vec<u8> {
    if is_last {
        // flags 0b0010 -> last packet, no sequence field
        let header = header_bytes(
            MessageType::ClientAudioOnlyRequest,
            0b0010,
            0b0000,
            0b0001,
            0b0001,
        );
        let mut data = Vec::with_capacity(8 + audio.len());
        data.extend_from_slice(&header);
        data.extend_from_slice(&(audio.len() as u32).to_be_bytes());
        data.extend_from_slice(audio);
        data
    } else {
        // flags 0b0001 -> sequence present and positive
        let header = header_bytes(
            MessageType::ClientAudioOnlyRequest,
            0b0001,
            0b0000,
            0b0001,
            0b0001,
        );

        let mut data = Vec::with_capacity(8 + 4 + audio.len());
        data.extend_from_slice(&header);
        data.extend_from_slice(&(seq as i32).to_be_bytes());
        data.extend_from_slice(&(audio.len() as u32).to_be_bytes());
        data.extend_from_slice(audio);
        data
    }
}

/// Decode server full response payload to text/is_final.
pub fn decode_server_response(payload: &[u8]) -> Option<AsrEvent> {
    #[derive(Deserialize)]
    struct Utterance {
        text: Option<String>,
        #[serde(default)]
        definite: Option<bool>,
    }

    #[derive(Deserialize)]
    struct ResultItem {
        text: Option<String>,
        #[serde(default)]
        definite: Option<bool>,
        #[serde(default)]
        prefetch: Option<bool>,
        #[serde(default)]
        utterances: Option<Vec<Utterance>>,
    }

    #[derive(Deserialize)]
    struct ResponseVec {
        result: Option<Vec<ResultItem>>,
        text: Option<String>,
    }

    #[derive(Deserialize)]
    struct ResponseObj {
        result: Option<ResultItem>,
        text: Option<String>,
    }

    // Helper to fold result item / utterance into a unified event.
    fn from_item(item: ResultItem) -> Option<AsrEvent> {
        let text = item
            .text
            .or_else(|| {
                item.utterances
                    .as_ref()
                    .and_then(|u| u.last().and_then(|u| u.text.clone()))
            })
            .unwrap_or_default();
        if text.is_empty() {
            return None;
        }

        let definite = item
            .definite
            .or_else(|| {
                item.utterances
                    .as_ref()
                    .and_then(|u| u.last().and_then(|u| u.definite))
            })
            .unwrap_or(false);
        let prefetch = item.prefetch.unwrap_or(false);

        Some(AsrEvent {
            text,
            is_final: definite,
            prefetch,
            definite,
            confidence: None,
        })
    }

    // Case 1: top-level text or vec result
    if let Ok(parsed) = serde_json::from_slice::<ResponseVec>(payload) {
        if let Some(text) = parsed.text {
            return Some(AsrEvent {
                text,
                is_final: false,
                prefetch: false,
                definite: false,
                confidence: None,
            });
        }

        if let Some(mut list) = parsed.result {
            if let Some(item) = list.pop() {
                if let Some(evt) = from_item(item) {
                    return Some(evt);
                }
            }
        }
    }

    // Case 2: result as object (common shape: {"result":{"text": "...", "definite": bool}})
    if let Ok(parsed) = serde_json::from_slice::<ResponseObj>(payload) {
        if let Some(text) = parsed.text {
            return Some(AsrEvent {
                text,
                is_final: false,
                prefetch: false,
                definite: false,
                confidence: None,
            });
        }

        if let Some(result) = parsed.result {
            return from_item(result);
        }
    }

    None
}

/// Decode a full server error frame according to the documented layout:
/// header + error code (4B) + error size (4B) + error message bytes.
pub fn decode_server_error_frame(frame: &[u8]) -> Option<AsrServerErrorFrame> {
    if frame.len() < 12 {
        return None;
    }

    let header_size_words = std::cmp::max(parse_header_size(frame), 1);
    let header_bytes = header_size_words * 4;
    if frame.len() < header_bytes + 8 {
        return None;
    }

    let compression = frame.get(2).map(|b| b & 0x0f).unwrap_or(0);
    let code = u32::from_be_bytes([
        frame[header_bytes],
        frame[header_bytes + 1],
        frame[header_bytes + 2],
        frame[header_bytes + 3],
    ]);
    let size_offset = header_bytes + 4;
    let message_size = u32::from_be_bytes([
        frame[size_offset],
        frame[size_offset + 1],
        frame[size_offset + 2],
        frame[size_offset + 3],
    ]) as usize;
    let message_offset = header_bytes + 8;
    let available = frame.len().saturating_sub(message_offset);
    let effective_size = std::cmp::min(message_size, available);
    let message_slice = &frame[message_offset..message_offset + effective_size];

    let message_bytes = if compression == 0x01 {
        let mut decoder = GzDecoder::new(message_slice);
        let mut buf = Vec::new();
        if decoder.read_to_end(&mut buf).is_err() {
            return None;
        }
        buf
    } else {
        message_slice.to_vec()
    };

    Some(AsrServerErrorFrame {
        code,
        message: String::from_utf8_lossy(&message_bytes).to_string(),
    })
}

/// Parse header size words (lower nibble of first byte)
pub fn parse_header_size(header: &[u8]) -> usize {
    if header.len() < 1 {
        return 0;
    }
    (header[0] & 0x0f) as usize
}

/// Parse message type (high nibble of byte1)
pub fn parse_message_type(header: &[u8]) -> Option<u8> {
    header.get(1).map(|b| b >> 4)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::{write::GzEncoder, Compression};

    use super::*;

    #[test]
    fn encode_full_request_uses_documented_layout_without_sequence() {
        let payload = br#"{"request":"ok"}"#;
        let frame = encode_full_request(payload);

        assert_eq!(frame[0], 0x11);
        assert_eq!(frame[1], 0x10);
        assert_eq!(frame[2], 0x11);
        assert_eq!(frame.len(), 8 + payload.len());
        assert_eq!(
            u32::from_be_bytes([frame[4], frame[5], frame[6], frame[7]]) as usize,
            payload.len()
        );
        assert_eq!(&frame[8..], payload);
    }

    #[test]
    fn decode_server_error_frame_reads_code_and_plaintext_message() {
        let message = br#"{"code":45000001,"message":"bad request"}"#;
        let mut frame = vec![0x11, 0xf0, 0x10, 0x00];
        frame.extend_from_slice(&45000001u32.to_be_bytes());
        frame.extend_from_slice(&(message.len() as u32).to_be_bytes());
        frame.extend_from_slice(message);

        let err = decode_server_error_frame(&frame).expect("error frame should decode");
        assert_eq!(err.code, 45000001);
        assert_eq!(err.message, String::from_utf8_lossy(message));
    }

    #[test]
    fn decode_server_error_frame_supports_gzip_message_bytes() {
        let message = br#"{"code":55000031,"message":"server busy"}"#;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(message).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut frame = vec![0x11, 0xf0, 0x11, 0x00];
        frame.extend_from_slice(&55000031u32.to_be_bytes());
        frame.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
        frame.extend_from_slice(&compressed);

        let err = decode_server_error_frame(&frame).expect("gzip error frame should decode");
        assert_eq!(err.code, 55000031);
        assert_eq!(err.message, String::from_utf8_lossy(message));
    }
}
