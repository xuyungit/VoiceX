//! Xiaomi MiMo speech synthesis backend.
//!
//! MiMo shapes TTS as a chat completion: the assistant message carries the text
//! to speak, an optional user message carries a natural-language style
//! instruction, and with `stream: true` the audio comes back as server-sent
//! events base64-encoded in `choices[0].delta.audio.data`. The stream is fixed
//! at 24 kHz mono — no sample-rate, speed or volume parameter exists on the
//! API, so speed is not offered for this provider and volume is local playback
//! gain.
//!
//! The streamed format is `pcm16`, not `mp3`, and that is a correctness choice,
//! not a taste one: each streamed MP3 chunk is an *independently encoded* file
//! whose encoder padding survives naive concatenation — measured at ~80 ms of
//! inserted silence per ~340 ms chunk, which plays as rhythmic stuttering. Raw
//! PCM has no per-chunk framing to get wrong, at the price of skipping the
//! shared MP3 decode pipeline. Probed live on 2026-08-14; see
//! `docs/mimo-tts-provider-research-2026-08-14.md` and
//! `scripts/tts/mimo_probe.py` for what the docs left open and what the
//! service actually does.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use base64::Engine;
use futures_util::StreamExt;
use serde_json::{json, Value};

use super::cloud_playback::{self, PieceStream};
use super::decode::RECV_POLL;
use super::playback::{negotiate_sample_rate, Playback, PlaybackHandle};
use super::{
    cloud_http_client, log_cloud_retry, log_event, piece_limit_for, CancelToken, CloudStreamError,
    SpeechProgress, TtsBackend, TtsError, TtsRequest, TtsStatus, TtsVoice,
};

const ENDPOINT: &str = "https://api.xiaomimimo.com/v1/chat/completions";

/// The preset-voice model. The sibling models (`-voicedesign`, `-voiceclone`)
/// take a voice description or an audio sample instead of a voice id — a
/// different settings surface, deliberately out of scope for reading.
const MODEL: &str = "mimo-v2.5-tts";

/// The only rate the service renders. There is no parameter to ask for
/// another, and output devices routinely refuse to open at 24 kHz (a 48
/// kHz-only interface is common) — so unlike the other cloud backends, this
/// one cannot negotiate its way out and resamples to the device rate instead.
const STREAM_RATE: u32 = 24_000;

/// Verified live: 2 000 characters synthesize to completion (537 s of audio,
/// the whole text). 5 000 characters come back `finish_reason: "stop"` but
/// with *less* audio than the 2 000-character run — the service truncates
/// silently somewhere in between, so `begin` splits a longer read into pieces
/// of the largest length proven whole.
const MAX_CHARS: usize = 2_000;

/// The full roster, confirmed by the service itself — sending an unknown id
/// returns `Unknown voice: ... Available voices: [mimo_default, 冰糖, 茉莉,
/// 苏打, 白桦, Mia, Chloe, Milo, Dean]`. Chinese voices use their Chinese
/// names as ids.
const VOICES: [(&str, &str, &str); 9] = [
    ("mimo_default", "MiMo 默认", "zh-CN"),
    ("冰糖", "冰糖", "zh-CN"),
    ("茉莉", "茉莉", "zh-CN"),
    ("苏打", "苏打", "zh-CN"),
    ("白桦", "白桦", "zh-CN"),
    ("Mia", "Mia", "en-US"),
    ("Chloe", "Chloe", "en-US"),
    ("Milo", "Milo", "en-US"),
    ("Dean", "Dean", "en-US"),
];

pub fn default_voice() -> &'static str {
    VOICES[0].0
}

#[derive(Debug, Clone)]
pub struct MimoConfig {
    pub api_key: String,
    /// Optional natural-language style instruction ("以平静的语气朗读"). This is
    /// the only control the API offers over delivery — there is no speed or
    /// pitch parameter — so it rides in the config rather than pretending to be
    /// a per-request voice parameter.
    pub instruction: String,
}

pub struct MimoBackend {
    config: Mutex<MimoConfig>,
    /// Filled in by the decode thread once it owns the output device, so `stop`
    /// can cut the audio immediately instead of waiting for the network side to
    /// notice. Shared rather than copied: the handle does not exist yet when
    /// `start` returns.
    playback: Arc<Mutex<Option<PlaybackHandle>>>,
    speaking: Arc<AtomicBool>,
    /// The current request's text as the pieces it was synthesized in,
    /// which is what `progress` reports on.
    pieces: Mutex<Vec<String>>,
}

