//! Azure Speech (Microsoft Cognitive Services) speech synthesis backend.
//!
//! One REST call per piece: POST SSML to the region's `/cognitiveservices/v1`
//! endpoint and the response body *is* the MP3, streamed as chunked transfer
//! encoding — no SSE framing, no base64, no mid-stream error frames. Failures
//! are ordinary HTTP status codes before any audio byte arrives, which makes
//! this the simplest of the cloud providers.
//!
//! Probed live on 2026-08-31 from this account (eastus): first byte in
//! 0.8–1.1 s, standard voices only (the Dragon HD tier costs ~40% more and is
//! deliberately not listed). The voice table is the curated audition set from
//! that session; the picker's typed-id path reaches the rest of the ~70 zh-CN
//! voices, and `/cognitiveservices/voices/list` exists if a live list is ever
//! worth a settings-page roundtrip.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use futures_util::StreamExt;

use super::decode::{decode_mp3_stream, ChunkSource};
use super::playback::{
    negotiate_sample_rate_among, prebuffer_samples, Playback, PlaybackHandle,
};
use super::{
    cloud_http_client, log_cloud_retry, log_event, split_for_backend, CancelToken,
    CloudStreamError, TtsBackend, TtsError, TtsRequest, TtsStatus, TtsVoice,
};

/// Speech resources are bound to one region and the key only works there, so
/// the region is configuration, not a constant. This is the fallback for an
/// unset field, matching the settings default.
pub const DEFAULT_REGION: &str = "eastus";

/// Rates the service renders as MP3, best first. Each maps to a named output
/// format below; the playback module picks whichever the output device likes.
const SAMPLE_RATES: [u32; 3] = [48_000, 24_000, 16_000];

/// The service caps a request at 10 minutes of *produced audio*, not at a
/// character count, and silently truncates past it: HTTP 200 and exactly
/// 600.000 s of MP3. Measured 2026-08-31 — 3268 characters at 1x and 2150 at
/// 0.5x both came back truncated on the nose, so the cap is audio time and
/// slow speech spends it faster per character. Dense Chinese prose runs ~260
/// characters a minute at 1x; 2000 keeps a piece at ~7.7 minutes there, and
/// [`piece_chars_for`] shrinks the budget with the rate below 1x so every
/// slider position keeps the same margin.
const PIECE_CHARS: usize = 2_000;

/// How much text one request may carry at `speed`.
///
/// Slower speech produces more audio per character, and the 10-minute cap is
/// on audio (see [`PIECE_CHARS`]) — so the budget scales down with the
/// multiplier below 1x. Faster speech would stretch it, but 7.7 minutes of
/// margin at 1x is already comfortable and multiplying the budget up would
/// trade a tested bound for an untested one.
fn piece_chars_for(speed: f32) -> usize {
    (PIECE_CHARS as f32 * speed.clamp(0.5, 1.0)) as usize
}

/// The audition set picked on 2026-08-31 (docs/azure-tts research session):
/// every popular standard Neural voice plus the Multilingual ones, which read
/// mixed Chinese-English selections without the stilted letter-by-letter
/// English of the plain zh-CN voices — and cost the same since late 2023.
/// Dialect and HD voices are reachable by typing their id into the picker.
const VOICES: [(&str, &str, &str); 12] = [
    ("zh-CN-XiaoyuMultilingualNeural", "晓宇（多语言）", "zh-CN"),
    ("zh-CN-XiaoxiaoMultilingualNeural", "晓晓（多语言）", "zh-CN"),
    ("zh-CN-YunfanMultilingualNeural", "云帆（多语言）", "zh-CN"),
    ("zh-CN-XiaoxiaoNeural", "晓晓", "zh-CN"),
    ("zh-CN-XiaoyiNeural", "晓伊", "zh-CN"),
    ("zh-CN-XiaochenNeural", "晓辰", "zh-CN"),
    ("zh-CN-XiaomengNeural", "晓梦", "zh-CN"),
    ("zh-CN-XiaorouNeural", "晓柔", "zh-CN"),
    ("zh-CN-YunxiNeural", "云希", "zh-CN"),
    ("zh-CN-YunjianNeural", "云健", "zh-CN"),
    ("zh-CN-YunyangNeural", "云扬", "zh-CN"),
    ("zh-CN-YunfengNeural", "云枫", "zh-CN"),
];

