//! Volcengine (ByteDance Doubao) Seed-TTS 2.0 backend.
//!
//! Uses the unidirectional streaming HTTP interface. Plan §5.4 records why that
//! one and not the two WebSocket variants: the text is fully known before the
//! request goes out, so bidirectional streaming solves a problem we do not have,
//! and a WebSocket buys nothing over HTTP here except a connection lifecycle to
//! manage.
//!
//! Audio arrives as base64 MP3 in many small chunks. Three stages run
//! concurrently — network, decode, playback — because the whole point is to
//! start speaking at the first chunk rather than after the last one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use base64::Engine;
use futures_util::StreamExt;
use serde::Serialize;

use super::cloud_playback::{self, PieceStream};
use super::playback::{negotiate_sample_rate, prebuffer_samples, PlaybackHandle};
use super::{
    cloud_http_client, log_cloud_retry, piece_limit_for, CancelToken, CloudStreamError,
    SpeechProgress, TtsBackend, TtsError, TtsRequest, TtsStatus, TtsVoice,
};

const ENDPOINT: &str = "https://openspeech.bytedance.com/api/v3/tts/unidirectional";

/// Default resource id. Note this is the literal model string, **not** the
/// instance id shown in the console — passing the instance id returns
/// `45000030 requested resource not granted` (plan §5.4).
pub const DEFAULT_RESOURCE_ID: &str = "seed-tts-2.0";

/// Terminal status code the provider sends as the last chunk.
const CODE_FINISHED: i64 = 20000000;

/// Longest text sent in one request (plan §4.5). The provider rejects
/// oversized text with `40402003`, so the caller trims instead of spending a
/// request to be told no.
///
/// No sentence-level chunking accompanies it: the plan assumed that would be
/// needed for latency, but the interface streams — first audio arrives in
/// 282-621 ms regardless of length (§5.4) — so splitting would add failure
/// modes and buy nothing.
const MAX_CHARS: usize = 5_000;

/// Voices verified against Seed-TTS 2.0. The provider exposes no working
/// speaker-list endpoint (plan §5.4), so this is a built-in allow-list; the
/// settings page also accepts a hand-typed id for anything else the account has.
const KNOWN_SPEAKERS: [(&str, &str, &str); 2] = [
    ("zh_female_vv_uranus_bigtts", "Vivi", "zh-CN"),
    ("zh_male_liufei_uranus_bigtts", "刘飞", "zh-CN"),
];

pub fn default_speaker() -> &'static str {
    KNOWN_SPEAKERS[0].0
}

#[derive(Debug, Clone)]
pub struct VolcengineConfig {
    pub api_key: String,
    pub resource_id: String,
}

#[derive(Serialize)]
struct RequestBody<'a> {
    user: User<'a>,
    req_params: ReqParams<'a>,
}

#[derive(Serialize)]
struct User<'a> {
    uid: &'a str,
}

#[derive(Serialize)]
struct ReqParams<'a> {
    text: &'a str,
    speaker: &'a str,
    audio_params: AudioParams,
}

#[derive(Serialize)]
struct AudioParams {
    format: &'static str,
    sample_rate: u32,
    speech_rate: i32,
    loudness_rate: i32,
}

/// Convert the stored 0.0..=1.0 rate into the provider's own scale.
///
/// The stored value is normalized around the macOS engine default of 0.5,
/// which the settings UI shows as 1x on a 0.5x–2x slider. Volcengine takes
/// 0 as neutral over a measured usable range of -50..=100 (plan §5.4), so the
/// two scales line up on a single linear mapping: 0.5x → -50, 1x → 0, 2x → 100.
fn speech_rate_from_normalized(rate: Option<f32>) -> i32 {
    let Some(rate) = rate else { return 0 };
    let multiplier = (rate / 0.5).clamp(0.5, 2.0);
    (((multiplier - 1.0) * 100.0).round() as i32).clamp(-50, 100)
}

pub struct VolcengineBackend {
    config: Mutex<VolcengineConfig>,
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

impl VolcengineBackend {
    pub fn new(config: VolcengineConfig) -> Self {
        Self {
            config: Mutex::new(config),
            playback: Arc::new(Mutex::new(None)),
            speaking: Arc::new(AtomicBool::new(false)),
            pieces: Mutex::new(Vec::new()),
        }
    }