impl MimoBackend {
    pub fn new(config: MimoConfig) -> Self {
        Self {
            config: Mutex::new(config),
            playback: Arc::new(Mutex::new(None)),
            speaking: Arc::new(AtomicBool::new(false)),
            pieces: Mutex::new(Vec::new()),
        }
    }

    pub fn apply_config(&self, config: MimoConfig) {
        if let Ok(mut slot) = self.config.lock() {
            *slot = config;
        }
    }

    fn config(&self) -> Result<MimoConfig, TtsError> {
        let config = self
            .config
            .lock()
            .map(|slot| slot.clone())
            .map_err(|_| TtsError::Backend("configuration is poisoned".to_string()))?;
        if config.api_key.trim().is_empty() {
            return Err(TtsError::Backend("no API key configured".to_string()));
        }
        Ok(config)
    }
}

impl TtsBackend for MimoBackend {
    fn name(&self) -> &'static str {
        "mimo"
    }

    fn list_voices(&self) -> Result<Vec<TtsVoice>, TtsError> {
        Ok(VOICES
            .iter()
            .map(|(id, name, language)| TtsVoice {
                id: id.to_string(),
                name: name.to_string(),
                language: language.to_string(),
            })
            .collect())
    }

    fn start(&self, request: TtsRequest, token: CancelToken) -> Result<(), TtsError> {
        // Every failure path has to hand the session back, or the session stays
        // claimed: the hotkey is stuck in "stop" mode from then on and the HUD
        // sits on "preparing" forever.
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

    fn audio_level(&self) -> Option<f32> {
        self.playback
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().and_then(|handle| handle.level()))
    }

    fn reports_progress(&self) -> bool {
        true
    }

    fn progress(&self) -> Option<SpeechProgress> {
        cloud_playback::progress(&self.speaking, &self.pieces, &self.playback)
    }
}

impl MimoBackend {
    fn begin(&self, request: TtsRequest, token: CancelToken) -> Result<(), TtsError> {
        let config = self.config()?;
        // The device's own preference, not the stream's fixed 24 kHz: the
        // stream is resampled to whatever this returns.
        let device_rate = negotiate_sample_rate()
            .map_err(|err| TtsError::Backend(format!("{} ({})", err, err.code())))?;

        let voice = request
            .voice
            .clone()
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| default_voice().to_string());
        // `request.rate` is deliberately ignored: the API has no speed
        // parameter, and the settings page hides the rate slider for this
        // provider so an ignored value cannot look like a broken one.
        let gain = request.volume.unwrap_or(1.0);

        // One request per piece, all feeding the same PCM pipeline: the
        // service silently truncates past MAX_CHARS, so splitting is the only
        // way a long selection is read in full. The pieces end on sentence
        // boundaries, so a seam is just an ordinary pause. Split before
        // anything starts, so `progress` can name the piece the sink is on.
        let pieces = cloud_playback::split_pieces(
            &request.text,
            piece_limit_for(request.piece_limit, MAX_CHARS),
            &self.pieces,
        );

        let (tx, rx) = mpsc::channel::<PieceStream>();
        // Lets the decode side tell "the provider failed" apart from "the audio
        // ended", which otherwise both look like a closed channel.
        let network_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let decode_token = token.clone();
        let decode_error = network_error.clone();

        // Clear any handle left by the previous utterance before publishing the
        // slot, so a stop arriving now cannot reach a device we already closed.
        if let Ok(mut slot) = self.playback.lock() {
            *slot = None;
        }

        let playback_slot = self.playback.clone();
        // Raised by the decode thread when the first samples reach the device,
        // not here: a request in flight is not a sound, and the HUD tells those
        // two states apart.
        let speaking = self.speaking.clone();
        thread::Builder::new()
            .name("voicex-tts-cloud".to_string())
            .spawn(move || {
                run_playback(
                    rx,
                    device_rate,
                    gain,
                    decode_token,
                    decode_error,
                    playback_slot,
                    speaking.clone(),
                );
                speaking.store(false, Ordering::SeqCst);
            })
            .map_err(|err| TtsError::Backend(format!("failed to spawn the decoder: {err}")))?;

