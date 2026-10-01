//! Microsoft Edge online Read Aloud, using its consumer WebSocket protocol.
//! No user credentials are required. Protocol identifiers are versioned here;
//! upstream changes fail visibly rather than switching to another provider.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Error as WsError, Message};

use super::caption_timeline::{self, CaptionTimeline};
use super::cloud_playback::{self, PieceStream};
use super::playback::{negotiate_sample_rate_among, prebuffer_samples, PlaybackHandle};
use super::{
    cloud_http_client, log_cloud_retry, log_event, split_for_backend, CancelToken,
    CloudStreamError, SpeechProgress, TtsBackend, TtsError, TtsRequest, TtsStatus, TtsVoice,
};

mod captions;
use captions::{Boundary, CaptionFeed};

const CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
const EDGE_VERSION: &str = "143.0.3650.75";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36 Edg/143.0.0.0";
const BASE_URL: &str = "speech.platform.bing.com/consumer/speech/synthesize/readaloud";
const STREAM_RATE: u32 = 24_000;
const OUTPUT_FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";
const MAX_MESSAGE_BYTES: usize = 4096;
const MAX_PIECE_CHARS: usize = 1800;
const DEFAULT_CAPTION_CHARS: usize = 120;
const IDLE_TIMEOUT: Duration = Duration::from_secs(20);

pub fn default_voice() -> &'static str {
    "zh-CN-XiaoxiaoNeural"
}

#[derive(Default)]
struct EdgeClock(AtomicI64);
impl EdgeClock {
    fn now(&self) -> i64 {
        Utc::now().timestamp() + self.0.load(Ordering::Relaxed)
    }
    fn correct(&self, date: Option<&str>) -> bool {
        let Some(server) = date.and_then(|s| chrono::DateTime::parse_from_rfc2822(s).ok()) else {
            return false;
        };
        let skew = server.timestamp() - Utc::now().timestamp();
        let before = self.0.swap(skew, Ordering::Relaxed);
        (skew - before).abs() > 5
    }
}

fn gec_at(unix_seconds: i64) -> String {
    // Windows FILETIME epoch, five-minute buckets, then SHA-256.
    let seconds = unix_seconds + 11_644_473_600;
    let ticks = (seconds - seconds.rem_euclid(300)) as u64 * 10_000_000;
    hex::encode_upper(Sha256::digest(format!("{ticks}{CLIENT_TOKEN}").as_bytes()))
}
fn auth_query(clock: &EdgeClock) -> String {
    format!(
        "TrustedClientToken={CLIENT_TOKEN}&Sec-MS-GEC={}&Sec-MS-GEC-Version=1-{EDGE_VERSION}",
        gec_at(clock.now())
    )
}
fn timestamp() -> String {
    Utc::now()
        .format("%a %b %d %Y %H:%M:%S GMT+0000 (Coordinated Universal Time)")
        .to_string()
}
fn request_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
fn escape_xml(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&apos;".to_string(),
            _ => ch.to_string(),
        })
        .collect()
}
fn unescape_xml(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}
fn speed(rate: Option<f32>) -> f32 {
    (rate.unwrap_or(0.5) / 0.5).clamp(0.5, 2.0)
}
fn ssml(text: &str, voice: &str, rate: f32, pitch_hz: i32) -> String {
    let name = match voice.rsplit_once('-') {
        Some((locale, speaker)) if locale.contains('-') => {
            format!("Microsoft Server Speech Text to Speech Voice ({locale}, {speaker})")
        }
        _ => voice.to_string(),
    };
    format!("<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='en-US'><voice name='{}'><prosody rate='{:+.0}%' pitch='{:+}Hz' volume='+0%'>{}</prosody></voice></speak>",
        escape_xml(&name), (rate - 1.0) * 100.0, pitch_hz.clamp(-100,100), escape_xml(text))
}
fn ssml_message(body: &str, id: &str) -> String {
    format!("X-RequestId:{id}\r\nContent-Type:application/ssml+xml\r\nX-Timestamp:{}\r\nPath:ssml\r\n\r\n{body}", timestamp())
}