pub fn default_voice() -> &'static str {
    // The multilingual voice first: selections are routinely mixed Chinese and
    // English, and this is the family that reads both naturally.
    VOICES[0].0
}

#[derive(Debug, Clone)]
pub struct AzureConfig {
    pub api_key: String,
    /// Azure region short name (`eastus`, `eastasia`, …). The key is issued
    /// per resource and a resource lives in exactly one region, so a mismatch
    /// is a 401 — not a slower success.
    pub region: String,
}

/// Convert the stored 0.0..=1.0 rate into the SSML prosody multiplier.
///
/// Same normalization as the other providers: 0.5 stored is the 1x mark, and
/// Azure's `<prosody rate>` takes a bare multiplier over the same 0.5–2.0
/// span the slider shows.
fn speed_from_normalized(rate: Option<f32>) -> f32 {
    let Some(rate) = rate else { return 1.0 };
    (rate / 0.5).clamp(0.5, 2.0)
}

/// The output-format name for a negotiated sample rate.
///
/// Named formats, not parameters: the service only speaks in whole format
/// strings, and asking for one it does not know is a 400 before any audio.
fn output_format(sample_rate: u32) -> &'static str {
    match sample_rate {
        48_000 => "audio-48khz-96kbitrate-mono-mp3",
        16_000 => "audio-16khz-64kbitrate-mono-mp3",
        _ => "audio-24khz-96kbitrate-mono-mp3",
    }
}

/// Escape text for embedding in SSML character data or attribute values.
///
/// The voice id goes through this too — it is user-typeable, and a quote in it
/// must break the request visibly as a 400 rather than rewrite the markup.
fn escape_xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// The one-request SSML document.
///
/// `xml:lang` is a default for text outside any voice, so zh-CN is safe for
/// every voice including the English ones — the voice element's own locale
/// wins. The prosody element is always present: rate 1.0 inside one renders
/// identically to no element at all, and one shape keeps the tests honest.
fn build_ssml(text: &str, voice: &str, speed: f32) -> String {
    format!(
        "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='zh-CN'>\
         <voice name='{}'><prosody rate='{:.2}'>{}</prosody></voice></speak>",
        escape_xml(voice),
        speed,
        escape_xml(text)
    )
}

fn synthesis_url(region: &str) -> String {
    let region = region.trim();
    let region = if region.is_empty() {
        DEFAULT_REGION
    } else {
        region
    };
    format!("https://{region}.tts.speech.microsoft.com/cognitiveservices/v1")
}

pub struct AzureBackend {
    config: Mutex<AzureConfig>,
    /// Filled in by the decode thread once it owns the output device, so `stop`
    /// can cut the audio immediately instead of waiting for the network side to
    /// notice. Shared rather than copied: the handle does not exist yet when
    /// `start` returns.
    playback: Arc<Mutex<Option<PlaybackHandle>>>,
    speaking: Arc<AtomicBool>,
}