    pub fn apply_config(&self, config: VolcengineConfig) {
        if let Ok(mut slot) = self.config.lock() {
            *slot = config;
        }
    }

    fn config(&self) -> Result<VolcengineConfig, TtsError> {
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

impl TtsBackend for VolcengineBackend {
    fn name(&self) -> &'static str {
        "volcengine"
    }

    fn list_voices(&self) -> Result<Vec<TtsVoice>, TtsError> {
        Ok(KNOWN_SPEAKERS
            .iter()
            .map(|(id, name, language)| TtsVoice {
                id: id.to_string(),
                name: name.to_string(),
                language: language.to_string(),
            })
            .collect())
    }

    fn start(&self, request: TtsRequest, token: CancelToken) -> Result<(), TtsError> {
        // Every failure path has to hand the session back. The trait requires
        // it, and the cost of missing one is not subtle: the session stays
        // claimed, so the hotkey is stuck in "stop" mode from then on and the
        // HUD shows "preparing" forever. The prologue below has three fallible
        // steps, which is why this wraps rather than repeating `finish` at each.
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

impl VolcengineBackend {
    fn begin(&self, request: TtsRequest, token: CancelToken) -> Result<(), TtsError> {
        let config = self.config()?;
        let sample_rate = negotiate_sample_rate()
            .map_err(|err| TtsError::Backend(format!("{} ({})", err, err.code())))?;

        let speaker = request
            .voice
            .clone()
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| default_speaker().to_string());
        // Volcengine has no pitch parameter; the settings page hides that row
        // for cloud providers (plan §5.4).
        let speech_rate = speech_rate_from_normalized(request.rate);
        let gain = request.volume.unwrap_or(1.0);
        // The provider's -50..=100 scale is linear in the 0.5x-2x multiplier,
        // so this recovers the multiplier the playback policy is written in.
        let prebuffer = prebuffer_samples(1.0 + speech_rate as f32 / 100.0, sample_rate);

        // One request per piece, all feeding the same sink: the service caps
        // a request, not a read, and the pieces end on sentence boundaries,
        // so a seam is audible only as an ordinary pause. Split before
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
        // two states apart — the 300-700 ms between them is what it shows.
        let speaking = self.speaking.clone();
        thread::Builder::new()
            .name("voicex-tts-cloud".to_string())
            .spawn(move || {
                cloud_playback::run_playback(
                    rx,
                    sample_rate,
                    gain,
                    prebuffer,
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
                    &config,
                    piece,
                    &speaker,
                    sample_rate,
                    speech_rate,
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

/// Stream one synthesis response, forwarding decoded audio bytes to `tx`.
///
/// `Ok(true)` means the piece completed and the caller may stream the next
/// one; `Ok(false)` means the read was cancelled or the decoder hung up, so
/// further pieces would only bill text nobody hears.
#[allow(clippy::too_many_arguments)]
async fn stream_audio_with_retry(
    config: &VolcengineConfig,
    text: &str,
    speaker: &str,
    sample_rate: u32,
    speech_rate: i32,
    tx: &Sender<Vec<u8>>,
    token: &CancelToken,
) -> Result<bool, String> {
    let mut retries_done = 0;
    loop {
        match stream_audio(
            config,
            text,
            speaker,
            sample_rate,
            speech_rate,
            tx,
            token,
        )
        .await
        {
            Ok(outcome) => return Ok(outcome),
            Err(err) => {
                if token.is_cancelled() {
                    return Ok(false);
                }
                if let Some(delay_ms) = err.retry_delay_ms(retries_done) {
                    log_cloud_retry("volcengine", retries_done, delay_ms, err.reason());
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

#[allow(clippy::too_many_arguments)]
async fn stream_audio(
    config: &VolcengineConfig,
    text: &str,
    speaker: &str,
    sample_rate: u32,
    speech_rate: i32,
    tx: &Sender<Vec<u8>>,
    token: &CancelToken,
) -> Result<bool, CloudStreamError> {
    let body = RequestBody {
        user: User { uid: "voicex" },
        req_params: ReqParams {
            text,
            speaker,
            audio_params: AudioParams {
                format: "mp3",
                sample_rate,
                speech_rate,
                loudness_rate: 0,
            },
        },
    };

    let response = cloud_http_client()?
        .post(ENDPOINT)
        .header("X-Api-Key", &config.api_key)
        .header("X-Api-Resource-Id", &config.resource_id)
        .header("X-Api-Request-Id", uuid::Uuid::new_v4().to_string())
        .json(&body)
        .send()
        .await
        .map_err(CloudStreamError::request)?;

    let status = response.status();
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        return Err(CloudStreamError::http(status, first_line(&detail)));
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

        // The response is newline-delimited JSON; a chunk may split a line.
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

/// Decode one response line into audio bytes, or `None` when it carries none.
fn parse_line(line: &[u8]) -> Result<Option<Vec<u8>>, String> {
    if line.iter().all(|byte| byte.is_ascii_whitespace()) {
        return Ok(None);
    }

    let value: serde_json::Value =
        serde_json::from_slice(line).map_err(|err| format!("malformed response: {err}"))?;

    // The provider nests errors under `header` and successes at the top level.
    let node = value.get("header").unwrap_or(&value);
    let code = node.get("code").and_then(|code| code.as_i64()).unwrap_or(0);
    if code != 0 && code != CODE_FINISHED {
        let message = node
            .get("message")
            .and_then(|message| message.as_str())
            .unwrap_or("unknown error");
        return Err(format!("provider error {code}: {message}"));
    }

    let Some(data) = value.get("data").and_then(|data| data.as_str()) else {
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

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rate_scales_line_up_at_both_ends_and_the_middle() {
        // Stored 0..1 around a 0.5 default, shown as 0.5x–2x, sent as -50..100.
        // Getting this wrong is silent: the voice just speaks at the wrong speed.
        assert_eq!(speech_rate_from_normalized(Some(0.25)), -50, "0.5x");
        assert_eq!(speech_rate_from_normalized(Some(0.5)), 0, "1x is neutral");
        assert_eq!(speech_rate_from_normalized(Some(1.0)), 100, "2x");
        assert_eq!(speech_rate_from_normalized(None), 0, "unset is neutral");
    }

    #[test]
    fn rates_outside_the_slider_are_clamped_to_what_the_provider_accepts() {
        // Measured limits: below -50 and above 100 the provider stops changing.
        assert_eq!(speech_rate_from_normalized(Some(0.0)), -50);
        assert_eq!(speech_rate_from_normalized(Some(5.0)), 100);
    }

    #[test]
    fn audio_lines_yield_bytes_and_the_terminal_line_yields_none() {
        let audio = parse_line(br#"{"code":0,"message":"","data":"aGVsbG8="}"#)
            .unwrap()
            .unwrap();
        assert_eq!(audio, b"hello");

        let done = parse_line(br#"{"code":20000000,"message":"OK","data":null}"#).unwrap();
        assert!(done.is_none(), "the terminal line carries no audio");

        assert!(parse_line(b"   ").unwrap().is_none());
    }

    #[test]
    fn provider_errors_are_reported_from_both_shapes_they_arrive_in() {
        // Successes come back flat; failures come nested under `header`.
        let flat = parse_line(
            br#"{"reqid":"","code":55000000,"message":"resource ID is mismatched with speaker related resource"}"#,
        )
        .unwrap_err();
        assert!(flat.contains("55000000"), "{flat}");

        let nested = parse_line(
            br#"{"header":{"reqid":"x","code":45000030,"message":"requested resource not granted"}}"#,
        )
        .unwrap_err();
        assert!(nested.contains("45000030"), "{nested}");
    }

    /// End-to-end against the live service: network, streaming parse, MP3
    /// decode and playback, in the same arrangement the backend uses. Plays
    /// audio, so it is opt-in:
    ///
    /// ```text
    /// VOLC_TTS_API_KEY=... cargo test --lib volcengine::tests::live -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires network access and credentials"]
    fn live_synthesis_decodes_and_plays() {
        use crate::tts::decode::{decode_mp3_stream, ChunkSource};
        use crate::tts::playback::Playback;
        use crate::tts::SessionSlot;
        use std::time::Instant;

        let api_key = std::env::var("VOLC_TTS_API_KEY").expect("VOLC_TTS_API_KEY is not set");
        let config = VolcengineConfig {
            api_key,
            resource_id: DEFAULT_RESOURCE_ID.to_string(),
        };
        let rate = negotiate_sample_rate().expect("no usable output sample rate");
        eprintln!("negotiated sample rate: {rate} Hz");

        let text = "火山引擎语音合成，端到端链路验证：网络流式接收、MP3 解码与本地播放。";
        let slot = SessionSlot::default();
        let token = slot.claim();
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let source = ChunkSource::new(rx, token.clone());

        let decode_token = token.clone();
        let decoder = thread::spawn(move || {
            let playback =
                Playback::open(rate, 1.0, 0).expect("failed to open the output device");
            let handle = playback.handle();
            // Sampled while audio is actually playing, because this is what
            // drives the HUD waveform and a level that only appears after the
            // last sample would leave the bars hidden for the whole read.
            let mut levels: Vec<f32> = Vec::new();
            let samples = decode_mp3_stream(source, rate, |chunk| {
                if let Some(level) = handle.level() {
                    levels.push(level);
                }
                playback.push(chunk)
            })
            .expect("decode failed");
            playback.mark_end_of_stream();
            playback
                .wait_until_drained(&decode_token)
                .expect("playback stalled");
            (samples, levels)
        });

        let started = Instant::now();
        let outcome = tauri::async_runtime::block_on(stream_audio(
            &config,
            text,
            default_speaker(),
            rate,
            0,
            &tx,
            &token,
        ));
        drop(tx);
        outcome.expect("streaming failed");

        let (samples, mut levels) = decoder.join().expect("decoder panicked");
        let seconds = samples as f64 / rate as f64;
        let peak_level = levels.iter().copied().fold(0.0f32, f32::max);
        let mean_level = levels.iter().sum::<f32>() / levels.len().max(1) as f32;
        levels.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = levels.get(levels.len() / 2).copied().unwrap_or(0.0);
        let p90 = levels.get(levels.len() * 9 / 10).copied().unwrap_or(0.0);
        eprintln!(
            "decoded {samples} samples ({seconds:.2} s) in {:?}",
            started.elapsed()
        );
        // Printed so the HUD's display gain can be calibrated against real
        // speech instead of guessed. The bars saturate above roughly 0.024
        // under the microphone's gain, which is why reading needs its own.
        eprintln!(
            "output RMS over {} readings: median {median:.4}  mean {mean_level:.4}  \
             p90 {p90:.4}  peak {peak_level:.4}",
            levels.len()
        );

        assert!(
            seconds > 2.0,
            "expected several seconds of speech, decoded {seconds:.2} s"
        );
        assert!(
            peak_level > 0.01,
            "the HUD waveform is driven by this level, and it stayed at {peak_level:.4} \
             while audio was playing"
        );
    }

    #[test]
    fn a_refused_start_hands_the_session_back() {
        // Without this the session stays claimed forever: the read hotkey turns
        // into a permanent "stop", and the HUD sits on "preparing" with nothing
        // ever coming. Reachable by simply not having configured a key yet.
        let backend = VolcengineBackend::new(VolcengineConfig {
            api_key: String::new(),
            resource_id: DEFAULT_RESOURCE_ID.to_string(),
        });
        let slot = crate::tts::SessionSlot::default();
        let token = slot.claim();
        assert!(slot.is_active());

        let outcome = backend.start(TtsRequest::plain("hi".to_string()), token);

        assert!(outcome.is_err(), "an unconfigured backend must refuse");
        assert!(!slot.is_active(), "a refusal must release the session");
        assert_eq!(backend.status(), TtsStatus::Idle);
    }

    #[test]
    fn the_known_speakers_are_seed_tts_2_voices() {
        // The classic *_moon_bigtts / *_mars_bigtts voices share nothing with
        // Seed-TTS 2.0 and fail with code 55000000 (plan §5.4).
        for (id, _, _) in KNOWN_SPEAKERS {
            assert!(
                id.ends_with("_uranus_bigtts"),
                "{id} is not a Seed-TTS 2.0 voice"
            );
        }
    }
}