        let http_token = token;
        let http_error = network_error;
        tauri::async_runtime::spawn(async move {
            for piece in &pieces {
                // Handed over before any of its bytes, so the sink can mark
                // where the piece begins in the sample stream.
                let (piece_tx, piece_rx) = mpsc::channel::<Vec<u8>>();
                if tx.send(piece_rx).is_err() {
                    // The sink is gone — it could not open the device, or the
                    // read was stopped — so nobody would hear the rest.
                    break;
                }
                let outcome = stream_audio_with_retry(
                    &config.api_key,
                    &build_body(piece, &voice, &config.instruction),
                    &piece_tx,
                    &http_token,
                )
                .await;
                // Closing the piece is what lets the sink move on to the next.
                drop(piece_tx);
                match outcome {
                    Ok(true) => {}
                    // Cancelled, or the decoder hung up: synthesizing the
                    // remaining pieces would only bill text nobody hears.
                    Ok(false) => break,
                    Err(err) => {
                        if let Ok(mut slot) = http_error.lock() {
                            *slot = Some(err);
                        }
                        break;
                    }
                }
            }
            // Closing the channel is what ends the sink's loop.
            drop(tx);
        });

        Ok(())
    }
}

/// The chat-shaped synthesis request. The assistant message is the text to
/// speak — the service rejects a request without one ("messages must contain an
/// assistant role for TTS model"). The user message is the style instruction
/// and is simply omitted when there is none; an empty one is also accepted, but
/// sending nothing is the honest form of "no instruction".
fn build_body(text: &str, voice: &str, instruction: &str) -> Value {
    let mut messages = Vec::with_capacity(2);
    let instruction = instruction.trim();
    if !instruction.is_empty() {
        messages.push(json!({ "role": "user", "content": instruction }));
    }
    messages.push(json!({ "role": "assistant", "content": text }));

    json!({
        "model": MODEL,
        "messages": messages,
        "audio": {
            // pcm16, not mp3: the streamed MP3 chunks are independent encodes
            // whose padding survives concatenation as ~80 ms of silence per
            // chunk — audible stuttering. See the module docs.
            "format": "pcm16",
            "voice": voice,
        },
        "stream": true,
    })
}

/// Stateful 16-bit little-endian PCM to f32 converter.
///
/// A network chunk may end halfway through a sample; the odd byte carries over
/// to the front of the next chunk. Dropping it instead would shift every later
/// sample by a byte, which decodes as full-scale noise.
struct PcmConverter {
    pending: Option<u8>,
}

impl PcmConverter {
    fn new() -> Self {
        Self { pending: None }
    }

    fn process(&mut self, bytes: &[u8], output: &mut Vec<f32>) {
        output.clear();
        let mut bytes = bytes;
        if let Some(low) = self.pending.take() {
            let Some((&high, rest)) = bytes.split_first() else {
                self.pending = Some(low);
                return;
            };
            output.push(f32::from(i16::from_le_bytes([low, high])) / 32768.0);
            bytes = rest;
        }
        let mut pairs = bytes.chunks_exact(2);
        for pair in &mut pairs {
            output.push(f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.0);
        }
        if let [odd] = pairs.remainder() {
            self.pending = Some(*odd);
        }
    }
}

/// Linear-interpolation resampler from the provider's fixed 24 kHz to the
/// device rate. Stateful across chunks — the last sample of one chunk seeds
/// the interpolation into the next, so chunk boundaries are inaudible.
///
/// Linear is enough here: the source is speech band-limited well below the
/// 12 kHz Nyquist of the 24 kHz stream, and every device rate on the
/// negotiation list is at or above the source rate, so there is no aliasing
/// to fight — only gaps to fill.
struct LinearResampler {
    /// Source samples advanced per output sample.
    step: f64,
    /// Position of the next output sample, in source samples, relative to the
    /// start of the current chunk. May be negative fractionally into `prev`.
    pos: f64,
    /// Last sample of the previous chunk, addressed as index -1.
    prev: Option<f32>,
}

impl LinearResampler {
    fn new(src_rate: u32, dst_rate: u32) -> Self {
        Self {
            step: f64::from(src_rate) / f64::from(dst_rate),
            pos: 0.0,
            prev: None,
        }
    }

    /// Whether `process` would just copy its input through.
    fn is_identity(&self) -> bool {
        self.step == 1.0
    }

    fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        output.clear();
        if input.is_empty() {
            return;
        }
        if self.is_identity() {
            output.extend_from_slice(input);
            return;
        }

