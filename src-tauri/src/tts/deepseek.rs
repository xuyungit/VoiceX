//! DeepSeek WebSocket TTS. Each piece requires an explicit successful finish.
use super::cloud_playback::{self, PieceStream};
use super::playback::{negotiate_sample_rate, PlaybackHandle};
use super::{
    piece_limit_for, CancelToken, SpeechProgress, TtsBackend, TtsError, TtsRequest, TtsStatus,
    TtsVoice,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread;
use std::time::Duration;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

const ENDPOINT: &str = "wss://api.deepseek.com/v1/tools/tts";
// At most 4,000 billed characters even if all characters are Chinese, safely
// below both the 10,000 character and 64 KiB request limits.
const MAX_CHARS: usize = 2_000;
const VOICES: [(&str, &str); 2] = [("stella", "Stella"), ("tide", "Tide")];

pub struct DeepSeekBackend {
    key: Mutex<String>,
    playback: Arc<Mutex<Option<PlaybackHandle>>>,
    speaking: Arc<AtomicBool>,
    pieces: Mutex<Vec<String>>,
    failure: Arc<Mutex<Option<String>>>,
}
impl DeepSeekBackend {
    pub fn new() -> Self {
        Self {
            key: Mutex::new(String::new()),
            playback: Arc::new(Mutex::new(None)),
            speaking: Arc::new(AtomicBool::new(false)),
            pieces: Mutex::new(Vec::new()),
            failure: Arc::new(Mutex::new(None)),
        }
    }
    pub fn apply_key(&self, key: &str) {
        if let Ok(mut slot) = self.key.lock() {
            *slot = key.to_string();
        }
    }
    fn begin(&self, request: TtsRequest, token: CancelToken) -> Result<(), TtsError> {
        let key = self
            .key
            .lock()
            .map_err(|_| TtsError::Backend("configuration is poisoned".into()))?
            .clone();
        if key.trim().is_empty() {
            return Err(TtsError::Backend("no DeepSeek API key configured".into()));
        }
        let voice = request.voice.unwrap_or_else(|| "stella".into());
        if !VOICES.iter().any(|(id, _)| *id == voice) {
            return Err(TtsError::Backend(format!(
                "Unknown DeepSeek voice: {voice}"
            )));
        }
        let device_rate = negotiate_sample_rate().map_err(|e| TtsError::Backend(e.to_string()))?;
        let pieces = cloud_playback::split_pieces(
            &request.text,
            piece_limit_for(request.piece_limit, MAX_CHARS),
            &self.pieces,
        );
        let (tx, rx) = mpsc::channel::<PieceStream>();
        if let Ok(mut slot) = self.playback.lock() {
            *slot = None;
        }
        if let Ok(mut slot) = self.failure.lock() {
            *slot = None;
        }
        let failure = self.failure.clone();
        let playback_slot = self.playback.clone();
        let speaking = self.speaking.clone();
        let decode_token = token.clone();
        let decode_failure = failure.clone();
        let gain = request.volume.unwrap_or(1.0);
        thread::Builder::new()
            .name("voicex-tts-deepseek".into())
            .spawn(move || {
                super::mimo::run_playback(
                    rx,
                    device_rate,
                    gain,
                    decode_token,
                    decode_failure,
                    playback_slot,
                    speaking.clone(),
                );
                speaking.store(false, Ordering::SeqCst);
            })
            .map_err(|e| TtsError::Backend(format!("failed to spawn playback: {e}")))?;
        tauri::async_runtime::spawn(async move {
            for piece in pieces {
                if token.is_cancelled() {
                    break;
                }
                let (piece_tx, piece_rx) = mpsc::channel();
                if tx.send(piece_rx).is_err() {
                    break;
                }
                let outcome = stream_piece(&key, &voice, &piece, &piece_tx, &token).await;
                if let Err(error) = outcome {
                    if !token.is_cancelled() {
                        if let Ok(mut slot) = failure.lock() {
                            *slot = Some(error);
                        }
                    }
                    drop(piece_tx);
                    break;
                }
                drop(piece_tx);
            }
            drop(tx);
        });
        Ok(())
    }
}
impl TtsBackend for DeepSeekBackend {
    fn name(&self) -> &'static str {
        "deepseek"
    }
    fn list_voices(&self) -> Result<Vec<TtsVoice>, TtsError> {
        Ok(VOICES
            .iter()
            .map(|(id, name)| TtsVoice {
                id: (*id).into(),
                name: (*name).into(),
                language: "mul".into(),
            })
            .collect())
    }
    fn start(&self, request: TtsRequest, token: CancelToken) -> Result<(), TtsError> {
        self.begin(request, token.clone()).inspect_err(|_| {
            token.finish();
        })
    }
    fn stop(&self) -> Result<(), TtsError> {
        self.speaking.store(false, Ordering::SeqCst);
        if let Ok(slot) = self.playback.lock() {
            if let Some(handle) = slot.as_ref() {
                handle.stop();
            }
        }
        Ok(())
    }
    fn status(&self) -> TtsStatus {
        if self.speaking.load(Ordering::SeqCst) {
            TtsStatus::Speaking
        } else {
            TtsStatus::Idle
        }
    }
    fn failure(&self) -> Option<String> {
        self.failure.lock().ok().and_then(|s| s.clone())
    }
    fn audio_level(&self) -> Option<f32> {
        self.playback
            .lock()
            .ok()
            .and_then(|s| s.as_ref().and_then(|h| h.level()))
    }
    fn reports_progress(&self) -> bool {
        true
    }
    fn progress(&self) -> Option<SpeechProgress> {
        cloud_playback::progress(&self.speaking, &self.pieces, &self.playback)
    }
}