/// Budget the actual UTF-8/XML message, preserving all text and natural cuts.
fn split_requests(text: &str, voice: &str, rate: f32, pitch: i32) -> Result<Vec<String>, String> {
    let overhead = ssml_message(&ssml("", voice, rate, pitch), &"0".repeat(32)).len();
    let budget = MAX_MESSAGE_BYTES
        .checked_sub(overhead)
        .filter(|b| *b >= 6)
        .ok_or("Edge voice identifier is too long")?;
    // The subtitle length never caps synthesis. Slow speech uses a smaller
    // character ceiling; all rates still obey the actual UTF-8/XML budget.
    let cap = (MAX_PIECE_CHARS as f32 * rate.clamp(0.5, 1.0)) as usize;
    let mut rest = text;
    let mut pieces = Vec::new();
    while !rest.is_empty() {
        let mut used = 0;
        let count = rest
            .chars()
            .take(cap)
            .take_while(|ch| {
                used += escape_xml(&ch.to_string()).len();
                used <= budget
            })
            .count();
        if count == 0 {
            return Err("Edge SSML message exceeds its byte budget".to_string());
        }
        // Only split this bounded prefix. Its last boundary may make the first
        // piece shorter; the untouched suffix is processed on the next pass.
        let end = rest
            .char_indices()
            .nth(count + 1)
            .map_or(rest.len(), |(i, _)| i);
        let piece = split_for_backend(&rest[..end], count)
            .into_iter()
            .next()
            .ok_or("Empty Edge text piece")?;
        rest = &rest[piece.len()..];
        pieces.push(piece);
    }
    Ok(pieces)
}

#[derive(Deserialize)]
struct VoiceRecord {
    #[serde(rename = "ShortName")]
    id: String,
    #[serde(rename = "FriendlyName")]
    name: String,
    #[serde(rename = "Locale")]
    language: String,
}
async fn fetch_voices(clock: &EdgeClock) -> Result<Vec<TtsVoice>, String> {
    for attempt in 0..2 {
        let response = cloud_http_client()
            .map_err(|e| e.to_string())?
            .get(format!(
                "https://{BASE_URL}/voices/list?{}",
                auth_query(clock)
            ))
            .header("User-Agent", USER_AGENT)
            .header("Cookie", format!("muid={};", request_id().to_uppercase()))
            .timeout(IDLE_TIMEOUT)
            .send()
            .await
            .map_err(|_| {
                "Edge voice list request failed (check the network/system proxy)".to_string()
            })?;
        let status = response.status();
        if status.as_u16() == 403
            && attempt == 0
            && clock.correct(response.headers().get("Date").and_then(|v| v.to_str().ok()))
        {
            continue;
        }
        if !status.is_success() {
            return Err(format!("Edge voice list returned HTTP {status}"));
        }
        let records: Vec<VoiceRecord> = response
            .json()
            .await
            .map_err(|_| "Invalid Edge voice list response".to_string())?;
        if records.is_empty() {
            return Err("Edge returned an empty voice list".to_string());
        }
        let mut voices: Vec<_> = records
            .into_iter()
            .map(|v| TtsVoice {
                id: v.id,
                name: v.name,
                language: v.language,
            })
            .collect();
        voices.sort_by(|a, b| a.language.cmp(&b.language).then(a.id.cmp(&b.id)));
        return Ok(voices);
    }
    Err("Edge voice list authentication failed".to_string())
}