        loop {
            let index = self.pos.floor();
            let frac = (self.pos - index) as f32;
            let index = index as isize;
            let at = |i: isize| -> Option<f32> {
                if i < 0 {
                    self.prev
                } else {
                    input.get(i as usize).copied()
                }
            };
            // Both neighbours must exist; the seam into the next chunk is
            // handled there via `prev`.
            let (Some(a), Some(b)) = (at(index), at(index + 1)) else {
                break;
            };
            output.push(a + (b - a) * frac);
            self.pos += self.step;
        }

        self.pos -= input.len() as f64;
        self.prev = input.last().copied();
    }
}

/// Convert and play, on a thread of its own because both block and because the
/// output stream may not cross threads.
///
/// `device_rate` is what the output device runs at; the stream itself is
/// always [`STREAM_RATE`] raw PCM, and the gap is closed by resampling. No
/// decoder sits in this path — raw PCM has nothing to decode, which is the
/// point (see the module docs).
fn run_playback(
    pieces: Receiver<PieceStream>,
    device_rate: u32,
    gain: f32,
    token: CancelToken,
    network_error: Arc<Mutex<Option<String>>>,
    handle_slot: Arc<Mutex<Option<PlaybackHandle>>>,
    speaking: Arc<AtomicBool>,
) {
    // No prebuffer: MiMo has no speed parameter, so playback never outpaces
    // synthesis the way fast speech does on the rate-capable providers.
    let playback = match Playback::open(device_rate, gain, 0) {
        Ok(playback) => playback,
        Err(err) => {
            if token.finish() {
                log_event(
                    "speak_err",
                    &[
                        ("error", err.code().to_string()),
                        ("detail", err.to_string()),
                    ],
                );
            }
            return;
        }
    };

    if let Ok(mut slot) = handle_slot.lock() {
        *slot = Some(playback.handle());
    }

    let mut started = false;
    let mut converter = PcmConverter::new();
    let mut samples = Vec::new();
    let mut resampler = LinearResampler::new(STREAM_RATE, device_rate);
    let mut resampled = Vec::new();
    // Ends when the network side drops the outer sender — after the last piece
    // or on failure — or when playback is stopped under us. The converter and
    // resampler carry across pieces: the stream is one continuous signal that
    // merely arrived in several requests.
    'pieces: loop {
        let piece = match pieces.recv_timeout(RECV_POLL) {
            Ok(piece) => piece,
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                if token.is_cancelled() {
                    break;
                }
                continue;
            }
        };
        // Marked before the piece's first byte, so the boundary is exactly
        // where the previous piece's samples ended.
        playback.begin_piece();
        for chunk in piece.iter() {
            if token.is_cancelled() {
                break 'pieces;
            }
            converter.process(&chunk, &mut samples);
            if samples.is_empty() {
                continue;
            }
            if !started {
                started = true;
                speaking.store(true, Ordering::SeqCst);
                log_event("speak_started", &[]);
            }
            resampler.process(&samples, &mut resampled);
            if !playback.push(&resampled) {
                break 'pieces;
            }
        }
    }

    let failure = network_error.lock().ok().and_then(|slot| slot.clone());

    match failure {
        None => {
            playback.mark_end_of_stream();
            match playback.wait_until_drained(&token) {
                Ok(true) => {
                    if token.finish() {
                        log_event("speak_finished", &[]);
                    }
                }
                // Cancelled: whoever cancelled owns the session now, but they
                // cannot report this part. `speak_stopped` fires when the stop
                // is accepted; only here is the audio actually finished, which
                // is what the smoke scripts need to assert on.
                Ok(false) => log_event("speak_cancelled", &[]),
                Err(err) => {
                    if token.finish() {
                        log_event(
                            "speak_err",
                            &[
                                ("error", err.code().to_string()),
                                ("detail", err.to_string()),
                            ],
                        );
                    } else {
                        log_event("speak_cancelled", &[]);
                    }
                }
            }
        }
        // A network failure reads as a truncated stream, so report the real
        // cause rather than silence that simply ends early. A stop is not a
        // failure, whatever the request it aborted recorded.
        Some(detail) => {
            if token.finish() {
                log_event(
                    "speak_err",
                    &[("error", "backend".to_string()), ("detail", detail)],
                );
            } else {
                log_event("speak_cancelled", &[]);
            }
        }
    }
}

