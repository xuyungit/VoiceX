//! Alibaba Cloud Model Studio (百炼) speech synthesis backend.
//!
//! Covers four model families behind one provider. Qwen3-TTS is its own
//! service. Qwen-Audio-3.0-TTS and CosyVoice-v3 share the SpeechSynthesizer
//! endpoint and the same request spelling — CosyVoice is the engine behind
//! both, which is why their error messages say `[cosyvoice:]` — but the voice
//! ids are not interchangeable. CosyVoice-v3.5-flash uses that same path and
//! spelling, with no system preset voices: every request has to name a cloned
//! or designed `voice_id` bound to this model, or the engine returns 418.
//! Over HTTP with server-sent events they all hand back the same thing in the
//! same shape: base64 MP3 in `output.audio.data`, chunk after chunk, with a
//! URL in the final frame that we discard. That similarity is what makes one
//! backend reasonable; [`ModelSpec`] holds everything that is genuinely per
//! model.
//!
//! Chosen over the two WebSocket protocols on the same measurement that settled
//! the Volcengine backend: the text is fully known before the request goes out,
//! so incremental input buys nothing, and first audio arrives in 407-539 ms
//! either way. See `docs/aliyun-tts-provider-research-2026-08-13.md` for the
//! probe results, including the several documented parameters that do not exist
//! and the several undocumented ones that do.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use base64::Engine;
use futures_util::StreamExt;
use serde_json::{json, Value};

use super::decode::{decode_mp3_stream, ChunkSource};
use super::playback::{
    negotiate_sample_rate_among, prebuffer_samples, Playback, PlaybackHandle,
};
use super::{
    cloud_http_client, log_cloud_retry, log_event, split_for_backend, CancelToken,
    CloudStreamError, TtsBackend, TtsError, TtsRequest, TtsStatus, TtsVoice,
};

/// Region host. The workspace-scoped `{id}.cn-beijing.maas.aliyuncs.com` form
/// is what the documentation now advertises, but the plain host still serves
/// both models over both HTTP and WebSocket, so requiring a workspace id would
/// be one more mandatory field for no gain. If the plain host is ever retired,
/// `crate::asr::funasr_client::qwen_workspace_host` already builds the other.
const HOST: &str = "https://dashscope.aliyuncs.com";

pub const MODEL_QWEN3: &str = "qwen3-tts-flash";
pub const MODEL_QWEN_AUDIO: &str = "qwen-audio-3.0-tts-flash";
pub const MODEL_COSYVOICE: &str = "cosyvoice-v3-flash";
pub const MODEL_COSYVOICE_V35: &str = "cosyvoice-v3.5-flash";

pub fn default_model() -> &'static str {
    // Qwen-Audio 3.0 over Qwen3: the same 中英混读 quality with the far larger
    // voice roster (12 system + ~600 basic ids) and the 9000-character piece
    // size. Qwen3 remains the pick for its dialect voices. Keep the frontend
    // store default (settings.ts) in step with this.
    MODEL_QWEN_AUDIO
}

/// Everything that differs between the model families.
///
/// The parameter names look like gratuitous variation — `speech_rate` against
/// `rate`, `response_format` against `format` — but they are separate services
/// and each ignores the other's spelling silently, returning 200 and audio at
/// the default setting. That silence is why these are a table rather than an
/// attempt to send both spellings and let the server sort it out.
struct ModelSpec {
    id: &'static str,
    path: &'static str,
    /// Rates the model renders, best first. Narrower than the playback module's
    /// own list for `qwen3-tts-flash`, which has no 44.1 or 22.05 kHz.
    sample_rates: &'static [u32],
    /// Longest text accepted in one request.
    max_chars: usize,
    /// Longest text one request reliably *finishes*. Distinct from
    /// `max_chars`, which is where the service starts rejecting: CosyVoice
    /// accepts thousands of characters and then silently truncates the audio
    /// at ~836 output tokens (~33 s at 1x) per internal batch, reporting
    /// `finish_reason=stop` as if nothing happened. Batches are sentences
    /// merged server-side to ~150 characters, so the only reliable control is
    /// keeping the whole request under the budget. Measured 2026-08: a
    /// 163-character single-sentence request lost its tail on every rate and
    /// sample rate; 86 characters always completed; replacing clause commas
    /// with full stops did not help because the server merges the sentences
    /// straight back into one batch.
    piece_chars: usize,
    voices: VoiceSource,
    /// Always called with this spec's own `id`, so the body cannot name a
    /// different model than the spec it belongs to — a copy-pasted entry that
    /// forgot to re-thread the constant used to compile fine and send requests
    /// for the wrong model.
    build_body: fn(model: &'static str, s: &Synthesis) -> Value,
}

/// Where a model's voice ids come from.
enum VoiceSource {
    /// System presets the picker can list; the first is the default.
    Preset(&'static [(&'static str, &'static str, &'static str)]),
    /// No presets at all: every request has to name a cloned or designed
    /// voice bound to this model, or the engine returns 418. Those ids belong
    /// to the account that created them, so there is nothing to ship as a
    /// default and nothing for the picker to list — the settings page shows a
    /// text field instead.
    Custom,
}