// Each read owns separate playback state. A late decoder from a cancelled
// read cannot clear a newer read's speaking flag, handle, captions or error.
struct EdgeRead {
    playback: Arc<Mutex<Option<PlaybackHandle>>>,
    speaking: Arc<AtomicBool>,
    timeline: Arc<Mutex<CaptionTimeline>>,
    error: Arc<Mutex<Option<String>>>,
}
impl EdgeRead {
    fn new(rate: u32) -> Self {
        Self {
            playback: Arc::new(Mutex::new(None)),
            speaking: Arc::new(AtomicBool::new(false)),
            timeline: Arc::new(Mutex::new(CaptionTimeline::new(rate))),
            error: Arc::new(Mutex::new(None)),
        }
    }
}
pub struct EdgeBackend {
    read: Mutex<Option<Arc<EdgeRead>>>,
    clock: Arc<EdgeClock>,
}
impl Default for EdgeBackend {
    fn default() -> Self {
        Self::new()
    }
}
impl EdgeBackend {
    pub fn new() -> Self {
        Self {
            read: Mutex::new(None),
            clock: Arc::new(EdgeClock::default()),
        }
    }
    fn current_read(&self) -> Option<Arc<EdgeRead>> {
        self.read.lock().ok()?.clone()
    }
    fn begin(&self, request: TtsRequest, token: CancelToken) -> Result<(), TtsError> {
        *self
            .read
            .lock()
            .map_err(|_| TtsError::Backend("Edge read lock is poisoned".to_string()))? = None;
        if request
            .text
            .chars()
            .any(|c| c < ' ' && !matches!(c, '\n' | '\r' | '\t'))
        {
            return Err(TtsError::Backend(
                "Text contains a control character unsupported by Edge SSML".to_string(),
            ));
        }
        let rate = negotiate_sample_rate_among(&[48_000, 44_100, STREAM_RATE])
            .map_err(|e| TtsError::Backend(e.to_string()))?;
        let voice = request
            .voice
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(default_voice())
            .to_string();
        let pace = speed(request.rate);
        let pitch = request.pitch_hz.unwrap_or(0).clamp(-100, 100);
        let caption_limit = request.piece_limit.unwrap_or(DEFAULT_CAPTION_CHARS);
        let pieces =
            split_requests(&request.text, &voice, pace, pitch).map_err(TtsError::Backend)?;
        if pieces.is_empty() {
            return Err(TtsError::Backend("Nothing to speak".to_string()));
        }
        let read = Arc::new(EdgeRead::new(rate));
        *self
            .read
            .lock()
            .map_err(|_| TtsError::Backend("Edge read lock is poisoned".to_string()))? =
            Some(read.clone());
        let timeline = read.timeline.clone();
        let error = read.error.clone();
        let (tx, rx) = mpsc::channel::<PieceStream>();
        let handle = read.playback.clone();
        let speaking = read.speaking.clone();
        let decode_token = token.clone();
        let decode_error = error.clone();
        let gain = request.volume.unwrap_or(1.0).clamp(0.0, 1.0);
        std::thread::Builder::new()
            .name("voicex-tts-edge".to_string())
            .spawn(move || {
                cloud_playback::run_playback_with_rates(
                    rx,
                    STREAM_RATE,
                    rate,
                    gain,
                    prebuffer_samples(pace, rate),
                    decode_token,
                    decode_error,
                    handle,
                    speaking.clone(),
                );
                speaking.store(false, Ordering::SeqCst);
            })
            .map_err(|e| TtsError::Backend(format!("Failed to start Edge playback: {e}")))?;
        let clock = self.clock.clone();
        tauri::async_runtime::spawn(async move {
            for (index, piece) in pieces.iter().enumerate() {
                if !wait_for_request_window(index, &read.playback, &token).await {
                    break;
                }
                let (audio_tx, audio_rx) = mpsc::channel();
                if tx.send(audio_rx).is_err() {
                    break;
                }
                let synthesis = Synthesis {
                    text: piece,
                    voice: &voice,
                    rate: pace,
                    pitch,
                    index,
                    timeline: &timeline,
                    caption_limit,
                };
                let outcome = stream_with_retry(&synthesis, &audio_tx, &token, &clock).await;
                // Publish the failure before closing the audio channel: the
                // sink must not mistake a truncated response for completion.
                if let Err(detail) = &outcome {
                    if let Ok(mut slot) = error.lock() {
                        *slot = Some(detail.clone());
                    }
                }
                drop(audio_tx);
                if !matches!(outcome, Ok(true)) {
                    break;
                }
            }
            drop(tx);
        });
        Ok(())
    }
}
impl TtsBackend for EdgeBackend {
    fn name(&self) -> &'static str {
        "edge"
    }
    fn list_voices(&self) -> Result<Vec<TtsVoice>, TtsError> {
        tauri::async_runtime::block_on(fetch_voices(&self.clock)).map_err(TtsError::Backend)
    }
    fn start(&self, request: TtsRequest, token: CancelToken) -> Result<(), TtsError> {
        self.begin(request, token.clone()).inspect_err(|_| {
            token.finish();
        })
    }
    fn stop(&self) -> Result<(), TtsError> {
        if let Some(read) = self.current_read() {
            read.speaking.store(false, Ordering::SeqCst);
            if let Ok(slot) = read.playback.lock() {
                if let Some(handle) = slot.as_ref() {
                    handle.stop();
                }
            }
        }
        Ok(())
    }
    fn status(&self) -> TtsStatus {
        if self
            .current_read()
            .is_some_and(|r| r.speaking.load(Ordering::SeqCst))
        {
            TtsStatus::Speaking
        } else {
            TtsStatus::Idle
        }
    }
    fn failure(&self) -> Option<String> {
        let read = self.current_read()?;
        let error = read.error.lock().ok()?.clone();
        error
    }
    fn audio_level(&self) -> Option<f32> {
        let read = self.current_read()?;
        let level = read.playback.lock().ok()?.as_ref()?.level();
        level
    }
    fn reports_progress(&self) -> bool {
        true
    }
    fn progress(&self) -> Option<SpeechProgress> {
        let read = self.current_read()?;
        let timeline = read.timeline.lock().ok()?;
        caption_timeline::progress(&read.speaking, &timeline, &read.playback)
    }
}