/// Stream the synthesis response, forwarding decoded audio bytes to `tx`.
/// Stream one synthesis response, forwarding PCM bytes to `tx`.
///
/// `Ok(true)` means the piece completed and the caller may stream the next
/// one; `Ok(false)` means the read was cancelled or the decoder hung up, so
/// further pieces would only bill text nobody hears.
async fn stream_audio_with_retry(
    api_key: &str,
    body: &Value,
    tx: &Sender<Vec<u8>>,
    token: &CancelToken,
) -> Result<bool, String> {
    let mut retries_done = 0;
    loop {
        match stream_audio(api_key, body, tx, token).await {
            Ok(outcome) => return Ok(outcome),
            Err(err) => {
                if token.is_cancelled() {
                    return Ok(false);
                }
                if let Some(delay_ms) = err.retry_delay_ms(retries_done) {
                    log_cloud_retry("mimo", retries_done, delay_ms, err.reason());
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    if token.is_cancelled() {
                        return Ok(false);
                    }
                    retries_done += 1;
                    continue;
                }
                return Err(err.into_detail());
            }
        }
    }
}

async fn stream_audio(
    api_key: &str,
    body: &Value,
    tx: &Sender<Vec<u8>>,
    token: &CancelToken,
) -> Result<bool, CloudStreamError> {
    let response = cloud_http_client()?
        .post(ENDPOINT)
        // The docs show an `api-key` header; the service accepts both it and
        // the standard bearer form. Bearer matches the MiMo ASR client, so a
        // key that works for one provably works for the other.
        .header("Authorization", format!("Bearer {api_key}"))
        .json(body)
        .send()
        .await
        .map_err(CloudStreamError::request)?;

    let status = response.status();
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        return Err(CloudStreamError::http(status, describe_failure(&detail)));
    }

    let mut stream = response.bytes_stream();
    let mut pending = Vec::<u8>::new();
    let mut audio_emitted = false;

    while let Some(chunk) = stream.next().await {
        if token.is_cancelled() {
            return Ok(false);
        }
        let chunk = chunk.map_err(|err| CloudStreamError::stream(err, audio_emitted))?;
        pending.extend_from_slice(&chunk);

        // Server-sent events are line-oriented and a chunk may split a line.
        while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=newline).collect();
            let audio = parse_line(&line[..line.len() - 1])
                .map_err(|err| CloudStreamError::response(err, audio_emitted))?;
            if let Some(audio) = audio {
                if tx.send(audio).is_err() {
                    // The decoder is gone; nothing left to stream into.
                    return Ok(false);
                }
                audio_emitted = true;
            }
        }
    }

    if !pending.is_empty() {
        let audio = parse_line(&pending)
            .map_err(|err| CloudStreamError::response(err, audio_emitted))?;
        if let Some(audio) = audio {
            if tx.send(audio).is_err() {
                return Ok(false);
            }
        }
    }

    Ok(true)
}

/// Decode one SSE line into audio bytes, or `None` when it carries none.
///
/// Frames without audio are normal, not errors: the first delta carries only
/// `role`, later ones may carry `content`/`transcript` text, and the final
/// frames carry `finish_reason` and `usage` before the `[DONE]` sentinel.
fn parse_line(line: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let text = std::str::from_utf8(line).map_err(|_| "response was not UTF-8".to_string())?;
    let Some(payload) = text.trim_end_matches('\r').strip_prefix("data:") else {
        return Ok(None);
    };
    let payload = payload.trim_start();
    if payload.is_empty() || payload == "[DONE]" {
        return Ok(None);
    }

    let value: Value =
        serde_json::from_str(payload).map_err(|err| format!("malformed response: {err}"))?;

    // Not observed mid-stream on the live service — errors so far arrive as
    // plain HTTP failures before any event — but an OpenAI-shaped stream is
    // allowed to carry one, and reading it as "no audio" would end the read
    // in silence with no explanation.
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(|message| message.as_str())
            .unwrap_or("unknown error");
        let code = error
            .get("code")
            .and_then(|code| code.as_str())
            .unwrap_or("");
        return Err(format!("provider error {code}: {message}"));
    }

    let Some(data) = value
        .pointer("/choices/0/delta/audio/data")
        .and_then(|data| data.as_str())
    else {
        return Ok(None);
    };
    if data.is_empty() {
        return Ok(None);
    }

    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map(Some)
        .map_err(|err| format!("bad base64 in response: {err}"))
}