impl AzureBackend {
    pub fn new(config: AzureConfig) -> Self {
        Self {
            config: Mutex::new(config),
            playback: Arc::new(Mutex::new(None)),
            speaking: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn apply_config(&self, config: AzureConfig) {
        if let Ok(mut slot) = self.config.lock() {
            *slot = config;
        }
    }

    fn config(&self) -> Result<AzureConfig, TtsError> {
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

impl TtsBackend for AzureBackend {
    fn name(&self) -> &'static str {
        "azure"
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
}

impl AzureBackend {
    fn begin(&self, request: TtsRequest, token: CancelToken) -> Result<(), TtsError> {
        let config = self.config()?;
        let sample_rate = negotiate_sample_rate_among(&SAMPLE_RATES)
            .map_err(|err| TtsError::Backend(format!("{} ({})", err, err.code())))?;

        let voice = request
            .voice
            .clone()
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| default_voice().to_string());
        let speed = speed_from_normalized(request.rate);
        let gain = request.volume.unwrap_or(1.0);
        // Unlike CosyVoice, synthesis here outruns even 2x playback — but the
        // shared prebuffer policy costs nothing at 1x and keeps a slow region
        // from crackling at the slider's fast end.
        let prebuffer = prebuffer_samples(speed, sample_rate);

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        // Lets the decode side tell "the provider failed" apart from "the audio
        // ended", which otherwise both look like a closed channel.
        let network_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let source = ChunkSource::new(rx, token.clone());
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
                    source,
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

        let text = request.text;
        let http_token = token;
        let http_error = network_error;
        tauri::async_runtime::spawn(async move {
            // One request per piece, all feeding the same decoder: the service
            // caps produced audio per request, not per read, and the pieces end
            // on sentence or clause boundaries, so a seam is audible only as an
            // ordinary pause. The budget follows the speed — the cap is audio
            // time, and 0.5x speech reaches it in half the characters.
            let pieces = split_for_backend(&text, piece_chars_for(speed));
            if pieces.len() > 1 {
                log_event("speak_chunked", &[("pieces", pieces.len().to_string())]);
            }
            for piece in &pieces {
                let outcome = stream_audio_with_retry(
                    &config,
                    &Synthesis {
                        text: piece,
                        voice: &voice,
                        sample_rate,
                        rate: speed,
                    },
                    &tx,
                    &http_token,
                )
                .await;
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
            // Closing the channel is what ends the decode loop.
            drop(tx);
        });

        Ok(())
    }
}

/// The synthesis parameters, already on the provider's own scales.
struct Synthesis<'a> {
    text: &'a str,
    voice: &'a str,
    sample_rate: u32,
    /// Speed multiplier, 1.0 neutral, as SSML prosody takes it.
    rate: f32,
}

/// Decode and play, on a thread of its own because both block and because the
/// output stream may not cross threads.
#[allow(clippy::too_many_arguments)]
fn run_playback(
    source: ChunkSource,
    sample_rate: u32,
    gain: f32,
    prebuffer: u64,
    token: CancelToken,
    network_error: Arc<Mutex<Option<String>>>,
    handle_slot: Arc<Mutex<Option<PlaybackHandle>>>,
    speaking: Arc<AtomicBool>,
) {
    let playback = match Playback::open(sample_rate, gain, prebuffer) {
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
    let decoded = decode_mp3_stream(source, sample_rate, |samples| {
        if !started {
            started = true;
            speaking.store(true, Ordering::SeqCst);
            log_event("speak_started", &[]);
        }
        playback.push(samples)
    });

    let failure = network_error.lock().ok().and_then(|slot| slot.clone());

    match decoded {
        Ok(_) if failure.is_none() => {
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
                    }
                }
            }
        }
        // A network failure reads as a truncated stream, so report the real
        // cause rather than the decoder's confusion about it.
        _ => {
            let detail = failure.unwrap_or_else(|| match &decoded {
                Err(err) => err.to_string(),
                Ok(_) => "the audio stream ended early".to_string(),
            });
            if token.finish() {
                log_event(
                    "speak_err",
                    &[("error", "backend".to_string()), ("detail", detail)],
                );
            }
        }
    }
}

/// Stream one synthesis response, forwarding MP3 bytes to `tx`.
///
/// `Ok(true)` means the piece completed and the caller may stream the next
/// one; `Ok(false)` means the read was cancelled or the decoder hung up, so
/// further pieces would only bill text nobody hears.
async fn stream_audio_with_retry(
    config: &AzureConfig,
    synthesis: &Synthesis<'_>,
    tx: &Sender<Vec<u8>>,
    token: &CancelToken,
) -> Result<bool, String> {
    let mut retries_done = 0;
    loop {
        match stream_audio(config, synthesis, tx, token).await {
            Ok(outcome) => return Ok(outcome),
            Err(err) => {
                if token.is_cancelled() {
                    return Ok(false);
                }
                if let Some(delay_ms) = err.retry_delay_ms(retries_done) {
                    log_cloud_retry("azure", retries_done, delay_ms, err.reason());
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
    config: &AzureConfig,
    synthesis: &Synthesis<'_>,
    tx: &Sender<Vec<u8>>,
    token: &CancelToken,
) -> Result<bool, CloudStreamError> {
    let response = cloud_http_client()?
        .post(synthesis_url(&config.region))
        .header("Ocp-Apim-Subscription-Key", config.api_key.trim())
        .header("Content-Type", "application/ssml+xml")
        .header("X-Microsoft-OutputFormat", output_format(synthesis.sample_rate))
        // Required by the v1 REST endpoint; requests without one are rejected.
        .header("User-Agent", "VoiceX")
        .body(build_ssml(synthesis.text, synthesis.voice, synthesis.rate))
        .send()
        .await
        .map_err(CloudStreamError::request)?;

    let status = response.status();
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        return Err(CloudStreamError::http(status, describe_failure(&detail)));
    }

    // Success is just the MP3, chunk by chunk. There is no framing to parse
    // and no mid-stream error channel: a failure after the headers can only be
    // a transport break, which the decode side reports as a truncated stream.
    let mut stream = response.bytes_stream();
    let mut audio_emitted = false;
    while let Some(chunk) = stream.next().await {
        if token.is_cancelled() {
            return Ok(false);
        }
        let chunk = chunk.map_err(|err| CloudStreamError::stream(err, audio_emitted))?;
        if chunk.is_empty() {
            continue;
        }
        if tx.send(chunk.to_vec()).is_err() {
            // The decoder is gone; nothing left to stream into.
            return Ok(false);
        }
        audio_emitted = true;
    }

    Ok(true)
}

/// Pull the useful part out of an error body.
///
/// The service answers auth failures with an empty body — the status line is
/// all there is — and 400s with a short plain-text or JSON sentence. Either
/// way the first line, capped, is the useful part.
fn describe_failure(body: &str) -> String {
    let line = body.lines().next().unwrap_or_default().trim();
    if line.is_empty() {
        return "no detail in the response body".to_string();
    }
    line.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rate_scales_line_up_at_both_ends_and_the_middle() {
        // Stored 0..1 around a 0.5 default, shown as 0.5x-2x, sent as the same
        // 0.5-2.0 multiplier. Getting this wrong is silent: the voice just
        // speaks at the wrong speed.
        assert_eq!(speed_from_normalized(Some(0.25)), 0.5, "0.5x");
        assert_eq!(speed_from_normalized(Some(0.5)), 1.0, "1x is neutral");
        assert_eq!(speed_from_normalized(Some(1.0)), 2.0, "2x");
        assert_eq!(speed_from_normalized(None), 1.0, "unset is neutral");
        assert_eq!(speed_from_normalized(Some(0.0)), 0.5, "clamped low");
        assert_eq!(speed_from_normalized(Some(5.0)), 2.0, "clamped high");
    }

    #[test]
    fn slow_speech_shrinks_the_piece_to_stay_inside_the_ten_minute_cap() {
        // The cap is produced audio, silently truncated at 600 s (measured:
        // 2150 characters at 0.5x lost their tail with HTTP 200). A fixed
        // 2000-character piece is 7.7 minutes at 1x but 15 at 0.5x.
        assert_eq!(piece_chars_for(1.0), 2_000);
        assert_eq!(piece_chars_for(0.5), 1_000);
        assert_eq!(piece_chars_for(2.0), 2_000, "fast speech keeps the 1x budget");
        // ~260 characters a minute at 1x: every slider position must land
        // well under the 10-minute truncation point.
        for speed in [0.5f32, 0.75, 1.0, 1.5, 2.0] {
            let minutes = piece_chars_for(speed) as f32 / (260.0 * speed);
            assert!(minutes < 9.0, "{speed}x renders {minutes:.1} min per piece");
        }
    }

    #[test]
    fn the_ssml_carries_voice_rate_and_text() {
        let ssml = build_ssml("你好。", "zh-CN-XiaoxiaoNeural", 1.5);
        assert!(ssml.contains("<voice name='zh-CN-XiaoxiaoNeural'>"), "{ssml}");
        assert!(ssml.contains("<prosody rate='1.50'>你好。</prosody>"), "{ssml}");
        assert!(ssml.starts_with("<speak"), "{ssml}");
        assert!(ssml.ends_with("</speak>"), "{ssml}");
    }

    #[test]
    fn markup_in_the_selection_is_read_as_text_not_executed_as_ssml() {
        // Selections routinely contain XML and code. Unescaped, a stray tag is
        // at best a 400 and at worst silently swallowed text.
        let ssml = build_ssml("a < b && c > d \"quoted\"", "v", 1.0);
        assert!(
            ssml.contains("a &lt; b &amp;&amp; c &gt; d &quot;quoted&quot;"),
            "{ssml}"
        );

        // The voice id is user-typeable; a quote in it must not rewrite the
        // markup around it.
        let ssml = build_ssml("hi", "bad'voice", 1.0);
        assert!(ssml.contains("name='bad&apos;voice'"), "{ssml}");
    }

    #[test]
    fn every_negotiable_rate_has_a_named_output_format() {
        // The service only speaks whole format names; a rate that fell through
        // to a mismatched format would decode at the wrong pitch or 400.
        assert_eq!(output_format(48_000), "audio-48khz-96kbitrate-mono-mp3");
        assert_eq!(output_format(24_000), "audio-24khz-96kbitrate-mono-mp3");
        assert_eq!(output_format(16_000), "audio-16khz-64kbitrate-mono-mp3");
        for rate in SAMPLE_RATES {
            assert!(output_format(rate).contains(&format!("{}khz", rate / 1_000)));
        }
    }

    #[test]
    fn the_region_shapes_the_endpoint_and_an_empty_one_falls_back() {
        assert_eq!(
            synthesis_url("eastasia"),
            "https://eastasia.tts.speech.microsoft.com/cognitiveservices/v1"
        );
        // Settings are user-editable; an accidentally cleared region must not
        // produce `https://.tts…`, which fails DNS with a baffling error.
        assert_eq!(
            synthesis_url(""),
            "https://eastus.tts.speech.microsoft.com/cognitiveservices/v1"
        );
        assert_eq!(synthesis_url("  "), synthesis_url(DEFAULT_REGION));
    }

    #[test]
    fn the_voice_table_leads_with_multilingual_and_stays_standard_tier() {
        // Selections are routinely mixed Chinese and English; the multilingual
        // family is the one that reads both naturally, so it is the default.
        assert_eq!(default_voice(), "zh-CN-XiaoyuMultilingualNeural");
        // Cost guard: HD voices bill at a higher tier, so the curated list
        // must never quietly grow one — typing the id stays possible.
        for (id, _, _) in VOICES {
            assert!(!id.contains(':'), "{id} is an HD-tier voice");
            assert!(id.ends_with("Neural"), "{id}");
        }
    }

    #[test]
    fn empty_error_bodies_still_describe_the_failure() {
        // Auth failures arrive with an empty body; the description must not be
        // an empty string that renders as "HTTP 401: ".
        assert_eq!(describe_failure(""), "no detail in the response body");
        assert_eq!(describe_failure("Bad request\nsecond line"), "Bad request");
    }

    #[test]
    fn a_refused_start_hands_the_session_back() {
        // Without this the session stays claimed forever: the read hotkey turns
        // into a permanent "stop", and the HUD sits on "preparing" with nothing
        // ever coming. Reachable by simply not having configured a key yet.
        let backend = AzureBackend::new(AzureConfig {
            api_key: String::new(),
            region: DEFAULT_REGION.to_string(),
        });
        let slot = crate::tts::SessionSlot::default();
        let token = slot.claim();
        assert!(slot.is_active());

        let outcome = backend.start(TtsRequest::plain("hi".to_string()), token);

        assert!(outcome.is_err(), "an unconfigured backend must refuse");
        assert!(!slot.is_active(), "a refusal must release the session");
        assert_eq!(backend.status(), TtsStatus::Idle);
    }

    /// End-to-end against the live service: network, chunked MP3 decode and
    /// playback, in the same arrangement the backend uses. Plays audio, so it
    /// is opt-in:
    ///
    /// ```text
    /// AZURE_TTS_API_KEY=... AZURE_TTS_REGION=eastus \
    ///   cargo test --lib azure::tests::live -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires network access and credentials"]
    fn live_synthesis_decodes_and_plays() {
        use crate::tts::SessionSlot;
        use std::time::Instant;

        let api_key = std::env::var("AZURE_TTS_API_KEY").expect("AZURE_TTS_API_KEY is not set");
        let region =
            std::env::var("AZURE_TTS_REGION").unwrap_or_else(|_| DEFAULT_REGION.to_string());
        let config = AzureConfig { api_key, region };

        let rate =
            negotiate_sample_rate_among(&SAMPLE_RATES).expect("no usable output sample rate");
        let voice = default_voice();
        eprintln!("\n=== azure @ {rate} Hz, voice {voice} ===");

        let text = "Azure Speech 语音合成，端到端链路验证：chunked streaming、MP3 解码与本地播放。";
        let slot = SessionSlot::default();
        let token = slot.claim();
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let source = ChunkSource::new(rx, token.clone());

        let decode_token = token.clone();
        let decoder = thread::spawn(move || {
            let playback = Playback::open(rate, 1.0, 0).expect("failed to open the output device");
            let handle = playback.handle();
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
            &Synthesis {
                text,
                voice,
                sample_rate: rate,
                rate: 1.0,
            },
            &tx,
            &token,
        ));
        drop(tx);
        outcome.expect("streaming failed");

        let (samples, levels) = decoder.join().expect("decoder panicked");
        let seconds = samples as f64 / rate as f64;
        let peak = levels.iter().copied().fold(0.0f32, f32::max);
        eprintln!(
            "decoded {samples} samples ({seconds:.2} s) in {:?}, peak level {peak:.4}",
            started.elapsed()
        );

        assert!(
            seconds > 2.0,
            "expected several seconds of speech, decoded {seconds:.2} s"
        );
        assert!(
            peak > 0.01,
            "the HUD waveform is driven by this level, and it stayed at {peak:.4} \
             while audio was playing"
        );
    }

    /// Which voice ids the account actually accepts. The table is hand-kept,
    /// and a wrong id fails only at speak time — so this checks them all in
    /// one pass:
    ///
    /// ```text
    /// AZURE_TTS_API_KEY=... cargo test --lib azure::tests::voice_table -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires network access and credentials"]
    fn voice_table_matches_what_the_service_accepts() {
        let api_key = std::env::var("AZURE_TTS_API_KEY").expect("AZURE_TTS_API_KEY is not set");
        let region =
            std::env::var("AZURE_TTS_REGION").unwrap_or_else(|_| DEFAULT_REGION.to_string());
        let config = AzureConfig { api_key, region };
        let mut rejected: Vec<String> = Vec::new();

        for (id, name, _) in VOICES {
            let slot = crate::tts::SessionSlot::default();
            let token = slot.claim();
            let (tx, rx) = mpsc::channel::<Vec<u8>>();
            // Drained on a thread so a slow reader cannot stall the sender.
            let drain = thread::spawn(move || rx.iter().count());

            let outcome = tauri::async_runtime::block_on(stream_audio(
                &config,
                &Synthesis {
                    text: "测试。",
                    voice: id,
                    sample_rate: SAMPLE_RATES[0],
                    rate: 1.0,
                },
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