impl ModelSpec {
    fn presets(&self) -> &'static [(&'static str, &'static str, &'static str)] {
        match self.voices {
            VoiceSource::Preset(voices) => voices,
            VoiceSource::Custom => &[],
        }
    }

    fn default_voice(&self) -> Option<&'static str> {
        self.presets().first().map(|(id, _, _)| *id)
    }

    fn custom_voice_only(&self) -> bool {
        matches!(self.voices, VoiceSource::Custom)
    }
}

/// The synthesis parameters, already on the provider's own scales.
struct Synthesis<'a> {
    text: &'a str,
    voice: &'a str,
    sample_rate: u32,
    /// Speed multiplier, 1.0 neutral, as both families take it.
    rate: f32,
}

/// Qwen3-TTS. Voices are English given names; the dialect voices are the
/// reason to reach for this family over the other one.
const QWEN3_VOICES: [(&str, &str, &str); 12] = [
    ("Cherry", "芊悦", "zh-CN"),
    ("Serena", "苏瑶", "zh-CN"),
    ("Ethan", "晨煦", "zh-CN"),
    ("Chelsie", "千雪", "zh-CN"),
    ("Nofish", "不吃鱼", "zh-CN"),
    ("Dylan", "北京-晓东", "zh-CN"),
    ("Jada", "上海-阿珍", "zh-CN"),
    ("Sunny", "四川-晴儿", "zh-CN"),
    ("Rocky", "粤语-阿强", "zh-CN"),
    ("Kiki", "粤语-阿清", "zh-CN"),
    ("Jennifer", "詹妮弗", "en-US"),
    ("Ryan", "甜茶", "en-US"),
];

/// Qwen-Audio-3.0-TTS, which shares its engine with CosyVoice — the error
/// messages say `[cosyvoice:]` outright. Voice ids still do not carry across:
/// `longanhuan_v3.6` here is not `longanhuan` / `longanhuan_v3` on CosyVoice.
///
/// The first twelve are the flash model's complete system-voice list. Below
/// them is a reading-shaped pick from the ~600 "basic" voices the same model
/// serves under `qwen-audio-3.0-tts-flash-<suffix>` ids (the 有声阅读/知识分享
/// rows of the published Excel); the picker's typed-id path reaches the rest.
/// Every entry here returned audio from the live account on 2026-08-30.
const QWEN_AUDIO_VOICES: [(&str, &str, &str); 19] = [
    ("longanfengyue", "龙安风悦", "zh-CN"),
    ("longanyuanfei", "龙安元妃", "zh-CN"),
    ("longanlingxi", "龙安灵希", "zh-CN"),
    ("longanxiaoxin", "龙安小昕", "zh-CN"),
    ("longanhuan_v3.6", "龙安欢", "zh-CN"),
    ("longjielidou_v3.6", "龙杰力豆", "zh-CN"),
    ("longpaopao_v3.6", "龙泡泡", "zh-CN"),
    ("longhuohuo_v3.6", "龙火火", "zh-CN"),
    ("longchuanshu_v3.6", "龙川叔", "zh-CN"),
    ("loongmary", "loongmary", "en-GB"),
    ("loongeva_v3.6", "loongeva", "en-US"),
    ("loongjohn", "loongJohn", "en-US"),
    ("qwen-audio-3.0-tts-flash-longrongtaolian", "龙蓉桃涟（有声阅读）", "zh-CN"),
    ("qwen-audio-3.0-tts-flash-longyimuling", "龙翼暮凌（有声阅读）", "zh-CN"),
    ("qwen-audio-3.0-tts-flash-longyuyaoluan", "龙羽瑶鸾（有声阅读）", "zh-CN"),
    ("qwen-audio-3.0-tts-flash-longnilanfeng", "龙霓岚凤（有声阅读）", "zh-CN"),
    ("qwen-audio-3.0-tts-flash-longjinlianxiao", "龙瑾涟晓（有声阅读）", "zh-CN"),
    ("qwen-audio-3.0-tts-flash-loongadriangao", "Adrian Gao（阅读）", "en-US"),
    ("qwen-audio-3.0-tts-flash-loongivyhu", "Ivy Hu（朗诵）", "en-US"),
];