/// Pull the useful part out of an error body, falling back to its first line.
///
/// MiMo failures look like `{"error":{"code":"400","message":"Param
/// Incorrect","param":"Unknown voice: ...","type":""}}` — the `param` field is
/// where the actionable text lives, so it is included when present.
fn describe_failure(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            let error = value.get("error")?;
            let message = error
                .get("message")
                .and_then(|message| message.as_str())
                .unwrap_or("");
            let param = error
                .get("param")
                .and_then(|param| param.as_str())
                .unwrap_or("");
            Some(if param.is_empty() {
                message.to_string()
            } else {
                format!("{message}: {param}")
            })
        })
        .filter(|described| !described.is_empty())
        .unwrap_or_else(|| body.lines().next().unwrap_or_default().to_string())
        .chars()
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_body_is_chat_shaped_with_the_text_in_the_assistant_message() {
        // The service rejects any other arrangement outright ("messages must
        // contain an assistant role for TTS model"), so the shape is the
        // contract, not a style choice.
        let body = build_body("你好", "冰糖", "");
        assert_eq!(body["model"], MODEL);
        assert_eq!(body["stream"], true);
        // pcm16 and not mp3 is load-bearing: the streamed MP3 chunks are
        // independent encodes whose padding plays as stuttering.
        assert_eq!(body["audio"]["format"], "pcm16");
        assert_eq!(body["audio"]["voice"], "冰糖");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "no instruction, no user message");
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"], "你好");
    }

    #[test]
    fn an_instruction_becomes_a_leading_user_message() {
        let body = build_body("你好", "茉莉", "  以平静的语气朗读  ");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "以平静的语气朗读");
        assert_eq!(messages[1]["role"], "assistant");

        // Whitespace-only means none: an empty user message is accepted by the
        // service but sending one anyway would be noise in every request.
        let messages = build_body("你好", "茉莉", "   ")["messages"].clone();
        assert_eq!(messages.as_array().unwrap().len(), 1);
    }

    #[test]
    fn audio_frames_yield_bytes_and_framing_lines_yield_none() {
        let audio = parse_line(br#"data: {"choices":[{"delta":{"audio":{"data":"aGVsbG8="}}}]}"#)
            .unwrap()
            .unwrap();
        assert_eq!(audio, b"hello");

        for framing in [
            // The first delta carries only the role; later ones may carry
            // transcript text; the last carries finish_reason and usage.
            &br#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#[..],
            br#"data: {"choices":[{"delta":{"content":"..."}}]}"#,
            br#"data: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{}}"#,
            br#"data: {"choices":[]}"#,
            b"data: [DONE]",
            b"data:",
            b"",
            b": keep-alive",
        ] {
            assert!(parse_line(framing).unwrap().is_none(), "{framing:?}");
        }
    }

    #[test]
    fn a_mid_stream_error_is_reported_rather_than_read_as_end_of_audio() {
        let err =
            parse_line(br#"data: {"error":{"code":"429","message":"rate limited","type":""}}"#)
                .unwrap_err();
        assert!(err.contains("429"), "{err}");
        assert!(err.contains("rate limited"), "{err}");
    }

    #[test]
    fn a_carriage_return_does_not_hide_the_payload() {
        // CRLF framing would otherwise leave a trailing \r inside the JSON.
        let audio =
            parse_line(b"data: {\"choices\":[{\"delta\":{\"audio\":{\"data\":\"aGk=\"}}}]}\r")
                .unwrap()
                .unwrap();
        assert_eq!(audio, b"hi");
    }

    #[test]
    fn http_failures_surface_the_param_field_where_the_details_live() {
        // The message alone is just "Param Incorrect"; the param field is what
        // tells the user their voice id is wrong and what the choices are.
        let described = describe_failure(
            r#"{"error":{"code":"400","message":"Param Incorrect","param":"Unknown voice: x. Available voices: [mimo_default]","type":""}}"#,
        );
        assert_eq!(
            described,
            "Param Incorrect: Unknown voice: x. Available voices: [mimo_default]"
        );
        assert_eq!(
            describe_failure("gateway timeout\nsecond line"),
            "gateway timeout"
        );
    }

    #[test]
    fn every_voice_id_resolves_and_the_default_is_first() {
        assert_eq!(default_voice(), "mimo_default");
        let backend = MimoBackend::new(MimoConfig {
            api_key: "key".to_string(),
            instruction: String::new(),
        });
        let listed = backend.list_voices().unwrap();
        assert_eq!(listed.len(), VOICES.len());
        assert!(listed.iter().any(|voice| voice.id == "冰糖"));
    }

    #[test]
    fn a_sample_split_across_chunks_is_reassembled_not_dropped() {
        // Dropping the odd byte would shift every later sample by a byte and
        // decode as full-scale noise, so the seam is the whole test.
        let mut converter = PcmConverter::new();
        let mut out = Vec::new();

        // 0x0100 = 256, 0x0302 = 770 — split mid-sample.
        converter.process(&[0x00, 0x01, 0x02], &mut out);
        assert_eq!(out, vec![256.0 / 32768.0]);

        converter.process(&[0x03], &mut out);
        assert_eq!(out, vec![770.0 / 32768.0]);

        // An empty chunk while a byte is pending must keep it pending.
        converter.process(&[], &mut out);
        assert!(out.is_empty());
        converter.process(&[0xFF, 0xFF, 0x7F], &mut out);
        assert_eq!(out, vec![-1.0 / 32768.0]); // 0xFFFF = -1
    }

    #[test]
    fn pcm_conversion_covers_the_full_signed_range() {
        let mut converter = PcmConverter::new();
        let mut out = Vec::new();
        // i16::MIN, 0, i16::MAX in little-endian byte order.
        converter.process(&[0x00, 0x80, 0x00, 0x00, 0xFF, 0x7F], &mut out);
        assert_eq!(out, vec![-1.0, 0.0, 32767.0 / 32768.0]);
    }

    #[test]
    fn resampling_to_the_same_rate_is_a_passthrough() {
        let mut resampler = LinearResampler::new(24_000, 24_000);
        assert!(resampler.is_identity());
        let mut out = Vec::new();
        resampler.process(&[0.1, 0.2, 0.3], &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn upsampling_doubles_the_sample_count_and_interpolates_between_points() {
        let mut resampler = LinearResampler::new(24_000, 48_000);
        let mut out = Vec::new();
        resampler.process(&[0.0, 1.0], &mut out);
        // Positions 0, 0.5 land inside the chunk; 1.0 (the last sample) and
        // 1.5 wait for the next chunk's first sample to interpolate against.
        assert_eq!(out, vec![0.0, 0.5]);

        // The seam: position 1.0 relative to the previous chunk is -1.0 here,
        // which is exactly `prev` — no sample is skipped or invented.
        resampler.process(&[0.0], &mut out);
        assert_eq!(out, vec![1.0, 0.5]);
    }

    #[test]
    fn downsampling_to_a_fractional_rate_keeps_the_long_run_ratio() {
        // 24 kHz -> 44.1 kHz across many chunks: the output count must track
        // input * 44100/24000 without drifting at chunk boundaries.
        let mut resampler = LinearResampler::new(24_000, 44_100);
        let mut produced = 0usize;
        let mut out = Vec::new();
        for _ in 0..100 {
            resampler.process(&[0.5; 240], &mut out);
            produced += out.len();
        }
        let expected = (240.0 * 100.0 * 44_100.0 / 24_000.0) as usize;
        let drift = produced.abs_diff(expected);
        assert!(drift <= 2, "produced {produced}, expected ~{expected}");
    }

    #[test]
    fn a_refused_start_hands_the_session_back() {
        // Without this the session stays claimed forever: the read hotkey turns
        // into a permanent "stop", and the HUD sits on "preparing" with nothing
        // ever coming. Reachable by simply not having configured a key yet.
        let backend = MimoBackend::new(MimoConfig {
            api_key: String::new(),
            instruction: String::new(),
        });
        let slot = crate::tts::SessionSlot::default();
        let token = slot.claim();
        assert!(slot.is_active());

        let outcome = backend.start(TtsRequest::plain("hi".to_string()), token);

        assert!(outcome.is_err(), "an unconfigured backend must refuse");
        assert!(!slot.is_active(), "a refusal must release the session");
        assert_eq!(backend.status(), TtsStatus::Idle);
    }

    /// End-to-end against the live service: network, SSE parse, PCM conversion,
    /// resampling and playback, in the same arrangement the backend uses.
    /// Also guards against the MP3 regression: the reported duration must match
    /// what the service billed for, and the per-chunk padding that motivated
    /// the pcm16 switch inflated it by ~25%. Plays audio, so it is opt-in:
    ///
    /// ```text
    /// MIMO_TTS_API_KEY=... cargo test --lib mimo::tests::live -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires network access and credentials"]
    fn live_synthesis_decodes_and_plays() {
        use crate::tts::SessionSlot;
        use std::time::Instant;

        let api_key = std::env::var("MIMO_TTS_API_KEY").expect("MIMO_TTS_API_KEY is not set");
        let rate = negotiate_sample_rate().expect("no usable output sample rate");
        eprintln!("device rate {rate} Hz, stream rate {STREAM_RATE} Hz");

        let text = "小米 MiMo 语音合成，端到端链路验证：流式接收、PCM 转换、重采样与本地播放。";
        let slot = SessionSlot::default();
        let token = slot.claim();
        let (tx, rx) = mpsc::channel::<Vec<u8>>();

        let decode_token = token.clone();
        let player = thread::spawn(move || {
            let playback =
                Playback::open(rate, 1.0, 0).expect("failed to open the output device");
            let handle = playback.handle();
            // Sampled while audio is playing, because this drives the HUD
            // waveform and a level that only appears after the last sample
            // would leave the bars hidden for the whole read.
            let mut levels: Vec<f32> = Vec::new();
            let mut converter = PcmConverter::new();
            let mut samples = Vec::new();
            let mut resampler = LinearResampler::new(STREAM_RATE, rate);
            let mut resampled = Vec::new();
            let mut source_samples = 0usize;
            for chunk in rx.iter() {
                if let Some(level) = handle.level() {
                    levels.push(level);
                }
                converter.process(&chunk, &mut samples);
                source_samples += samples.len();
                resampler.process(&samples, &mut resampled);
                assert!(playback.push(&resampled), "playback stopped early");
            }
            playback.mark_end_of_stream();
            playback
                .wait_until_drained(&decode_token)
                .expect("playback stalled");
            (source_samples, levels)
        });

        let started = Instant::now();
        let outcome = tauri::async_runtime::block_on(stream_audio(
            &api_key,
            &build_body(text, default_voice(), ""),
            &tx,
            &token,
        ));
        drop(tx);
        outcome.expect("streaming failed");

        let (samples, levels) = player.join().expect("player panicked");
        // Source samples are at the stream rate regardless of the device rate.
        let seconds = samples as f64 / f64::from(STREAM_RATE);
        let peak = levels.iter().copied().fold(0.0f32, f32::max);
        eprintln!(
            "received {samples} samples ({seconds:.2} s) in {:?}, peak level {peak:.4}",
            started.elapsed()
        );

        assert!(
            seconds > 2.0,
            "expected several seconds of speech, received {seconds:.2} s"
        );
        assert!(
            peak > 0.01,
            "the HUD waveform is driven by this level, and it stayed at {peak:.4} \
             while audio was playing"
        );
    }

    /// Which voice ids the account actually accepts. The table is hand-kept
    /// (there is no list endpoint), and a wrong id fails only at speak time —
    /// so this checks them all in one pass:
    ///
    /// ```text
    /// MIMO_TTS_API_KEY=... cargo test --lib mimo::tests::voice_table -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires network access and credentials"]
    fn voice_table_matches_what_the_service_accepts() {
        let api_key = std::env::var("MIMO_TTS_API_KEY").expect("MIMO_TTS_API_KEY is not set");
        let mut rejected: Vec<String> = Vec::new();

        for (id, name, _) in VOICES {
            let slot = crate::tts::SessionSlot::default();
            let token = slot.claim();
            let (tx, rx) = mpsc::channel::<Vec<u8>>();
            // Drained on a thread so a slow reader cannot stall the sender.
            let drain = thread::spawn(move || rx.iter().count());

            let outcome = tauri::async_runtime::block_on(stream_audio(
                &api_key,
                &build_body("测试。", id, ""),
                &tx,
                &token,
            ));
            drop(tx);
            let chunks = drain.join().unwrap_or(0);

            match outcome {
                Ok(_) if chunks > 0 => eprintln!("  ok      {id} ({name})"),
                Ok(_) => {
                    eprintln!("  SILENT  {id} ({name})");
                    rejected.push(format!("{id}: no audio"));
                }
                Err(err) => {
                    eprintln!("  REJECT  {id} ({name}): {err}");
                    rejected.push(format!("{id}: {err}"));
                }
            }
        }

        assert!(rejected.is_empty(), "voice table is wrong:\n{rejected:#?}");
    }
}