struct Synthesis<'a> {
    text: &'a str,
    voice: &'a str,
    rate: f32,
    pitch: i32,
    index: usize,
    timeline: &'a Mutex<CaptionTimeline>,
    caption_limit: usize,
}

fn request_in_window(index: usize, playing: Option<usize>) -> bool {
    index <= playing.unwrap_or(0).saturating_add(1)
}

/// Receive the next request while the current one plays, but don't synthesize
/// the rest of a long document into unbounded buffers ahead of playback.
async fn wait_for_request_window(
    index: usize,
    playback: &Mutex<Option<PlaybackHandle>>,
    token: &CancelToken,
) -> bool {
    loop {
        if token.is_cancelled() {
            return false;
        }
        let playing = playback
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().and_then(PlaybackHandle::current_piece));
        if request_in_window(index, playing) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
}

/// Poll cancellation even while connection, send or receive futures are parked.
async fn cancellable<T>(token: &CancelToken, future: impl Future<Output = T>) -> Option<T> {
    tokio::pin!(future);
    loop {
        if token.is_cancelled() {
            return None;
        }
        tokio::select! { result=&mut future=>return Some(result), _=tokio::time::sleep(Duration::from_millis(60))=>{} }
    }
}
fn protocol_error(detail: impl Into<String>, emitted: bool) -> CloudStreamError {
    CloudStreamError::new(detail.into(), false, emitted, "protocol")
}
fn ws_error(err: WsError, emitted: bool, clock: &EdgeClock) -> CloudStreamError {
    match err {
        WsError::Http(response) => {
            let status = response.status();
            let skew = status.as_u16() == 403
                && clock.correct(response.headers().get("Date").and_then(|v| v.to_str().ok()));
            let retryable = skew || matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504);
            CloudStreamError::new(
                format!("Edge connection returned HTTP {status}"),
                retryable,
                emitted,
                if skew { "clock_skew" } else { "http" },
            )
        }
        // Do not log request objects: their URL contains protocol tokens.
        WsError::Io(_) => CloudStreamError::new(
            "Edge network connection failed (check the system proxy)".to_string(),
            true,
            emitted,
            "transport",
        ),
        _ => protocol_error("Edge WebSocket protocol failed", emitted),
    }
}
async fn stream_with_retry(
    s: &Synthesis<'_>,
    tx: &Sender<Vec<u8>>,
    token: &CancelToken,
    clock: &EdgeClock,
) -> Result<bool, String> {
    let mut retries = 0;
    loop {
        s.timeline
            .lock()
            .map_err(|_| "Edge captions lock is poisoned")?
            .begin_request(s.index);
        match stream_audio(s, tx, token, clock).await {
            Ok(done) => return Ok(done),
            Err(error) => {
                if token.is_cancelled() {
                    return Ok(false);
                }
                let Some(delay) = error.retry_delay_ms(retries) else {
                    return Err(error.into_detail());
                };
                log_cloud_retry("edge", retries, delay, error.reason());
                if cancellable(token, tokio::time::sleep(Duration::from_millis(delay)))
                    .await
                    .is_none()
                {
                    return Ok(false);
                }
                retries += 1;
            }
        }
    }
}
fn frame<'a>(bytes: &'a [u8], binary: bool) -> Result<(&'a str, &'a [u8]), String> {
    let (headers, body) = if binary {
        if bytes.len() < 2 {
            return Err("Edge binary frame has no header length".to_string());
        }
        let length = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
        if length > bytes.len() - 2 {
            return Err("Edge binary frame header is truncated".to_string());
        }
        (&bytes[2..2 + length], &bytes[2 + length..])
    } else {
        let end = bytes
            .windows(4)
            .position(|v| v == b"\r\n\r\n")
            .ok_or("Edge text frame has no header separator")?;
        (&bytes[..end], &bytes[end + 4..])
    };
    let headers = std::str::from_utf8(headers).map_err(|_| "Edge frame header is not UTF-8")?;
    let path = headers
        .lines()
        .find_map(|line| line.strip_prefix("Path:"))
        .map(str::trim)
        .ok_or("Edge frame has no path")?;
    Ok((path, body))
}
fn metadata(body: &[u8]) -> Result<Vec<Boundary>, String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| "Invalid Edge sentence metadata")?;
    let entries = value
        .get("Metadata")
        .and_then(Value::as_array)
        .ok_or("Edge metadata has no entries")?;
    let mut captions = Vec::new();
    for entry in entries {
        match entry.get("Type").and_then(Value::as_str) {
            Some(kind @ ("SentenceBoundary" | "WordBoundary")) => {
                let offset = entry
                    .pointer("/Data/Offset")
                    .and_then(Value::as_u64)
                    .ok_or("Edge sentence has no timestamp")?;
                let text = entry
                    .pointer("/Data/text/Text")
                    .and_then(Value::as_str)
                    .ok_or("Edge sentence has no text")?;
                let start_ms = offset / 10_000;
                let text = unescape_xml(text);
                captions.push(if kind == "SentenceBoundary" {
                    let duration_ms = entry
                        .pointer("/Data/Duration")
                        .and_then(Value::as_u64)
                        .ok_or("Edge sentence has no duration")?
                        / 10_000;
                    Boundary::Sentence {
                        start_ms,
                        duration_ms,
                        text,
                    }
                } else {
                    Boundary::Word { start_ms, text }
                });
            }
            Some("SessionEnd") => {}
            _ => return Err("Unexpected Edge metadata type".to_string()),
        }
    }
    Ok(captions)
}
async fn stream_audio(
    s: &Synthesis<'_>,
    tx: &Sender<Vec<u8>>,
    token: &CancelToken,
    clock: &EdgeClock,
) -> Result<bool, CloudStreamError> {
    let id = request_id();
    let url = format!(
        "wss://{BASE_URL}/edge/v1?{}&ConnectionId={id}",
        auth_query(clock)
    );
    let mut request = url
        .into_client_request()
        .map_err(|_| protocol_error("Invalid Edge connection URL", false))?;
    for (key, value) in [
        ("User-Agent", USER_AGENT.to_string()),
        (
            "Origin",
            "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold".to_string(),
        ),
        ("Cache-Control", "no-cache".to_string()),
        ("Pragma", "no-cache".to_string()),
        ("Cookie", format!("muid={};", request_id().to_uppercase())),
    ] {
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::HeaderName::from_bytes(key.as_bytes())
                .unwrap(),
            value
                .parse()
                .map_err(|_| protocol_error("Invalid Edge request header", false))?,
        );
    }
    let Some(connection) = cancellable(token, crate::network::connect_async(request)).await else {
        return Ok(false);
    };
    let (mut ws, _) = connection.map_err(|e| ws_error(e, false, clock))?;
    let body = ssml(s.text, s.voice, s.rate, s.pitch);
    let configuration = json!({"context":{"synthesis":{"audio":{"metadataoptions":{"sentenceBoundaryEnabled":"true","wordBoundaryEnabled":"true"},"outputFormat":OUTPUT_FORMAT}}}});
    let messages=[format!("X-Timestamp:{}\r\nContent-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n{configuration}",timestamp()),ssml_message(&body,&id)];
    log_event(
        "edge_request",
        &[
            ("voice", s.voice.to_string()),
            ("request_id", id),
            ("piece", s.index.to_string()),
            ("chars", s.text.chars().count().to_string()),
        ],
    );
    for message in messages {
        let Some(result) = cancellable(
            token,
            tokio::time::timeout(IDLE_TIMEOUT, ws.send(Message::Text(message))),
        )
        .await
        else {
            return Ok(false);
        };
        result
            .map_err(|_| {
                CloudStreamError::new("Edge send timed out".to_string(), true, false, "timeout")
            })?
            .map_err(|e| ws_error(e, false, clock))?;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let mut emitted = false;
    let mut captions = CaptionFeed::new(s.caption_limit);
    loop {
        let receive = tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + IDLE_TIMEOUT),
            ws.next(),
        );
        let Some(result) = cancellable(token, receive).await else {
            return Ok(false);
        };
        let next = result.map_err(|_| {
            CloudStreamError::new(
                "Edge audio reception timed out".to_string(),
                true,
                emitted,
                "timeout",
            )
        })?;
        let message = next
            .ok_or_else(|| protocol_error("Edge connection ended before turn.end", emitted))?
            .map_err(|e| ws_error(e, emitted, clock))?;
        match message {
            Message::Binary(bytes) => {
                let (path, data) = frame(&bytes, true).map_err(|e| protocol_error(e, emitted))?;
                if path != "audio" {
                    return Err(protocol_error("Unexpected Edge binary frame", emitted));
                }
                if !data.is_empty() {
                    if tx.send(data.to_vec()).is_err() {
                        return Ok(false);
                    }
                    emitted = true;
                }
            }
            Message::Text(text) => {
                let (path, data) =
                    frame(text.as_bytes(), false).map_err(|e| protocol_error(e, emitted))?;
                match path {
                    "audio.metadata" => {
                        for event in metadata(data).map_err(|e| protocol_error(e, emitted))? {
                            captions.read(event);
                        }
                        if let Some(schedule) = captions.schedule(false) {
                            s.timeline
                                .lock()
                                .map_err(|_| {
                                    protocol_error("Edge captions lock is poisoned", emitted)
                                })?
                                .replace_request(s.index, schedule);
                        }
                    }
                    "turn.end" => {
                        if let Some(schedule) = captions.schedule(true) {
                            s.timeline
                                .lock()
                                .map_err(|_| {
                                    protocol_error("Edge captions lock is poisoned", emitted)
                                })?
                                .replace_request(s.index, schedule);
                        }
                        return if emitted {
                            Ok(true)
                        } else {
                            Err(protocol_error("Edge returned no audio", false))
                        };
                    }
                    "turn.start" | "response" => {}
                    _ => return Err(protocol_error("Unexpected Edge text frame", emitted)),
                }
            }
            Message::Close(_) => {
                return Err(protocol_error(
                    "Edge connection closed before completion",
                    emitted,
                ))
            }
            Message::Ping(_) | Message::Pong(_) => {}
            _ => return Err(protocol_error("Unexpected Edge WebSocket message", emitted)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::SessionSlot;
    use super::*;

    #[test]
    fn a_previous_decoder_cannot_change_the_current_read() {
        let backend = EdgeBackend::new();
        let old = Arc::new(EdgeRead::new(48_000));
        let next = Arc::new(EdgeRead::new(48_000));
        *backend.read.lock().unwrap() = Some(old.clone());
        next.speaking.store(true, Ordering::SeqCst);
        *backend.read.lock().unwrap() = Some(next);
        old.speaking.store(false, Ordering::SeqCst);
        *old.error.lock().unwrap() = Some("old failure".to_string());
        assert_eq!(backend.status(), TtsStatus::Speaking);
        assert_eq!(backend.failure(), None);
    }

    #[test]
    #[ignore = "contacts Microsoft and opens the local audio device (muted)"]
    fn live_playback_captions_stop_and_restart() {
        use std::time::Instant;
        let backend = EdgeBackend::new();
        let session = SessionSlot::default();
        let run = |text: &str| {
            let mut request = TtsRequest::plain(text.to_string());
            request.volume = Some(0.0);
            request.rate = Some(0.75);
            request.pitch_hz = Some(25);
            request.piece_limit = Some(12);
            backend.start(request, session.claim()).unwrap();
        };
        run("这是一个将被取消的朗读请求。");
        std::thread::sleep(Duration::from_millis(50));
        session.release();
        backend.stop().unwrap();
        assert_eq!(backend.status(), TtsStatus::Idle);
        run("你好，这是流式朗读。第二句验证字幕。第三句完成测试。");
        let deadline = Instant::now() + Duration::from_secs(25);
        let mut captions = Vec::new();
        while session.is_active() && Instant::now() < deadline {
            if let Some(progress) = backend.progress() {
                if captions.last() != Some(&progress.index) {
                    captions.push(progress.index);
                }
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(!session.is_active(), "playback failed to finish");
        assert_eq!(backend.failure(), None);
        assert!(captions.len() >= 2, "captions did not follow the audio");
    }

    #[test]
    fn xml_and_utf8_budget_preserve_every_character() {
        let text = "中文😀<&\"'>。".repeat(180);
        let pieces = split_requests(&text, default_voice(), 2.0, 100).unwrap();
        assert_eq!(pieces.concat(), text);
        for piece in pieces {
            assert!(
                ssml_message(&ssml(&piece, default_voice(), 2.0, 100), &"0".repeat(32)).len()
                    <= MAX_MESSAGE_BYTES
            );
        }
        assert!(ssml("<&", default_voice(), 0.5, -100).contains("&lt;&amp;"));
    }

    #[test]
    fn large_requests_keep_the_byte_budget_and_reduce_caption_sized_requests() {
        let text = "这是一段长文测试，包含反力10.05千牛和English words，验证完整性与自然停顿。"
            .repeat(100);
        let old = split_for_backend(&text, DEFAULT_CAPTION_CHARS);
        let new = split_requests(&text, default_voice(), 1.0, 0).unwrap();
        assert_eq!(new.concat(), text);
        assert!(new.len() * 5 < old.len());
        assert!(new[..new.len() - 1]
            .iter()
            .all(|piece| piece.chars().count() > 700));
        for rate in [0.5, 1.0, 2.0] {
            for piece in split_requests(&text, default_voice(), rate, 0).unwrap() {
                assert!(
                    ssml_message(&ssml(&piece, default_voice(), rate, 0), &"0".repeat(32)).len()
                        <= MAX_MESSAGE_BYTES
                );
                assert!(piece.chars().count() <= (MAX_PIECE_CHARS as f32 * rate.min(1.0)) as usize);
            }
        }
    }

    #[test]
    fn only_one_request_is_prefetched_ahead_of_playback() {
        assert!(request_in_window(0, None));
        assert!(request_in_window(1, None));
        assert!(!request_in_window(2, None));
        assert!(request_in_window(4, Some(3)));
        assert!(!request_in_window(5, Some(3)));
    }

    #[tokio::test]
    async fn stop_releases_a_producer_waiting_for_the_prefetch_window() {
        let slot = SessionSlot::default();
        let token = slot.claim();
        let playback = Arc::new(Mutex::new(None));
        let task = tokio::spawn(async move { wait_for_request_window(2, &playback, &token).await });
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        slot.release();
        assert!(!tokio::time::timeout(Duration::from_millis(200), task)
            .await
            .unwrap()
            .unwrap());
    }

    #[test]
    #[ignore = "contacts Microsoft for a large mixed-language request"]
    fn live_large_request_keeps_short_timed_captions() {
        let text = format!("{}最后验证小于符号&lt;、实际符号<和&，English words with spaces。", "本次测试反力10.05千牛，检查长句字幕的边界，保留数字和中英文之间的空格，分段只影响合成请求而不丢失任何正文，字幕继续按照实际播放的句子与单词时间切换，".repeat(28));
        let pieces = split_requests(&text, default_voice(), 1.0, 0).unwrap();
        assert_eq!(pieces.len(), 2);
        let clock = EdgeClock::default();
        let timeline = Mutex::new(CaptionTimeline::new(STREAM_RATE));
        let slot = SessionSlot::default();
        let token = slot.claim();
        for (index, piece) in pieces.iter().enumerate() {
            let (tx, rx) = mpsc::channel();
            let synthesis = Synthesis {
                text: piece,
                voice: default_voice(),
                rate: 1.0,
                pitch: 0,
                index,
                timeline: &timeline,
                caption_limit: DEFAULT_CAPTION_CHARS,
            };
            assert!(tauri::async_runtime::block_on(stream_with_retry(
                &synthesis, &tx, &token, &clock
            ))
            .unwrap());
            drop(tx);
            let count = super::super::decode::decode_mp3_stream(
                super::super::decode::ChunkSource::new(rx, token.clone()),
                STREAM_RATE,
                |_| true,
            )
            .unwrap();
            assert!(count > u64::from(STREAM_RATE) * 10);
            let mut shown = std::collections::BTreeMap::new();
            for position in (0..count).step_by(STREAM_RATE as usize / 1000) {
                if let Some(caption) = timeline.lock().unwrap().at(index, position) {
                    assert!(caption.text.chars().count() <= DEFAULT_CAPTION_CHARS);
                    shown.insert(caption.index, caption.text);
                }
            }
            assert!(shown.len() >= 2);
            assert_eq!(shown.into_values().collect::<String>(), *piece);
        }
    }
    #[test]
    fn client_hash_rotates_only_at_five_minute_boundaries() {
        assert_eq!(gec_at(1_700_000_100), gec_at(1_700_000_101));
        assert_ne!(gec_at(1_700_000_100), gec_at(1_700_000_400));
        assert_eq!(gec_at(1_700_000_100).len(), 64);
    }
    #[test]
    fn binary_frames_use_the_two_byte_header_length() {
        let headers = b"Path:audio\r\nContent-Type:audio/mpeg\r\n";
        let mut bytes = (headers.len() as u16).to_be_bytes().to_vec();
        bytes.extend(headers);
        bytes.extend([1, 2, 3]);
        assert_eq!(frame(&bytes, true).unwrap(), ("audio", &[1, 2, 3][..]));
        assert!(frame(&[0, 8, 1], true).is_err());
        assert!(frame(b"Path:audio", false).is_err());
    }
    #[test]
    fn sentence_metadata_uses_service_time_and_decodes_entities_once() {
        let body=br#"{"Metadata":[{"Type":"SentenceBoundary","Data":{"Offset":1230000,"Duration":10000000,"text":{"Text":"A &amp;lt; B"}}},{"Type":"WordBoundary","Data":{"Offset":9000000,"text":{"Text":"second"}}}]}"#;
        assert_eq!(
            metadata(body).unwrap(),
            vec![
                Boundary::Sentence {
                    start_ms: 123,
                    duration_ms: 1000,
                    text: "A &lt; B".into()
                },
                Boundary::Word {
                    start_ms: 900,
                    text: "second".into()
                }
            ]
        );
        assert!(metadata(br#"{"Metadata":[{"Type":"SentenceBoundary","Data":{}}]}"#).is_err());
    }
    #[tokio::test]
    async fn cancellation_interrupts_a_parked_network_operation() {
        let session = SessionSlot::default();
        let token = session.claim();
        let task =
            tokio::spawn(async move { cancellable(&token, std::future::pending::<()>()).await });
        session.release();
        assert!(tokio::time::timeout(Duration::from_millis(200), task)
            .await
            .unwrap()
            .unwrap()
            .is_none());
    }
    #[test]
    fn connection_failure_cannot_retry_after_audio_emission() {
        let error = CloudStreamError::new("lost".to_string(), true, true, "transport");
        assert_eq!(error.retry_delay_ms(0), None);
    }
    #[test]
    #[ignore = "contacts the Microsoft online speech service"]
    fn live_synthesis_and_voice_list() {
        let clock = EdgeClock::default();
        let voices = tauri::async_runtime::block_on(fetch_voices(&clock)).unwrap();
        assert!(voices.iter().any(|v| v.id == default_voice()));
        let timeline = Mutex::new(CaptionTimeline::new(STREAM_RATE));
        timeline.lock().unwrap().begin_request(0);
        let slot = SessionSlot::default();
        let token = slot.claim();
        let (tx, rx) = mpsc::channel();
        let synthesis = Synthesis {
            text: "你好，这是 VoiceX 的 Edge 语音合成验证。",
            voice: default_voice(),
            rate: 1.0,
            pitch: 0,
            index: 0,
            timeline: &timeline,
            caption_limit: DEFAULT_CAPTION_CHARS,
        };
        assert!(
            tauri::async_runtime::block_on(stream_with_retry(&synthesis, &tx, &token, &clock))
                .unwrap()
        );
        drop(tx);
        let chunks: Vec<_> = rx.into_iter().collect();
        assert!(chunks.len() > 1);
        let (tx, rx) = mpsc::channel();
        for chunk in chunks {
            tx.send(chunk).unwrap();
        }
        drop(tx);
        let count = super::super::decode::decode_mp3_stream(
            super::super::decode::ChunkSource::new(rx, token),
            STREAM_RATE,
            |_| true,
        )
        .unwrap();
        assert!(count > STREAM_RATE as u64);
        assert!(timeline.lock().unwrap().at(0, STREAM_RATE as u64).is_some());
    }
}