/// CosyVoice-v3-flash system voices. The service has eighty-plus; this is the
/// reading-shaped subset that the live account accepted. The picker still
/// accepts a typed id, so the rest stay reachable.
///
/// Language tags follow the voice list's primary language and the convention
/// the Qwen3 table set: dialect voices (Cantonese included) are `zh-CN`, so
/// filtering the picker by language behaves the same under every model. Per
/// the published list, `loongbella_v3` is a Mandarin+English voice despite the
/// `loong` prefix that marks the English-only ones.
const COSYVOICE_VOICES: [(&str, &str, &str); 10] = [
    ("longanyang", "龙安洋", "zh-CN"),
    ("longanhuan", "龙安欢", "zh-CN"),
    ("longanhuan_v3", "龙安欢（方言）", "zh-CN"),
    ("longhuhu_v3", "龙呼呼", "zh-CN"),
    ("longjiaxin_v3", "龙嘉欣（粤语）", "zh-CN"),
    ("longlaotie_v3", "龙老铁", "zh-CN"),
    ("longsanshu_v3", "龙三叔", "zh-CN"),
    ("longshuo_v3", "龙硕", "zh-CN"),
    ("loongabby_v3", "loongabby", "en-US"),
    ("loongbella_v3", "Bella3.0", "zh-CN"),
];

fn speech_synthesizer_body(model: &'static str, s: &Synthesis<'_>) -> Value {
    json!({
        "model": model,
        "input": {
            "text": s.text,
            "voice": s.voice,
            "format": "mp3",
            "sample_rate": s.sample_rate,
            "rate": s.rate,
        },
    })
}

const SPECS: [ModelSpec; 4] = [
    ModelSpec {
        id: MODEL_QWEN3,
        path: "/api/v1/services/aigc/multimodal-generation/generation",
        sample_rates: &[48_000, 24_000, 16_000],
        // Measured: 5000 characters are accepted, and the documented 512-token
        // ceiling does not exist. First audio does grow with length — 479 ms at
        // 500 characters, 1.9 s at 5000 — so this is the limit, not a target.
        max_chars: 5_000,
        // Renders ~4x realtime and completed every long-text probe intact.
        piece_chars: 5_000,
        voices: VoiceSource::Preset(&QWEN3_VOICES),
        build_body: |model, s| {
            json!({
                "model": model,
                "input": {
                    "text": s.text,
                    "voice": s.voice,
                    // Selections are routinely mixed Chinese and English, and
                    // naming one language makes the other one read badly.
                    "language_type": "Auto",
                },
                "parameters": {
                    "response_format": "mp3",
                    "sample_rate": s.sample_rate,
                    "speech_rate": s.rate,
                },
            })
        },
    },
    ModelSpec {
        id: MODEL_QWEN_AUDIO,
        path: "/api/v1/services/audio/tts/SpeechSynthesizer",
        sample_rates: &[48_000, 44_100, 24_000, 22_050, 16_000],
        // The service caps a request at 20000 of its own units and counts a
        // CJK character as two of them: 20000 characters were reported back
        // as 36000 and 15000 as 27000, both rejected; 10000 were accepted.
        // 9000 keeps a pure-CJK selection at 18000 units, short of the edge.
        max_chars: 9_000,
        // Shares CosyVoice's engine but not its budget: the 163-character
        // single-sentence probe came back complete, ~10x realtime.
        piece_chars: 9_000,
        voices: VoiceSource::Preset(&QWEN_AUDIO_VOICES),
        build_body: speech_synthesizer_body,
    },
    ModelSpec {
        id: MODEL_COSYVOICE,
        path: "/api/v1/services/audio/tts/SpeechSynthesizer",
        // Measured: every rate on this list comes back honoured in the MP3
        // frame header, so the device-preferred rate the playback module
        // negotiates is safe to request.
        sample_rates: &[48_000, 44_100, 24_000, 22_050, 16_000],
        // Same 20000-unit cap as qwen-audio, with the same CJK-counts-double
        // rule: 15000 characters were rejected as 27000 units, 10000 accepted.
        max_chars: 9_000,
        // The ~836-token batch budget (see `piece_chars` on the struct) bites
        // at ~155 characters of dense prose, sooner for text that speaks
        // slower — digits and heavy punctuation expand. 120 keeps ordinary
        // text a comfortable margin inside it while cutting only once per
        // ~half minute of speech.
        piece_chars: 120,
        voices: VoiceSource::Preset(&COSYVOICE_VOICES),
        build_body: speech_synthesizer_body,
    },
    ModelSpec {
        id: MODEL_COSYVOICE_V35,
        path: "/api/v1/services/audio/tts/SpeechSynthesizer",
        sample_rates: &[48_000, 44_100, 24_000, 22_050, 16_000],
        max_chars: 9_000,
        // Same silent-truncation class as v3-flash, measured 2026-09-08 with
        // a designed voice: a period-free 180-character request returned
        // complete audio (~5.2 kB/char); 200 characters returned an ID3
        // header and nothing else, `finish_reason=stop`, billed in full.
        // That single plain-prose point says nothing about digit-heavy or
        // slow-rate text, which expands on this engine exactly as on v3, so
        // v3's 120 applies unchanged until dense text is measured here too.
        piece_chars: 120,
        voices: VoiceSource::Custom,
        build_body: speech_synthesizer_body,
    },
];