async fn stream_piece(
    key: &str,
    voice: &str,
    text: &str,
    tx: &mpsc::Sender<Vec<u8>>,
    token: &CancelToken,
) -> Result<(), String> {
    stream_piece_at(ENDPOINT, key, voice, text, tx, token).await
}

async fn stream_piece_at(
    endpoint: &str,
    key: &str,
    voice: &str,
    text: &str,
    tx: &mpsc::Sender<Vec<u8>>,
    token: &CancelToken,
) -> Result<(), String> {
    let url = format!("{endpoint}?voice_id={voice}&format=pcm");
    let mut req = url.into_client_request().map_err(|e| e.to_string())?;
    req.headers_mut().insert(
        "Authorization",
        format!("Bearer {key}")
            .parse()
            .map_err(|_| "Invalid API key".to_string())?,
    );
    let connect = tokio::time::timeout(Duration::from_secs(20), crate::network::connect_async(req));
    let (mut ws, _) = tokio::select! {
        _ = wait_for_cancel(token) => return Ok(()),
        result = connect => result.map_err(|_| "DeepSeek TTS connection timed out".to_string())?
            .map_err(|e| format!("DeepSeek TTS connection failed: {e}"))?,
    };
    if token.is_cancelled() {
        return Ok(());
    }
    for message in [
        json!({"event":"text","text":text}).to_string(),
        json!({"event":"finish_text"}).to_string(),
    ] {
        tokio::select! {
            _ = wait_for_cancel(token) => return Ok(()),
            result = tokio::time::timeout(Duration::from_secs(20), ws.send(Message::Text(message))) => {
                result.map_err(|_| "DeepSeek TTS send timed out".to_string())?
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    let mut audio_bytes = 0usize;
    let mut last_activity = std::time::Instant::now();
    loop {
        if token.is_cancelled() {
            return Ok(());
        }
        let next = match tokio::time::timeout(Duration::from_millis(200), ws.next()).await {
            Ok(next) => next,
            Err(_) if last_activity.elapsed() > Duration::from_secs(60) => {
                return Err("DeepSeek TTS timed out".into())
            }
            Err(_) => continue,
        };
        let msg = next
            .ok_or_else(|| "DeepSeek TTS disconnected before finish".to_string())?
            .map_err(|e| format!("DeepSeek TTS stream failed: {e}"))?;
        match msg {
            Message::Binary(bytes) => {
                if !bytes.is_empty() {
                    last_activity = std::time::Instant::now();
                    audio_bytes += bytes.len();
                    if tx.send(bytes).is_err() {
                        return Ok(());
                    }
                }
            }
            Message::Text(text) => {
                last_activity = std::time::Instant::now();
                let event: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| format!("Invalid DeepSeek TTS response: {e}"))?;
                if event["event"] == "finish" {
                    let code = event["code"].as_str().unwrap_or("");
                    if code != "success" {
                        return Err(format!(
                            "DeepSeek TTS: {}: {}",
                            code,
                            event["message"].as_str().unwrap_or("")
                        ));
                    }
                    return if audio_bytes == 0 {
                        Err("DeepSeek TTS returned no audio".into())
                    } else if audio_bytes % 2 != 0 {
                        Err("DeepSeek TTS returned incomplete PCM".into())
                    } else {
                        Ok(())
                    };
                }
            }
            Message::Ping(payload) => ws
                .send(Message::Pong(payload))
                .await
                .map_err(|e| e.to_string())?,
            Message::Close(_) => return Err("DeepSeek TTS disconnected before finish".into()),
            _ => {}
        }
    }
}

async fn wait_for_cancel(token: &CancelToken) {
    while !token.is_cancelled() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tts::SessionSlot;

    #[test]
    fn missing_key_releases_session() {
        let backend = DeepSeekBackend::new();
        let slot = SessionSlot::default();
        let result = backend.start(TtsRequest::plain("hello".into()), slot.claim());
        assert!(result.is_err());
        assert!(!slot.is_active());
    }

    async fn serve(events: Vec<Message>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let text = ws.next().await.unwrap().unwrap();
            assert!(text.to_text().unwrap().contains("hello"));
            let end = ws.next().await.unwrap().unwrap();
            assert!(end.to_text().unwrap().contains("finish_text"));
            for event in events {
                ws.send(event).await.unwrap();
            }
        });
        url
    }

    #[tokio::test]
    async fn websocket_requires_successful_finish() {
        for (events, success) in [
            (
                vec![
                    Message::Binary(vec![0, 0]),
                    Message::Text(r#"{"event":"finish","code":"success"}"#.into()),
                ],
                true,
            ),
            (
                vec![
                    Message::Binary(vec![0, 0]),
                    Message::Text(
                        r#"{"event":"finish","code":"service_error","message":"broken"}"#.into(),
                    ),
                ],
                false,
            ),
            (
                vec![Message::Binary(vec![0, 0]), Message::Close(None)],
                false,
            ),
            (
                vec![Message::Text(
                    r#"{"event":"finish","code":"success"}"#.into(),
                )],
                false,
            ),
        ] {
            let url = serve(events).await;
            let slot = SessionSlot::default();
            let (tx, _rx) = mpsc::channel();
            assert_eq!(
                stream_piece_at(&url, "test", "stella", "hello", &tx, &slot.claim())
                    .await
                    .is_ok(),
                success
            );
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_waiting_stream() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ws.next().await;
            ws.next().await;
            tokio::time::sleep(Duration::from_secs(3)).await;
        });
        let slot = SessionSlot::default();
        let token = slot.claim();
        let cancelling = slot.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancelling.release();
        });
        let (tx, _rx) = mpsc::channel();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            stream_piece_at(&url, "test", "stella", "hello", &tx, &token),
        )
        .await;
        assert!(result.is_ok(), "cancellation must not wait for the server");
    }

    #[tokio::test]
    #[ignore = "requires a DeepSeek API key and network"]
    async fn live_websocket() {
        let key = std::env::var("DEEPSEEK_API_KEY").unwrap();
        let slot = SessionSlot::default();
        let (tx, rx) = mpsc::channel();
        stream_piece(
            &key,
            "stella",
            "这是 VoiceX 的语音合成测试。",
            &tx,
            &slot.claim(),
        )
        .await
        .unwrap();
        drop(tx);
        assert!(rx.iter().map(|bytes| bytes.len()).sum::<usize>() > 1000);
    }
}