fn spec_for(model: &str) -> &'static ModelSpec {
    SPECS
        .iter()
        .find(|spec| spec.id == model)
        .unwrap_or(&SPECS[0])
}

pub fn default_voice_for(model: &str) -> &'static str {
    spec_for(model).default_voice().unwrap_or("")
}

#[derive(Debug, Clone)]
pub struct AliyunConfig {
    pub api_key: String,
    pub model: String,
}

/// Convert the stored 0.0..=1.0 rate into the provider's own multiplier.
///
/// The stored value is normalized around the macOS engine default of 0.5, shown
/// as 1x on a 0.5x-2x slider, and every model family takes exactly that
/// multiplier over exactly that range — so unlike Volcengine's -50..=100 this
/// needs no remapping, only the same clamp.
fn speed_from_normalized(rate: Option<f32>) -> f32 {
    let Some(rate) = rate else { return 1.0 };
    (rate / 0.5).clamp(0.5, 2.0)
}

pub struct AliyunBackend {
    config: Mutex<AliyunConfig>,
    /// Filled in by the decode thread once it owns the output device, so `stop`
    /// can cut the audio immediately instead of waiting for the network side to
    /// notice. Shared rather than copied: the handle does not exist yet when
    /// `start` returns.
    playback: Arc<Mutex<Option<PlaybackHandle>>>,
    speaking: Arc<AtomicBool>,
}

impl AliyunBackend {
    pub fn new(config: AliyunConfig) -> Self {
        Self {
            config: Mutex::new(config),
            playback: Arc::new(Mutex::new(None)),
            speaking: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn apply_config(&self, config: AliyunConfig) {
        if let Ok(mut slot) = self.config.lock() {
            *slot = config;
        }
    }

    fn config(&self) -> Result<AliyunConfig, TtsError> {
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

    fn spec(&self) -> &'static ModelSpec {
        self.config
            .lock()
            .map(|slot| spec_for(&slot.model))
            .unwrap_or(&SPECS[0])
    }
}

impl TtsBackend for AliyunBackend {
    fn name(&self) -> &'static str {
        "aliyun"
    }

    fn list_voices(&self) -> Result<Vec<TtsVoice>, TtsError> {
        Ok(self
            .spec()
            .presets()
            .iter()
            .map(|(id, name, language)| TtsVoice {
                id: id.to_string(),
                name: name.to_string(),
                language: language.to_string(),
            })
            .collect())
    }

    fn custom_voice_only(&self) -> bool {
        self.spec().custom_voice_only()
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

impl AliyunBackend {
    fn begin(&self, request: TtsRequest, token: CancelToken) -> Result<(), TtsError> {
        let config = self.config()?;
        let spec = spec_for(&config.model);
        let sample_rate = negotiate_sample_rate_among(spec.sample_rates)
            .map_err(|err| TtsError::Backend(format!("{} ({})", err, err.code())))?;

        // Trimmed, not just trim-checked: a designed-voice id is pasted by
        // hand, and a trailing newline from the console would otherwise reach
        // the service verbatim and come back as the same 418 a wrong id gets.
        let voice = match request.voice.as_deref().map(str::trim) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => spec
                .default_voice()
                .map(str::to_string)
                .ok_or_else(|| {
                    TtsError::Backend(format!(
                        "{} has no system voices; configure a cloned or designed voice id",
                        spec.id
                    ))
                })?,
        };
        // Pitch is deliberately not sent. `pitch_rate` does apply, but halving
        // it stretched the audio to 4.2x rather than the 2x a resampling pitch
        // shift would give, so what it actually changes is unclear and the
        // settings page hides the row for cloud providers anyway.
        let speed = speed_from_normalized(request.rate);
        let gain = request.volume.unwrap_or(1.0);
        // Fast speech drains audio quicker than CosyVoice synthesizes it (the
        // service generates a constant characters-per-second regardless of the
        // requested rate), so those reads trade a moment of startup latency
        // for underruns that pause cleanly instead of crackling.
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
            // caps a request, not a read, and the pieces end on sentence or
            // clause boundaries, so a seam is audible only as an ordinary
            // pause. `piece_chars` leads because CosyVoice's silent per-batch
            // output budget bites thousands of characters before `max_chars`
            // would get the request rejected outright; the `min` only guards
            // a future spec whose two limits drift past each other.
            let pieces = split_for_backend(&text, spec.piece_chars.min(spec.max_chars));
            if pieces.len() > 1 {
                log_event("speak_chunked", &[("pieces", pieces.len().to_string())]);
            }
            for piece in &pieces {
                let outcome = stream_audio_with_retry(
                    &config.api_key,
                    spec,
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

/// Stream one synthesis response, forwarding decoded audio bytes to `tx`.
///
/// `Ok(true)` means the piece completed and the caller may stream the next
/// one; `Ok(false)` means the read was cancelled or the decoder hung up, so
/// further pieces would only bill text nobody hears.
async fn stream_audio_with_retry(
    api_key: &str,
    spec: &ModelSpec,
    synthesis: &Synthesis<'_>,
    tx: &Sender<Vec<u8>>,
    token: &CancelToken,
) -> Result<bool, String> {
    let mut retries_done = 0;
    loop {
        match stream_audio(api_key, spec, synthesis, tx, token).await {
            Ok(outcome) => return Ok(outcome),
            Err(err) => {
                if token.is_cancelled() {
                    return Ok(false);
                }
                if let Some(delay_ms) = err.retry_delay_ms(retries_done) {
                    log_cloud_retry("aliyun", retries_done, delay_ms, err.reason());
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
    spec: &ModelSpec,
    synthesis: &Synthesis<'_>,
    tx: &Sender<Vec<u8>>,
    token: &CancelToken,
) -> Result<bool, CloudStreamError> {
    let response = cloud_http_client()?
        .post(format!("{HOST}{}", spec.path))
        .header("Authorization", format!("Bearer {api_key}"))
        // Without this the service synthesizes the whole text before replying,
        // which for a long selection takes minutes rather than seconds.
        .header("X-DashScope-SSE", "enable")
        .json(&(spec.build_body)(spec.id, synthesis))
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
/// Everything but `data:` is framing — `id:`, `event:`, the `:HTTP_STATUS/200`
/// comment, and the blank line between events.
fn parse_line(line: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let text = std::str::from_utf8(line).map_err(|_| "response was not UTF-8".to_string())?;
    let Some(payload) = text.trim_end_matches('\r').strip_prefix("data:") else {
        return Ok(None);
    };
    let payload = payload.trim_start();
    if payload.is_empty() {
        return Ok(None);
    }

    let value: Value =
        serde_json::from_str(payload).map_err(|err| format!("malformed response: {err}"))?;

    // Failures arrive mid-stream with the same 200 as the audio frames, so the
    // only signal is this field appearing.
    if let Some(code) = value.get("code").and_then(|code| code.as_str()) {
        let message = value
            .get("message")
            .and_then(|message| message.as_str())
            .unwrap_or("unknown error");
        return Err(format!("provider error {code}: {message}"));
    }

    let Some(data) = value
        .pointer("/output/audio/data")
        .and_then(|data| data.as_str())
    else {
        return Ok(None);
    };
    // The final frame carries the finished file's URL and an empty `data`. We
    // already have every byte it points at.
    if data.is_empty() {
        return Ok(None);
    }

    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map(Some)
        .map_err(|err| format!("bad base64 in response: {err}"))
}

/// Pull the useful part out of an error body, falling back to its first line.
fn describe_failure(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            let code = value.get("code").and_then(|code| code.as_str())?;
            let message = value
                .get("message")
                .and_then(|message| message.as_str())
                .unwrap_or("");
            Some(format!("{code}: {message}"))
        })
        .unwrap_or_else(|| body.lines().next().unwrap_or_default().to_string())
        .chars()
        .take(200)
        .collect()
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
    }

    #[test]
    fn rates_outside_the_slider_are_clamped_to_what_the_provider_accepts() {
        assert_eq!(speed_from_normalized(Some(0.0)), 0.5);
        assert_eq!(speed_from_normalized(Some(5.0)), 2.0);
    }

    #[test]
    fn each_model_spells_its_parameters_its_own_way() {
        // The two services ignore each other's spelling silently — 200 and
        // audio at the default rate — so a mix-up here would be audible only as
        // "the speed slider does nothing".
        let synthesis = Synthesis {
            text: "hi",
            voice: "Cherry",
            sample_rate: 24_000,
            rate: 1.5,
        };

        let qwen3 = (spec_for(MODEL_QWEN3).build_body)(MODEL_QWEN3, &synthesis);
        assert_eq!(qwen3["parameters"]["speech_rate"], 1.5);
        assert_eq!(qwen3["parameters"]["response_format"], "mp3");
        assert_eq!(qwen3["input"]["language_type"], "Auto");
        assert!(qwen3["input"].get("format").is_none());

        let audio = (spec_for(MODEL_QWEN_AUDIO).build_body)(MODEL_QWEN_AUDIO, &synthesis);
        assert_eq!(audio["input"]["rate"], 1.5);
        assert_eq!(audio["input"]["format"], "mp3");
        assert!(audio["input"].get("language_type").is_none());
        assert!(audio.get("parameters").is_none());

        let cosy = (spec_for(MODEL_COSYVOICE).build_body)(MODEL_COSYVOICE, &synthesis);
        assert_eq!(cosy["model"], MODEL_COSYVOICE);
        assert_eq!(cosy["input"]["rate"], 1.5);
        assert_eq!(cosy["input"]["format"], "mp3");
        assert_eq!(
            spec_for(MODEL_COSYVOICE).path,
            spec_for(MODEL_QWEN_AUDIO).path
        );

        let cosy35 = (spec_for(MODEL_COSYVOICE_V35).build_body)(MODEL_COSYVOICE_V35, &synthesis);
        assert_eq!(cosy35["model"], MODEL_COSYVOICE_V35);
        assert_eq!(cosy35["input"]["rate"], 1.5);
        assert_eq!(cosy35["input"]["format"], "mp3");
        assert_eq!(
            spec_for(MODEL_COSYVOICE_V35).path,
            spec_for(MODEL_COSYVOICE).path
        );
    }

    #[test]
    fn every_spec_names_its_own_model_in_the_body() {
        // The id decides which spec is looked up; the body decides which model
        // the service runs. `stream_audio` threads `spec.id` into `build_body`,
        // and this pins that a body cannot disagree with its spec.
        let synthesis = Synthesis {
            text: "hi",
            voice: "Cherry",
            sample_rate: 24_000,
            rate: 1.0,
        };
        for spec in &SPECS {
            assert_eq!((spec.build_body)(spec.id, &synthesis)["model"], spec.id);
        }
    }

    #[test]
    fn qwen3_never_asks_for_a_rate_it_cannot_render() {
        // The playback module's own list includes 44.1 and 22.05 kHz, which
        // this model does not render. Asking anyway comes back as audio at some
        // other rate and surfaces as a decode mismatch, not as speech.
        for rate in spec_for(MODEL_QWEN3).sample_rates {
            assert!(
                matches!(rate, 48_000 | 24_000 | 16_000 | 8_000),
                "{rate} Hz is not a qwen3-tts-flash rate"
            );
        }
    }

    #[test]
    fn audio_frames_yield_bytes_and_framing_lines_yield_none() {
        let audio = parse_line(br#"data:{"output":{"audio":{"data":"aGVsbG8="}}}"#)
            .unwrap()
            .unwrap();
        assert_eq!(audio, b"hello");

        // The final frame points at the finished file; we already have it.
        let last =
            parse_line(br#"data:{"output":{"audio":{"data":"","url":"http://x"}}}"#).unwrap();
        assert!(last.is_none());

        for framing in [
            &b"id:1"[..],
            b"event:result",
            b":HTTP_STATUS/200",
            b"",
            b"data:",
        ] {
            assert!(parse_line(framing).unwrap().is_none(), "{framing:?}");
        }
    }

    #[test]
    fn a_mid_stream_failure_is_reported_rather_than_read_as_end_of_audio() {
        // These arrive with the same HTTP 200 as the audio frames, so missing
        // one would look like a read that simply stopped early.
        let err = parse_line(
            br#"data:{"code":"InvalidParameter","message":"Invalid voice specified","request_id":"x"}"#,
        )
        .unwrap_err();
        assert!(err.contains("InvalidParameter"), "{err}");
        assert!(err.contains("Invalid voice"), "{err}");
    }

    #[test]
    fn a_carriage_return_does_not_hide_the_payload() {
        // CRLF framing would otherwise leave a trailing \r inside the JSON.
        let audio = parse_line(b"data:{\"output\":{\"audio\":{\"data\":\"aGk=\"}}}\r")
            .unwrap()
            .unwrap();
        assert_eq!(audio, b"hi");
    }

    #[test]
    fn http_failures_report_the_provider_code_not_the_raw_body() {
        let described = describe_failure(
            r#"{"code":"InvalidApiKey","message":"Invalid API-key provided.","request_id":"x"}"#,
        );
        assert_eq!(described, "InvalidApiKey: Invalid API-key provided.");
        assert_eq!(
            describe_failure("gateway timeout\nsecond line"),
            "gateway timeout"
        );
    }

    #[test]
    fn an_unknown_model_falls_back_rather_than_panicking() {
        // Settings are user-editable and survive downgrades, so a model string
        // this build has never heard of has to resolve to something.
        assert_eq!(spec_for("qwen9-tts-imaginary").id, MODEL_QWEN3);
        assert_eq!(default_voice_for("qwen9-tts-imaginary"), "Cherry");
        assert_eq!(default_voice_for(MODEL_QWEN_AUDIO), "longanfengyue");
        assert_eq!(default_voice_for(MODEL_COSYVOICE), "longanyang");
        assert_eq!(
            default_voice_for(MODEL_COSYVOICE_V35),
            "",
            "v3.5-flash has no system voice to fall back to"
        );
    }

    #[test]
    fn switching_model_switches_the_voice_table_with_it() {
        // What the settings page's voice picker rides on: it re-asks after a
        // model change, and the controller answers by re-applying the config
        // first. If the table did not follow, the picker would offer voices the
        // selected model rejects.
        let backend = AliyunBackend::new(AliyunConfig {
            api_key: "sk-test".to_string(),
            model: MODEL_QWEN3.to_string(),
        });
        let listed = backend.list_voices().unwrap();
        assert!(listed.iter().any(|voice| voice.id == "Cherry"));
        assert!(!listed.iter().any(|voice| voice.id == "longanfengyue"));

        backend.apply_config(AliyunConfig {
            api_key: "sk-test".to_string(),
            model: MODEL_QWEN_AUDIO.to_string(),
        });
        let listed = backend.list_voices().unwrap();
        assert!(listed.iter().any(|voice| voice.id == "longanfengyue"));
        assert!(!listed.iter().any(|voice| voice.id == "Cherry"));
        assert!(!listed.iter().any(|voice| voice.id == "longanyang"));

        backend.apply_config(AliyunConfig {
            api_key: "sk-test".to_string(),
            model: MODEL_COSYVOICE.to_string(),
        });
        let listed = backend.list_voices().unwrap();
        assert!(listed.iter().any(|voice| voice.id == "longanyang"));
        assert!(!listed.iter().any(|voice| voice.id == "longanfengyue"));
        assert!(!listed.iter().any(|voice| voice.id == "Cherry"));

        backend.apply_config(AliyunConfig {
            api_key: "sk-test".to_string(),
            model: MODEL_COSYVOICE_V35.to_string(),
        });
        let listed = backend.list_voices().unwrap();
        assert!(
            listed.is_empty(),
            "v3.5-flash must not list system voices the service will 418"
        );

        // The per-request limit rides along with the model; `begin` splits a
        // longer selection into pieces of at most this size.
        assert_eq!(spec_for(MODEL_COSYVOICE).max_chars, 9_000);
        assert_eq!(spec_for(MODEL_COSYVOICE_V35).max_chars, 9_000);
    }

    #[test]
    fn only_cosyvoice_needs_the_short_piece_limit() {
        // The ~836-token batch budget was reproduced on cosyvoice-v3-flash
        // (2026-08) and again on v3.5-flash with a designed voice (2026-09:
        // 180 characters complete, 200 characters an empty ID3). The same
        // 163-character single-sentence text came back complete from
        // qwen3-tts-flash and qwen-audio-3.0-tts-flash. Capping the others
        // would only multiply requests and seams for nothing — and a
        // cosyvoice limit above the measured cliff would reintroduce
        // silently truncated tails.
        assert!(
            spec_for(MODEL_COSYVOICE).piece_chars <= 150,
            "cosyvoice pieces must stay inside the silent output budget"
        );
        assert_eq!(
            spec_for(MODEL_COSYVOICE_V35).piece_chars,
            spec_for(MODEL_COSYVOICE).piece_chars,
            "v3.5-flash shares v3's engine and its margin until dense text is measured on it"
        );
        assert_eq!(spec_for(MODEL_QWEN3).piece_chars, 5_000);
        assert_eq!(spec_for(MODEL_QWEN_AUDIO).piece_chars, 9_000);
        for spec in &SPECS {
            assert!(
                spec.piece_chars <= spec.max_chars,
                "{}: a piece the service rejects outright is never right",
                spec.id
            );
        }
    }

    #[test]
    fn a_refused_start_hands_the_session_back() {
        // Without this the session stays claimed forever: the read hotkey turns
        // into a permanent "stop", and the HUD sits on "preparing" with nothing
        // ever coming. Reachable by simply not having configured a key yet.
        let backend = AliyunBackend::new(AliyunConfig {
            api_key: String::new(),
            model: MODEL_QWEN3.to_string(),
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
    fn a_v35_start_without_a_voice_id_hands_the_session_back() {
        // v3.5-flash has an empty voice table, so a missing id cannot fall
        // back to a preset. The refusal has to release the session the same
        // way an empty API key does, or the HUD sits on preparing forever.
        let backend = AliyunBackend::new(AliyunConfig {
            api_key: "sk-test".to_string(),
            model: MODEL_COSYVOICE_V35.to_string(),
        });
        let slot = crate::tts::SessionSlot::default();
        let token = slot.claim();
        let outcome = backend.start(TtsRequest::plain("hi".to_string()), token);
        let err = outcome.expect_err("a designed-voice model must refuse an empty id");
        assert!(err.to_string().contains("no system voices"), "{err}");
        assert!(!slot.is_active(), "a refusal must release the session");
        assert_eq!(backend.status(), TtsStatus::Idle);
    }

    /// End-to-end against the live service for both models: network, SSE parse,
    /// MP3 decode and playback, in the same arrangement the backend uses. Plays
    /// audio, so it is opt-in:
    ///
    /// ```text
    /// ALIYUN_TTS_API_KEY=... cargo test --lib aliyun::tests::live -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires network access and credentials"]
    fn live_synthesis_decodes_and_plays() {
        use crate::tts::SessionSlot;
        use std::time::Instant;

        let api_key = std::env::var("ALIYUN_TTS_API_KEY").expect("ALIYUN_TTS_API_KEY is not set");

        for spec in SPECS.iter() {
            let Some(voice) = spec.default_voice() else {
                eprintln!(
                    "\n=== {} skipped (no system voices; designed/cloned id required) ===",
                    spec.id
                );
                continue;
            };
            let rate = negotiate_sample_rate_among(spec.sample_rates)
                .expect("no usable output sample rate");
            eprintln!("\n=== {} @ {rate} Hz, voice {voice} ===", spec.id);

            let text = "阿里云百炼语音合成，端到端链路验证：流式接收、MP3 解码与本地播放。";
            let slot = SessionSlot::default();
            let token = slot.claim();
            let (tx, rx) = mpsc::channel::<Vec<u8>>();
            let source = ChunkSource::new(rx, token.clone());

            let decode_token = token.clone();
            let decoder = thread::spawn(move || {
                let playback =
                    Playback::open(rate, 1.0, 0).expect("failed to open the output device");
                let handle = playback.handle();
                // Sampled while audio is playing, because this drives the HUD
                // waveform and a level that only appears after the last sample
                // would leave the bars hidden for the whole read.
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
                &api_key,
                spec,
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
                "{}: expected several seconds of speech, decoded {seconds:.2} s",
                spec.id
            );
            assert!(
                peak > 0.01,
                "{}: the HUD waveform is driven by this level, and it stayed at {peak:.4} \
                 while audio was playing",
                spec.id
            );
        }
    }

    /// Which voice ids the account actually accepts. Voice tables are hand-kept
    /// (there is no list endpoint), and a wrong id fails only at speak time with
    /// `Invalid voice specified` — so this checks them all in one pass:
    ///
    /// ```text
    /// ALIYUN_TTS_API_KEY=... cargo test --lib aliyun::tests::voice_table -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires network access and credentials"]
    fn voice_table_matches_what_the_service_accepts() {
        let api_key = std::env::var("ALIYUN_TTS_API_KEY").expect("ALIYUN_TTS_API_KEY is not set");
        let mut rejected: Vec<String> = Vec::new();

        for spec in SPECS.iter() {
            for (id, name, _) in spec.presets() {
                let slot = crate::tts::SessionSlot::default();
                let token = slot.claim();
                let (tx, rx) = mpsc::channel::<Vec<u8>>();
                // Drained on a thread so a slow reader cannot stall the sender.
                let drain = thread::spawn(move || rx.iter().count());

                let outcome = tauri::async_runtime::block_on(stream_audio(
                    &api_key,
                    spec,
                    &Synthesis {
                        text: "测试。",
                        voice: id,
                        sample_rate: spec.sample_rates[0],
                        rate: 1.0,
                    },
                    &tx,
                    &token,
                ));
                drop(tx);
                let chunks = drain.join().unwrap_or(0);

                match outcome {
                    Ok(_) if chunks > 0 => eprintln!("  ok      {} {id} ({name})", spec.id),
                    Ok(_) => {
                        eprintln!("  SILENT  {} {id} ({name})", spec.id);
                        rejected.push(format!("{} {id}: no audio", spec.id));
                    }
                    Err(err) => {
                        eprintln!("  REJECT  {} {id} ({name}): {err}", spec.id);
                        rejected.push(format!("{} {id}: {err}", spec.id));
                    }
                }
            }
        }

        assert!(rejected.is_empty(), "voice table is wrong:\n{rejected:#?}");
    }
}
